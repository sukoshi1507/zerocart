//! Application state: tabs, selection, running transfer. Keys in, commands out (to the worker), events in.
use crate::readers::{self, Detected};
use crate::worker::{Cmd, Event};
use zerocart_core::ReaderKind;
use zerocart_ui::screens::ReaderRow;
use zerocart_ui::{Action, CartView, Overlay, ProgressView, ReaderView, System, Tab, View};
use std::path::PathBuf;
use std::time::{Duration, Instant};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::Arc;
use crate::input::{Key, Routed};
use zerocart_ui::{loc, Lang};

/// A write action being set up: first a file is picked, then it is confirmed with Y.
enum Flow {
    None,
    Picking { action: Action, files: Vec<PathBuf>, selected: usize },
    Confirming { action: Action, file: PathBuf },
}

/// Where the user is: the start screens, then the tabs of the chosen system.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    Splash,
    /// The four icons: Game Boy, Game Boy Color, Game Boy Advance, Mega Drive
    System,
    /// The readers that can take that system, with a live "plugged in" check
    Readers,
    /// Cartridge / Actions / Tools
    Main,
    /// File sharing (ZeroSend): reached from the system choice with S
    Share,
}

const SPLASH_TIME: Duration = Duration::from_millis(1600);
const SCAN_EVERY: Duration = Duration::from_millis(600);

pub struct App {
    pub stage: Stage,
    pub system: System,
    system_sel: usize,
    reader_sel: usize,
    /// The user moved the highlight: the scan no longer moves it to the plugged-in reader.
    reader_moved: bool,
    detected: Vec<Detected>,
    connecting: bool,
    splash_since: Option<Instant>,
    last_scan: Option<Instant>,
    scanner: Box<dyn FnMut() -> Vec<Detected>>,
    /// The sharing screen, alive while it is open (the server and the discovery stop with it).
    share: Option<crate::share::Share>,
    share_starter: crate::share::Starter,
    pub lang: Lang,
    /// The language as the worker thread sees it (its messages are written in it).
    pub lang_cell: Option<Arc<std::sync::atomic::AtomicU8>>,
    pub tab: Tab,
    pub action_sel: usize,
    pub reader: Option<ReaderView>,
    pub cart: Option<CartView>,
    pub scanning: bool,
    pub progress: Option<ProgressView>,
    pub banner: Option<(String, bool)>,
    pub want_quit: bool,
    dirty: bool,
    tx: Sender<Cmd>,
    cancel: Arc<AtomicBool>,
    out_dir: PathBuf,
    flow: Flow,
    picker_items: Vec<String>,
    confirm_lines: Vec<String>,
    /// The cartridge being written is a GBA one (changes the wording of the confirmation).
    flow_gba: bool,
    /// The ROM being written is for a Mega Drive cartridge (changes the confirmation text).
    flow_md: bool,
    analysis: Option<Vec<String>>,
    analysis_scroll: usize,
    analysing: bool,
}

impl App {
    pub fn new(lang: Lang, tx: Sender<Cmd>, cancel: Arc<AtomicBool>, out_dir: PathBuf) -> Self {
        Self {
            stage: Stage::Splash, system: System::Gb, system_sel: 0, reader_sel: 0, reader_moved: false, detected: Vec::new(), connecting: false,
            splash_since: None, last_scan: None, scanner: Box::new(readers::scan),
            lang, lang_cell: None, tab: Tab::Info, action_sel: 0, reader: None, cart: None, scanning: false, progress: None,
            banner: None, want_quit: false, dirty: true, tx, cancel, out_dir, flow: Flow::None,
            picker_items: Vec::new(), confirm_lines: Vec::new(), flow_gba: false, flow_md: false,
            analysis: None, analysis_scroll: 0, analysing: false,
            share: None, share_starter: crate::share::default_starter(),
        }
    }

    /// Replaces the USB scan (tests).
    #[cfg(test)]
    pub fn with_scanner(mut self, f: impl FnMut() -> Vec<Detected> + 'static) -> Self {
        self.scanner = Box::new(f);
        self
    }

    /// Skips the start screens: the reader is connected and the cartridge of `system` read, as when the user has
    /// chosen them (used by the `--reader` / `--system` options).
    pub fn start_with(&mut self, kind: ReaderKind, system: System) {
        self.system = system;
        self.system_sel = System::ALL.iter().position(|&s| s == system).unwrap_or(0);
        self.stage = Stage::Readers;
        self.connect(kind);
    }

    /// Goes to the tabs of `system` and reads the cartridge (the reader is already connected).
    pub fn enter_main(&mut self, system: System) {
        self.system = system;
        self.stage = Stage::Main;
        self.tab = Tab::Info;
        self.detect(self.dmg());
    }

    /// Game Boy and Game Boy Color are read in Game Boy mode (5 V), the Advance in GBA mode (3.3 V).
    fn dmg(&self) -> bool {
        self.system != System::Gba
    }

    fn connect(&mut self, kind: ReaderKind) {
        self.connecting = true;
        self.banner = None;
        let _ = self.tx.send(Cmd::Connect(kind));
        self.dirty = true;
    }

    /// Called every loop turn: ends the splash, and refreshes the "plugged in" check while the readers are shown.
    pub fn tick(&mut self, now: Instant) {
        match self.stage {
            Stage::Splash => {
                let since = *self.splash_since.get_or_insert(now);
                if now.duration_since(since) >= SPLASH_TIME {
                    self.stage = Stage::System;
                    self.dirty = true;
                }
            }
            Stage::Share => {
                if self.share.as_mut().is_some_and(|s| s.poll()) {
                    self.dirty = true;
                }
            }
            Stage::Readers if !self.connecting => {
                if self.last_scan.is_none_or(|t| now.duration_since(t) >= SCAN_EVERY) {
                    self.last_scan = Some(now);
                    let found = (self.scanner)();
                    if found != self.detected {
                        self.detected = found;
                        if !self.reader_moved {
                            self.reader_sel = self.first_plugged();
                        }
                        self.dirty = true;
                    }
                }
            }
            _ => {}
        }
    }

    fn reader_kinds(&self) -> Vec<ReaderKind> {
        readers::compatible(self.system)
    }

    fn is_plugged(&self, kind: ReaderKind) -> bool {
        self.detected.iter().any(|d| d.kind == kind)
    }

    /// The first plugged-in reader of the list, the one to highlight.
    fn first_plugged(&self) -> usize {
        self.reader_kinds().iter().position(|&k| self.is_plugged(k)).unwrap_or(0)
    }

    fn rows(&self) -> Vec<ReaderRow> {
        self.reader_kinds()
            .into_iter()
            .map(|k| {
                let found = self.detected.iter().find(|d| d.kind == k);
                ReaderRow {
                    name: k.label().to_string(),
                    detail: found.map_or_else(|| readers::usb_hint(k).to_string(), |d| d.port.clone()),
                    present: found.is_some(),
                }
            })
            .collect()
    }

    /// Back one level: tabs -> readers -> systems -> quit.
    fn back(&mut self) {
        match self.stage {
            Stage::Main => {
                let _ = self.tx.send(Cmd::Disconnect);
                self.reader = None;
                self.cart = None;
                self.analysis = None;
                self.banner = None;
                self.stage = Stage::Readers;
                self.connecting = false;
                self.last_scan = None; // look again at once
                self.reader_moved = false;
                self.reader_sel = self.first_plugged();
            }
            Stage::Readers => {
                self.banner = None;
                self.stage = Stage::System;
            }
            Stage::System => self.want_quit = true,
            Stage::Splash => self.stage = Stage::System,
            Stage::Share => {
                self.share = None; // stops the server and the discovery
                self.stage = Stage::System;
            }
        }
    }

    /// Switches to the next language (French, English, Japanese, Simplified Chinese) and remembers the choice.
    fn next_language(&mut self) {
        self.lang = self.lang.next();
        if let Some(cell) = &self.lang_cell {
            cell.store(self.lang.index(), Ordering::SeqCst);
        }
        crate::settings::save_lang(self.lang);
        self.banner = Some((self.lang.native_name().to_string(), false));
    }

    /// Opens the sharing screen: starts ZeroSend (the certificate, the server on 53317, the announcements).
    fn open_share(&mut self) {
        match crate::share::Share::start(&self.share_starter, self.out_dir.clone(), self.lang) {
            Ok(s) => {
                self.share = Some(s);
                self.stage = Stage::Share;
                self.banner = None;
            }
            Err(e) => self.banner = Some((e, true)),
        }
    }

    /// Replaces the way ZeroSend is started (tests use a fake).
    #[cfg(test)]
    pub fn set_share_starter(&mut self, starter: crate::share::Starter) {
        self.share_starter = starter;
    }

    fn share_key(&mut self, key: Key) {
        let leave = self.share.as_mut().is_some_and(|s| s.on_key(key));
        if leave {
            self.back();
        }
    }

    fn start_key(&mut self, key: Key) {
        match (self.stage, key) {
            (Stage::Splash, _) => self.stage = Stage::System,
            (Stage::System, Key::Left) => self.system_sel = (self.system_sel + System::ALL.len() - 1) % System::ALL.len(),
            (Stage::System, Key::Right | Key::Tab) => self.system_sel = (self.system_sel + 1) % System::ALL.len(),
            (Stage::System, Key::Enter) => {
                self.system = System::ALL[self.system_sel];
                self.stage = Stage::Readers;
                self.detected.clear();
                self.last_scan = None;
                self.reader_sel = 0;
                self.reader_moved = false;
                self.banner = None;
            }
            (Stage::System, Key::Char('s' | 'S')) => self.open_share(),
            (Stage::System, Key::Char('l' | 'L')) => self.next_language(),
            (Stage::System, Key::Esc) | (Stage::Readers, Key::Esc) => self.back(),
            (Stage::Readers, Key::Up) | (Stage::Readers, Key::Down) if !self.connecting => {
                let n = self.reader_kinds().len().max(1);
                self.reader_moved = true;
                self.reader_sel = if key == Key::Up { (self.reader_sel + n - 1) % n } else { (self.reader_sel + 1) % n };
            }
            (Stage::Readers, Key::Enter) if !self.connecting => {
                let kinds = self.reader_kinds();
                let Some(&kind) = kinds.get(self.reader_sel) else { return };
                if self.is_plugged(kind) {
                    self.connect(kind);
                } else {
                    let name = kind.label();
                    self.banner = Some((
                        loc!(self.lang,
                            fr: format!("{name} : non branché"),
                            en: format!("{name}: not plugged in"),
                            ja: format!("{name}：接続されていません"),
                            zh: format!("{name}：未连接")),
                        true,
                    ));
                }
            }
            _ => self.dirty = false,
        }
    }

    /// A command is running in the worker: scripted keys wait.
    pub fn busy(&self) -> bool {
        self.scanning || self.connecting
    }


    fn detect(&mut self, dmg: bool) {
        self.scanning = true;
        self.cart = None;
        self.banner = None;
        let _ = self.tx.send(Cmd::Detect { dmg });
        self.dirty = true;
    }

    pub fn take_dirty(&mut self) -> bool {
        std::mem::take(&mut self.dirty)
    }

    pub fn on_routed(&mut self, r: Routed) {
        let Routed::Down(key) = r else { return };
        self.dirty = true;
        if self.stage == Stage::Share {
            self.share_key(key); // the sharing screen has a text field: it decides itself what F, Z, X and C mean
            return;
        }
        let key = arrow_alias(key);
        if self.stage != Stage::Main {
            self.start_key(key);
            return;
        }
        if !matches!(self.flow, Flow::None) {
            self.flow_key(key);
            return;
        }
        if self.progress.is_some() {
            if key == Key::Esc {
                self.cancel.store(true, Ordering::SeqCst);
            }
            return;
        }
        match key {
            Key::Esc => self.back(),
            Key::Right | Key::Tab => self.tab = self.tab.next(),
            Key::Left => self.tab = self.tab.prev(),
            Key::Char('r') | Key::Char('R') if !self.scanning => self.detect(self.dmg()),
            Key::Up if self.tab == Tab::Actions => self.action_sel = (self.action_sel + Action::ALL.len() - 1) % Action::ALL.len(),
            Key::Down if self.tab == Tab::Actions => self.action_sel = (self.action_sel + 1) % Action::ALL.len(),
            Key::Enter if self.tab == Tab::Actions => self.run_action(),
            Key::Enter if self.tab == Tab::Tools => self.start_analysis(),
            Key::Up if self.tab == Tab::Tools => self.analysis_scroll = self.analysis_scroll.saturating_sub(1),
            Key::Down if self.tab == Tab::Tools => {
                let max = self.analysis.as_ref().map_or(0, |a| a.len().saturating_sub(zerocart_ui::ANALYSIS_ROWS));
                self.analysis_scroll = (self.analysis_scroll + 1).min(max);
            }
            _ => self.dirty = false,
        }
    }

    fn start_analysis(&mut self) {
        if self.analysing {
            return;
        }
        if self.cart.is_none() {
            self.banner = Some((no_cartridge(self.lang).into(), true));
            return;
        }
        self.banner = None;
        self.analysing = true;
        self.analysis = None;
        self.analysis_scroll = 0;
        let _ = self.tx.send(Cmd::Analyze);
    }

    fn run_action(&mut self) {
        let action = Action::ALL[self.action_sel];
        let lang = self.lang;
        let Some(cart) = &self.cart else {
            self.banner = Some((no_cartridge(lang).into(), true));
            return;
        };
        self.banner = None;
        let md = self.system == System::MegaDrive;
        if md && action == Action::WriteSave {
            self.banner = Some((
                loc!(lang,
                    fr: "Sauvegarde Mega Drive : écriture pas encore gérée",
                    en: "Mega Drive save writing: not supported yet",
                    ja: "メガドライブのセーブ書き込みは未対応です",
                    zh: "暂不支持写入Mega Drive存档")
                .into(),
                true,
            ));
            return;
        }
        if action.writes() {
            let gba = cart.system == "GBA";
            let exts: &[&str] = match action {
                Action::FlashRom if md => &["bin", "md", "gen"],
                Action::FlashRom if gba => &["gba"],
                Action::FlashRom => &["gb", "gbc"],
                _ => &["sav"],
            };
            self.flow_gba = gba;
            self.flow_md = md;
            let files = list_files(&self.out_dir, exts);
            if files.is_empty() {
                let (ext, dir) = (exts[0], self.out_dir.display());
                self.banner = Some((
                    loc!(lang,
                        fr: format!("Aucun fichier .{ext} dans {dir}"),
                        en: format!("No .{ext} file in {dir}"),
                        ja: format!("{dir} に .{ext} ファイルがありません"),
                        zh: format!("{dir} 中没有 .{ext} 文件"))
                    ,
                    true,
                ));
                return;
            }
            self.picker_items = files.iter().map(|f| describe(f)).collect();
            self.flow = Flow::Picking { action, files, selected: 0 };
            return;
        }
        self.progress = Some(ProgressView { label: action.label(self.lang).into(), done: 0, total: 0, kib_s: 0.0 });
        let _ = self.tx.send(match action {
            Action::ReadRom => Cmd::ReadRom,
            _ => Cmd::ReadSave,
        });
    }

    fn flow_key(&mut self, key: Key) {
        let flow = std::mem::replace(&mut self.flow, Flow::None);
        self.flow = match (flow, key) {
            (Flow::Picking { .. }, Key::Esc) => Flow::None,
            (Flow::Picking { action, files, selected }, Key::Up) => {
                Flow::Picking { selected: (selected + files.len() - 1) % files.len(), action, files }
            }
            (Flow::Picking { action, files, selected }, Key::Down) => {
                Flow::Picking { selected: (selected + 1) % files.len(), action, files }
            }
            (Flow::Picking { action, files, selected }, Key::Enter) => {
                let file = files[selected].clone();
                self.confirm_lines =
                    if self.flow_md { confirm_text_md(&file, self.lang) } else { confirm_text(action, &file, self.flow_gba, self.lang) };
                Flow::Confirming { action, file }
            }
            (f @ Flow::Picking { .. }, _) => f,
            (Flow::Confirming { .. }, Key::Esc) => Flow::None,
            (Flow::Confirming { action, file }, Key::Char('y') | Key::Char('Y')) => {
                self.progress = Some(ProgressView { label: action.label(self.lang).into(), done: 0, total: 0, kib_s: 0.0 });
                let _ = self.tx.send(match action {
                    Action::FlashRom => Cmd::FlashRom(file),
                    _ => Cmd::WriteSave(file),
                });
                Flow::None
            }
            (f @ Flow::Confirming { .. }, _) => f,
            (Flow::None, _) => Flow::None,
        };
    }

    pub fn on_event(&mut self, e: Event) {
        self.dirty = true;
        match e {
            Event::Detected(Ok((reader, cart))) => {
                self.scanning = false;
                if self.banner.as_ref().is_some_and(|b| b.1) {
                    self.banner = None;
                }
                self.reader = Some(reader);
                if cart.is_none() {
                    // Both readers have one connector for every cartridge: a Game Boy cartridge read as GBA (or the
                    // reverse) has no valid header. Say what to try instead of waiting silently.
                    let lang = self.lang;
                    let text = if self.system == System::MegaDrive {
                        loc!(lang,
                            fr: "Pas de cartouche Mega Drive reconnue (en-tête SEGA absent)",
                            en: "No Mega Drive cartridge recognised (no SEGA header)",
                            ja: "メガドライブのカートを認識できません（SEGAヘッダーなし）",
                            zh: "未识别到Mega Drive卡带（没有SEGA文件头）")
                    } else if !self.dmg() {
                        loc!(lang,
                            fr: "Pas de cartouche GBA : est-ce une Game Boy ? (Échap : changer)",
                            en: "No GBA cartridge: is it a Game Boy one? (Esc: change)",
                            ja: "GBAカートがありません。Game Boyでは？（Esc：変更）",
                            zh: "没有GBA卡带：是Game Boy卡带吗？（Esc：更换）")
                    } else {
                        loc!(lang,
                            fr: "Pas de cartouche Game Boy : est-ce une GBA ? (Échap : changer)",
                            en: "No Game Boy cartridge: is it a GBA one? (Esc: change)",
                            ja: "Game Boyカートがありません。GBAでは？（Esc：変更）",
                            zh: "没有Game Boy卡带：是GBA卡带吗？（Esc：更换）")
                    };
                    self.banner = Some((text.into(), true));
                }
                self.cart = cart;
                self.analysis = None; // it described the previous cartridge or mode
            }
            Event::Connected(Ok(view)) => {
                self.connecting = false;
                self.reader = Some(view);
                self.enter_main(self.system);
            }
            Event::Connected(Err(msg)) => {
                self.connecting = false;
                self.banner = Some((msg, true));
            }
            Event::Analysis(r) => {
                self.analysing = false;
                match r {
                    Ok(lines) => self.analysis = Some(lines),
                    Err(m) => self.banner = Some((m, true)),
                }
            }
            Event::Detected(Err(msg)) => {
                self.scanning = false;
                self.reader = None;
                self.banner = Some((msg, true));
            }
            Event::Progress { label, done, total, kib_s } => {
                self.progress = Some(ProgressView { label, done, total, kib_s });
            }
            Event::Finished(r) => {
                self.progress = None;
                self.banner = Some(match r {
                    // "!..." : the work was done but could not be verified: shown in the attention colour
                    Ok(m) if m.starts_with('!') => (m.trim_start_matches('!').to_string(), true),
                    Ok(m) => (m, false),
                    Err(m) => (m, true),
                });
            }
        }
    }

    /// Draws the current screen.
    pub fn render(&self, p: &mut zerocart_ui::widgets::Painter) {
        use zerocart_ui::screens;
        let banner = self.banner.as_ref().map(|(m, e)| (m.as_str(), *e));
        match self.stage {
            Stage::Splash => screens::render_splash(p, self.lang, env!("CARGO_PKG_VERSION")),
            Stage::System => screens::render_system(p, self.lang, self.system_sel, banner),
            Stage::Readers => screens::render_readers(p, self.lang, self.system, &self.rows(), self.reader_sel, self.connecting, banner),
            Stage::Main => zerocart_ui::render(p, &self.view()),
            Stage::Share => {
                if let Some(s) = &self.share {
                    s.render(p);
                }
            }
        }
    }

    pub fn view(&self) -> View<'_> {
        let overlay = match &self.flow {
            Flow::None => None,
            Flow::Picking { action, selected, .. } => Some(Overlay::Picker {
                title: if *action == Action::FlashRom {
                    loc!(self.lang, fr: "ROM à écrire sur la cartouche :", en: "ROM to write to the cartridge:", ja: "カートに書き込むROM：", zh: "要写入卡带的ROM：")
                } else {
                    loc!(self.lang, fr: "Sauvegarde à écrire :", en: "Save to write:", ja: "書き込むセーブ：", zh: "要写入的存档：")
                },
                items: &self.picker_items,
                selected: *selected,
            }),
            Flow::Confirming { .. } => Some(Overlay::Confirm { lines: &self.confirm_lines }),
        };
        View {
            overlay,
            lang: self.lang,
            tab: self.tab,
            reader: self.reader.as_ref(),
            cart: self.cart.as_ref(),
            scanning: self.scanning,
            action_sel: self.action_sel,
            progress: self.progress.as_ref(),
            banner: self.banner.as_ref().map(|(m, e)| (m.as_str(), *e)),
            analysis: self.analysis.as_deref(),
            analysis_scroll: self.analysis_scroll,
            analysing: self.analysing,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc::{channel, Receiver};

    /// An application already in the tabs of the GBA system (one `Detect` waits in the channel).
    fn app() -> (App, Receiver<Cmd>) {
        let (tx, rx) = channel();
        let mut a = App::new(Lang::Fr, tx, Arc::new(AtomicBool::new(false)), std::env::temp_dir().join("zerocart-test-none"));
        a.enter_main(System::Gba);
        (a, rx)
    }

    /// An application at the very start, with a scripted USB scan.
    fn fresh(found: Vec<Detected>) -> (App, Receiver<Cmd>) {
        let (tx, rx) = channel();
        let a = App::new(Lang::Fr, tx, Arc::new(AtomicBool::new(false)), std::env::temp_dir().join("zerocart-test-none"))
            .with_scanner(move || found.clone());
        (a, rx)
    }

    fn plugged(kind: ReaderKind) -> Detected {
        Detected { kind, port: "/dev/ttyTEST".into() }
    }
    fn cart() -> CartView {
        CartView { system: "GBA", title: "T".into(), code: "AGB-TEST".into(), rom: "4 MiB".into(), save: "SRAM".into(), database: None, header_ok: true }
    }
    fn reader() -> ReaderView {
        ReaderView { name: "GBFlash".into(), firmware: "L15".into(), voltage: "3.3V" }
    }
    fn down(a: &mut App, k: Key) {
        a.on_routed(Routed::Down(k));
    }

    #[test]
    fn read_rom_runs_cancel_and_report() {
        let (mut a, rx) = app();
        rx.try_recv().unwrap();
        a.on_event(Event::Detected(Ok((reader(), Some(cart())))));
        down(&mut a, Key::Right);
        assert_eq!(a.tab, Tab::Actions);
        down(&mut a, Key::Enter);
        assert!(matches!(rx.try_recv().unwrap(), Cmd::ReadRom));
        assert!(a.progress.is_some());
        down(&mut a, Key::Esc);
        assert!(a.cancel.load(Ordering::SeqCst) && !a.want_quit);
        a.on_event(Event::Finished(Err("annulé".into())));
        assert!(a.progress.is_none());
        assert_eq!(a.banner, Some(("annulé".into(), true)));
    }

    #[test]
    fn an_unverified_result_is_shown_as_a_warning_not_as_a_success() {
        let (mut a, _rx) = app();
        a.on_event(Event::Finished(Ok("!Attention : sauvegarde NON vérifiée : x.sav".into())));
        assert_eq!(a.banner, Some(("Attention : sauvegarde NON vérifiée : x.sav".into(), true)));
        a.on_event(Event::Finished(Ok("ROM vérifiée".into())));
        assert_eq!(a.banner, Some(("ROM vérifiée".into(), false)));
    }

    #[test]
    fn the_tabs_read_the_cartridge_in_the_mode_the_system_implies() {
        let (_a, rx) = app(); // Game Boy Advance
        assert!(matches!(rx.try_recv().unwrap(), Cmd::Detect { dmg: false }), "GBA is read at 3.3 V");
        for system in [System::Gb, System::Gbc] {
            let (tx, rx) = channel();
            let mut a = App::new(Lang::Fr, tx, Arc::new(AtomicBool::new(false)), std::env::temp_dir());
            a.enter_main(system);
            assert!(matches!(rx.try_recv().unwrap(), Cmd::Detect { dmg: true }), "{system:?} is read at 5 V");
        }
    }

    #[test]
    fn there_is_no_key_to_switch_the_mode_any_more() {
        let (mut a, rx) = app();
        rx.try_recv().unwrap();
        a.on_event(Event::Detected(Ok((reader(), Some(cart())))));
        down(&mut a, Key::Char('g'));
        down(&mut a, Key::Char('G'));
        assert!(rx.try_recv().is_err(), "G does nothing: the system was chosen at the start");
        down(&mut a, Key::Char('r'));
        assert!(matches!(rx.try_recv().unwrap(), Cmd::Detect { dmg: false }), "R reads the cartridge again, same mode");
    }

    #[test]
    fn no_cartridge_says_what_to_check_instead_of_waiting_silently() {
        let (mut a, _rx) = app(); // GBA
        a.on_event(Event::Detected(Ok((reader(), None))));
        assert_eq!(a.banner, Some(("Pas de cartouche GBA : est-ce une Game Boy ? (Échap : changer)".into(), true)));
        let (tx, _rx2) = channel();
        let mut b = App::new(Lang::Fr, tx, Arc::new(AtomicBool::new(false)), std::env::temp_dir());
        b.enter_main(System::Gb);
        b.on_event(Event::Detected(Ok((reader(), None))));
        assert_eq!(b.banner, Some(("Pas de cartouche Game Boy : est-ce une GBA ? (Échap : changer)".into(), true)));
        // a cartridge found clears the message
        b.on_event(Event::Detected(Ok((reader(), Some(cart())))));
        assert!(b.cart.is_some() && b.banner.is_none());
    }

    #[test]
    fn the_splash_ends_by_itself_or_on_any_key() {
        let (mut a, _rx) = fresh(vec![]);
        assert_eq!(a.stage, Stage::Splash);
        let t0 = Instant::now();
        a.tick(t0);
        assert_eq!(a.stage, Stage::Splash);
        a.tick(t0 + Duration::from_millis(1000));
        assert_eq!(a.stage, Stage::Splash, "not yet");
        a.tick(t0 + SPLASH_TIME);
        assert_eq!(a.stage, Stage::System);
        let (mut b, _rx) = fresh(vec![]);
        down(&mut b, Key::Char('x'));
        assert_eq!(b.stage, Stage::System, "a key skips the splash");
    }

    #[test]
    fn the_four_icons_are_chosen_with_the_arrows_and_enter() {
        let (mut a, _rx) = fresh(vec![]);
        down(&mut a, Key::Enter); // skips the splash
        assert_eq!(a.stage, Stage::System);
        down(&mut a, Key::Right);
        down(&mut a, Key::Right);
        down(&mut a, Key::Enter);
        assert_eq!((a.stage, a.system), (Stage::Readers, System::Gba), "third icon");
        down(&mut a, Key::Esc);
        assert_eq!(a.stage, Stage::System);
        down(&mut a, Key::Left); // third icon -> second -> first -> wraps to the fourth
        down(&mut a, Key::Left);
        down(&mut a, Key::Left);
        down(&mut a, Key::Enter);
        assert_eq!(a.system, System::MegaDrive, "three steps left from the third icon wrap around to the fourth");
        down(&mut a, Key::Esc);
        down(&mut a, Key::Esc);
        assert!(a.want_quit, "Esc on the first screen quits");
    }

    #[test]
    fn the_readers_shown_depend_on_the_system_and_follow_the_usb_scan() {
        let (mut a, _rx) = fresh(vec![plugged(ReaderKind::Chisflash)]);
        down(&mut a, Key::Enter);
        down(&mut a, Key::Enter); // Game Boy
        assert!(a.rows().iter().all(|r| !r.present), "nothing is known before the first scan");
        a.tick(Instant::now());
        let rows = a.rows();
        assert_eq!(rows.iter().map(|r| r.name.as_str()).collect::<Vec<_>>(), ["GBxCart RW / GBFlash", "Chisflash"]);
        assert_eq!(rows.iter().map(|r| r.present).collect::<Vec<_>>(), [false, true]);
        assert_eq!(a.reader_sel, 1, "the highlight goes to the reader that is plugged in");
        assert_eq!(rows[1].detail, "/dev/ttyTEST");
        assert_eq!(rows[0].detail, "USB 1a86:7523", "a reader that is not plugged in shows where to plug it");
        // Mega Drive: only the Krikzz programmer
        down(&mut a, Key::Esc);
        down(&mut a, Key::Right);
        down(&mut a, Key::Right);
        down(&mut a, Key::Right);
        down(&mut a, Key::Enter);
        assert_eq!(a.rows().iter().map(|r| r.name.as_str()).collect::<Vec<_>>(), ["Krikzz Mega Drive"]);
    }

    #[test]
    fn the_scan_is_repeated_and_a_reader_plugged_in_later_appears() {
        use std::cell::RefCell;
        use std::rc::Rc;
        let state: Rc<RefCell<Vec<Detected>>> = Rc::default();
        let s2 = state.clone();
        let (tx, _rx) = channel();
        let mut a = App::new(Lang::Fr, tx, Arc::new(AtomicBool::new(false)), std::env::temp_dir()).with_scanner(move || s2.borrow().clone());
        down(&mut a, Key::Enter);
        down(&mut a, Key::Enter);
        let t0 = Instant::now();
        a.tick(t0);
        assert!(a.rows().iter().all(|r| !r.present));
        *state.borrow_mut() = vec![plugged(ReaderKind::GbxCart)];
        a.tick(t0 + Duration::from_millis(100));
        assert!(a.rows().iter().all(|r| !r.present), "not rescanned yet");
        a.tick(t0 + SCAN_EVERY);
        assert!(a.rows()[0].present);
        assert!(a.take_dirty(), "the screen is redrawn when the scan changes");
    }

    #[test]
    fn confirming_a_plugged_reader_connects_and_an_absent_one_is_refused() {
        let (mut a, rx) = fresh(vec![plugged(ReaderKind::Chisflash)]);
        down(&mut a, Key::Enter);
        down(&mut a, Key::Right);
        down(&mut a, Key::Right);
        down(&mut a, Key::Enter); // GBA
        a.tick(Instant::now());
        down(&mut a, Key::Up); // the user moves back to the first row, the GBFlash, which is not plugged in
        down(&mut a, Key::Enter);
        assert!(rx.try_recv().is_err(), "nothing is sent for a reader that is not there");
        assert_eq!(a.banner, Some(("GBxCart RW / GBFlash : non branché".into(), true)));
        down(&mut a, Key::Down);
        down(&mut a, Key::Enter);
        assert!(matches!(rx.try_recv().unwrap(), Cmd::Connect(ReaderKind::Chisflash)));
        assert!(a.busy());
        // the answer leads to the tabs, which start by reading the cartridge in the system's mode
        a.on_event(Event::Connected(Ok(ReaderView { name: "Chisflash".into(), firmware: String::new(), voltage: "" })));
        assert_eq!(a.stage, Stage::Main);
        assert!(matches!(rx.try_recv().unwrap(), Cmd::Detect { dmg: false }));
    }

    #[test]
    fn a_reader_that_does_not_answer_stays_on_the_reader_screen_with_the_reason() {
        let (mut a, rx) = fresh(vec![plugged(ReaderKind::GbxCart)]);
        down(&mut a, Key::Enter);
        down(&mut a, Key::Enter);
        a.tick(Instant::now());
        down(&mut a, Key::Enter);
        rx.try_recv().unwrap();
        a.on_event(Event::Connected(Err("le lecteur ne répond pas".into())));
        assert_eq!(a.stage, Stage::Readers);
        assert!(!a.busy());
        assert_eq!(a.banner, Some(("le lecteur ne répond pas".into(), true)));
    }

    #[test]
    fn escape_in_the_tabs_goes_back_to_the_readers_and_disconnects() {
        let (mut a, rx) = app();
        rx.try_recv().unwrap();
        a.on_event(Event::Detected(Ok((reader(), Some(cart())))));
        down(&mut a, Key::Esc);
        assert!(matches!(rx.try_recv().unwrap(), Cmd::Disconnect));
        assert_eq!(a.stage, Stage::Readers);
        assert!(a.cart.is_none() && a.reader.is_none() && !a.want_quit);
        down(&mut a, Key::Esc);
        assert_eq!(a.stage, Stage::System);
    }

    #[test]
    fn escape_still_cancels_a_transfer_and_does_not_leave_the_tabs() {
        let (mut a, rx) = app();
        rx.try_recv().unwrap();
        a.on_event(Event::Detected(Ok((reader(), Some(cart())))));
        down(&mut a, Key::Right);
        down(&mut a, Key::Enter);
        rx.try_recv().unwrap();
        down(&mut a, Key::Esc);
        assert!(a.cancel.load(Ordering::SeqCst));
        assert_eq!(a.stage, Stage::Main);
    }

    #[test]
    fn the_mega_drive_reads_its_cartridge_and_writes_a_rom_after_a_confirmation() {
        let (mut a, rx) = fresh(vec![plugged(ReaderKind::KrikzzMd)]);
        down(&mut a, Key::Enter);
        for _ in 0..3 {
            down(&mut a, Key::Right);
        }
        down(&mut a, Key::Enter);
        a.tick(Instant::now());
        down(&mut a, Key::Enter);
        assert!(matches!(rx.try_recv().unwrap(), Cmd::Connect(ReaderKind::KrikzzMd)));
        a.on_event(Event::Connected(Ok(ReaderView { name: "Krikzz FlashKit MD".into(), firmware: String::new(), voltage: "" })));
        assert_eq!((a.stage, a.system), (Stage::Main, System::MegaDrive));
        assert!(matches!(rx.try_recv().unwrap(), Cmd::Detect { .. }), "the header is read at once");
        a.on_event(Event::Detected(Ok((reader(), None))));
        assert_eq!(a.banner, Some(("Pas de cartouche Mega Drive reconnue (en-tête SEGA absent)".into(), true)));
        a.on_event(Event::Detected(Ok((reader(), Some(CartView { system: "Mega Drive", ..cart() })))));
        down(&mut a, Key::Right);
        down(&mut a, Key::Enter);
        assert!(matches!(rx.try_recv().unwrap(), Cmd::ReadRom));
        a.on_event(Event::Finished(Ok("ROM lue".into())));
        // saves cannot be written yet: refused before anything is asked
        a.action_sel = 2;
        down(&mut a, Key::Enter);
        assert!(rx.try_recv().is_err(), "nothing is sent");
        assert_eq!(a.banner, Some(("Sauvegarde Mega Drive : écriture pas encore gérée".into(), true)));
        // flashing a ROM asks for a file (.bin/.md/.gen), then a confirmation on Y, then sends the command
        let dir = std::env::temp_dir().join(format!("zerocart-md-app-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("jeu.bin"), vec![0u8; 0x600000]).unwrap(); // 6 MiB: the upper half is tested first
        std::fs::write(dir.join("jeu.gba"), b"x").unwrap();
        a.out_dir = dir.clone();
        a.action_sel = 3;
        down(&mut a, Key::Enter);
        assert!(rx.try_recv().is_err(), "choosing a file sends nothing");
        down(&mut a, Key::Enter); // the only .bin file
        assert!(a.confirm_lines.iter().any(|l| l.contains("Mega Drive")));
        assert!(a.confirm_lines.iter().any(|l| l.contains("4 Mo")), "{:?}", a.confirm_lines);
        down(&mut a, Key::Enter);
        assert!(rx.try_recv().is_err(), "Enter does not confirm a write");
        down(&mut a, Key::Char('y'));
        assert!(matches!(rx.try_recv().unwrap(), Cmd::FlashRom(p) if p.ends_with("jeu.bin")));
        let _ = std::fs::remove_dir_all(&dir);
        a.on_event(Event::Finished(Ok("ok".into())));
        down(&mut a, Key::Esc);
        down(&mut a, Key::Esc);
        assert!(matches!(rx.try_recv().unwrap(), Cmd::Disconnect));
        assert_eq!(a.stage, Stage::System);
    }

    /// A ZeroSend that does nothing: no network in the tests.
    struct Quiet;
    impl crate::share::Backend for Quiet {
        fn alias(&self) -> String {
            "ZeroCart".into()
        }
        fn port(&self) -> u16 {
            53317
        }
        fn discovering(&self) -> bool {
            true
        }
        fn peers(&self) -> Vec<zerocart_send::send::Peer> {
            vec![]
        }
        fn rescan(&self) {}
        fn add_manual(&self, _: &str) -> Result<zerocart_send::send::Peer, String> {
            Err("non".into())
        }
        fn send(
            &self,
            _: &zerocart_send::send::Peer,
            _: Vec<zerocart_send::send::Outgoing>,
            _: &mut dyn FnMut(zerocart_send::send::SendEvent),
            _: &zerocart_send::send::Cancel,
        ) -> Result<zerocart_send::send::Outcome, String> {
            Err("non".into())
        }
    }

    #[test]
    fn s_opens_the_sharing_screen_from_the_systems_and_esc_comes_back_and_stops_it() {
        let (mut a, _rx) = fresh(vec![]);
        a.set_share_starter(Box::new(|_, _| Ok(Arc::new(Quiet) as Arc<dyn crate::share::Backend>)));
        down(&mut a, Key::Esc); // the splash is skipped by a key
        assert_eq!(a.stage, Stage::System);
        down(&mut a, Key::Char('s'));
        assert_eq!(a.stage, Stage::Share);
        assert!(a.share.is_some());
        // inside, F is not an arrow: typing an address keeps the letters
        down(&mut a, Key::Char('a'));
        for c in "fzxc".chars() {
            down(&mut a, Key::Char(c));
        }
        down(&mut a, Key::Esc); // closes the text field
        assert_eq!(a.stage, Stage::Share, "Esc leaves the text field, not the screen");
        down(&mut a, Key::Esc);
        assert_eq!(a.stage, Stage::System);
        assert!(a.share.is_none(), "leaving stops ZeroSend");
        assert!(!a.want_quit);
    }

    #[test]
    fn a_zerosend_that_cannot_start_says_why_and_stays_on_the_systems() {
        let (mut a, _rx) = fresh(vec![]);
        a.set_share_starter(Box::new(|_, _| Err("port 53317 occupé".into())));
        down(&mut a, Key::Esc);
        down(&mut a, Key::Char('S'));
        assert_eq!(a.stage, Stage::System);
        assert_eq!(a.banner, Some(("port 53317 occupé".to_string(), true)));
    }

    #[test]
    fn f_z_x_c_are_up_left_down_right_without_the_fn_key() {
        assert_eq!(
            ['f', 'z', 'x', 'c', 'F', 'Z', 'X', 'C'].map(|c| arrow_alias(Key::Char(c))),
            [Key::Up, Key::Left, Key::Down, Key::Right, Key::Up, Key::Left, Key::Down, Key::Right]
        );
        for k in [Key::Up, Key::Enter, Key::Esc, Key::Char('r'), Key::Char('y'), Key::Char('g')] {
            assert_eq!(arrow_alias(k), k, "other keys are left alone");
        }
    }

    #[test]
    fn the_letters_drive_the_start_screens_and_the_tabs_like_the_arrows() {
        let (mut a, _rx) = fresh(vec![]);
        down(&mut a, Key::Char('x')); // any key ends the splash
        assert_eq!(a.stage, Stage::System);
        down(&mut a, Key::Char('c')); // right: Game Boy Color
        down(&mut a, Key::Char('c')); // right: Game Boy Advance
        down(&mut a, Key::Char('z')); // left: back to Game Boy Color
        down(&mut a, Key::Enter);
        assert_eq!((a.stage, a.system), (Stage::Readers, System::Gbc));
        let (mut b, rx) = app();
        rx.try_recv().unwrap();
        b.on_event(Event::Detected(Ok((reader(), Some(cart())))));
        down(&mut b, Key::Char('c')); // right: Actions tab
        assert_eq!(b.tab, Tab::Actions);
        down(&mut b, Key::Char('x')); // down: second action
        assert_eq!(b.action_sel, 1);
        down(&mut b, Key::Char('f')); // up
        assert_eq!(b.action_sel, 0);
        down(&mut b, Key::Char('z')); // left: Cartridge tab
        assert_eq!(b.tab, Tab::Info);
    }

    #[test]
    fn the_file_picker_is_driven_by_the_letters_too_but_confirmation_stays_on_y() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("a.sav"), vec![0u8; 16]).unwrap();
        std::fs::write(d.path().join("b.sav"), vec![0u8; 16]).unwrap();
        let (mut a, rx) = app_in(d.path());
        a.action_sel = 2; // write save
        down(&mut a, Key::Enter);
        down(&mut a, Key::Char('x')); // down to b.sav
        down(&mut a, Key::Enter);
        down(&mut a, Key::Char('c')); // right is not a confirmation
        assert!(rx.try_recv().is_err());
        down(&mut a, Key::Char('y'));
        match rx.try_recv().unwrap() {
            Cmd::WriteSave(p) => assert!(p.ends_with("b.sav")),
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn l_on_the_system_screen_cycles_the_language_and_tells_the_worker() {
        let (mut a, _rx) = fresh(vec![]);
        let cell = Arc::new(std::sync::atomic::AtomicU8::new(Lang::Fr.index()));
        a.lang_cell = Some(cell.clone());
        down(&mut a, Key::Enter); // splash -> systems
        assert_eq!(a.stage, Stage::System);
        for want in [Lang::En, Lang::Ja, Lang::Zh, Lang::Fr] {
            down(&mut a, Key::Char('l'));
            assert_eq!(a.lang, want);
            assert_eq!(Lang::from_index(cell.load(Ordering::SeqCst)), want);
            assert_eq!(a.banner, Some((want.native_name().to_string(), false)), "the banner names the new language");
        }
        // the screens follow: the Actions labels come from the language
        a.lang = Lang::Ja;
        assert_eq!(Action::ReadRom.label(a.lang), "ROMを読み出す");
    }

    #[test]
    fn start_with_skips_the_start_screens() {
        let (mut a, rx) = fresh(vec![]);
        a.start_with(ReaderKind::GbxCart, System::Gbc);
        assert!(matches!(rx.try_recv().unwrap(), Cmd::Connect(ReaderKind::GbxCart)));
        a.on_event(Event::Connected(Ok(reader())));
        assert_eq!((a.stage, a.system), (Stage::Main, System::Gbc));
        assert!(matches!(rx.try_recv().unwrap(), Cmd::Detect { dmg: true }));
    }

    fn app_in(dir: &std::path::Path) -> (App, Receiver<Cmd>) {
        let (tx, rx) = channel();
        let mut a = App::new(Lang::Fr, tx, Arc::new(AtomicBool::new(false)), dir.to_path_buf());
        a.enter_main(System::Gb);
        rx.try_recv().unwrap();
        a.on_event(Event::Detected(Ok((reader(), Some(CartView { system: "GB/GBC", ..cart() })))));
        a.tab = Tab::Actions;
        (a, rx)
    }

    #[test]
    fn writing_without_files_only_shows_a_banner() {
        let d = tempfile::tempdir().unwrap();
        let (mut a, rx) = app_in(d.path());
        a.action_sel = 3; // flash ROM
        down(&mut a, Key::Enter);
        assert!(rx.try_recv().is_err());
        assert!(a.banner.as_ref().unwrap().1);
    }

    #[test]
    fn flashing_needs_a_file_then_y_and_nothing_is_sent_before() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("game.gb"), vec![0u8; 0x8000]).unwrap();
        std::fs::write(d.path().join("notes.txt"), b"x").unwrap();
        let (mut a, rx) = app_in(d.path());
        a.action_sel = 3;
        down(&mut a, Key::Enter); // pick list
        assert_eq!(a.picker_items.len(), 1, "only the .gb file is listed");
        down(&mut a, Key::Enter); // choose -> confirmation
        assert!(rx.try_recv().is_err(), "choosing a file does not write");
        down(&mut a, Key::Enter); // Enter is not a confirmation
        assert!(rx.try_recv().is_err());
        down(&mut a, Key::Char('y'));
        match rx.try_recv().unwrap() {
            Cmd::FlashRom(p) => assert!(p.ends_with("game.gb")),
            other => panic!("unexpected {other:?}"),
        }
        assert!(a.progress.is_some());
    }

    #[test]
    fn escape_cancels_at_every_step_of_a_write() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("a.sav"), vec![0u8; 0x8000]).unwrap();
        let (mut a, rx) = app_in(d.path());
        a.action_sel = 2; // write save
        down(&mut a, Key::Enter);
        down(&mut a, Key::Esc);
        assert!(!a.want_quit && matches!(a.flow, Flow::None));
        down(&mut a, Key::Enter);
        down(&mut a, Key::Enter);
        down(&mut a, Key::Esc);
        assert!(rx.try_recv().is_err() && a.progress.is_none());
    }

    #[test]
    fn gba_flashing_lists_gba_roms_and_warns_that_the_whole_chip_is_erased() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("g.gba"), vec![0u8; 16]).unwrap();
        std::fs::write(d.path().join("old.gb"), vec![0u8; 16]).unwrap();
        let (mut a, rx) = app();
        rx.try_recv().unwrap();
        a.on_event(Event::Detected(Ok((reader(), Some(cart())))));
        a.out_dir = d.path().to_path_buf();
        a.tab = Tab::Actions;
        a.action_sel = 3;
        down(&mut a, Key::Enter);
        assert_eq!(a.picker_items.len(), 1, "only the .gba file is offered for a GBA cartridge");
        down(&mut a, Key::Enter);
        assert!(a.confirm_lines[0].contains("TOUTE la puce"));
        assert!(rx.try_recv().is_err(), "nothing is sent before the confirmation");
        down(&mut a, Key::Char('y'));
        assert!(matches!(rx.try_recv().unwrap(), Cmd::FlashRom(_)));
    }

    #[test]
    fn gba_save_writing_lists_sav_files() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("g.sav"), vec![0u8; 16]).unwrap();
        let (mut a, rx) = app();
        rx.try_recv().unwrap();
        a.on_event(Event::Detected(Ok((reader(), Some(cart())))));
        a.out_dir = d.path().to_path_buf();
        a.tab = Tab::Actions;
        a.action_sel = 2;
        down(&mut a, Key::Enter);
        assert_eq!(a.picker_items.len(), 1);
        down(&mut a, Key::Enter);
        down(&mut a, Key::Char('y'));
        assert!(matches!(rx.try_recv().unwrap(), Cmd::WriteSave(_)));
    }

    #[test]
    fn the_tools_tab_asks_for_an_analysis_and_scrolls_the_result() {
        let (mut a, rx) = app();
        rx.try_recv().unwrap();
        a.on_event(Event::Detected(Ok((reader(), Some(cart())))));
        a.tab = Tab::Tools;
        down(&mut a, Key::Enter);
        assert!(matches!(rx.try_recv().unwrap(), Cmd::Analyze));
        assert!(a.analysing);
        down(&mut a, Key::Enter);
        assert!(rx.try_recv().is_err(), "a second request while one runs is ignored");
        a.on_event(Event::Analysis(Ok((0..12).map(|i| format!("ligne {i}")).collect())));
        assert!(!a.analysing && a.analysis.as_ref().unwrap().len() == 12);
        for _ in 0..20 {
            down(&mut a, Key::Down);
        }
        assert_eq!(a.analysis_scroll, 12 - zerocart_ui::ANALYSIS_ROWS, "scrolling stops at the end");
        down(&mut a, Key::Up);
        assert_eq!(a.analysis_scroll, 12 - zerocart_ui::ANALYSIS_ROWS - 1);
        a.on_event(Event::Detected(Ok((reader(), Some(cart())))));
        assert!(a.analysis.is_none(), "a new detection drops the old analysis");
    }

    #[test]
    fn an_analysis_error_shows_a_banner_and_no_cartridge_means_no_analysis() {
        let (mut a, rx) = app();
        rx.try_recv().unwrap();
        a.tab = Tab::Tools;
        down(&mut a, Key::Enter);
        assert!(rx.try_recv().is_err() && a.banner.as_ref().unwrap().1);
        a.on_event(Event::Detected(Ok((reader(), Some(cart())))));
        down(&mut a, Key::Enter);
        rx.try_recv().unwrap();
        a.on_event(Event::Analysis(Err("lecteur déconnecté".into())));
        assert!(!a.analysing && a.analysis.is_none());
        assert_eq!(a.banner, Some(("lecteur déconnecté".into(), true)));
    }

    #[test]
    fn no_cartridge_no_transfer() {
        let (mut a, rx) = app();
        rx.try_recv().unwrap();
        a.on_event(Event::Detected(Ok((reader(), None))));
        a.tab = Tab::Actions;
        down(&mut a, Key::Enter);
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn escape_walks_back_up_the_screens_and_only_quits_from_the_first_one() {
        let (mut a, _rx) = app();
        a.on_event(Event::Detected(Err("lecteur non trouvé".into())));
        down(&mut a, Key::Esc);
        assert_eq!(a.stage, Stage::Readers);
        assert!(!a.want_quit);
        down(&mut a, Key::Esc);
        assert_eq!(a.stage, Stage::System);
        assert!(!a.want_quit);
        down(&mut a, Key::Esc);
        assert!(a.want_quit);
    }
}

/// The cardputer's arrows need the Fn key, which is awkward: F, Z, X and C (an inverted T: F above, Z X C below) are
/// taken as up, left, down and right everywhere. The application has no text entry, so no letter is lost.
pub fn arrow_alias(key: Key) -> Key {
    match key {
        Key::Char('f' | 'F') => Key::Up,
        Key::Char('z' | 'Z') => Key::Left,
        Key::Char('x' | 'X') => Key::Down,
        Key::Char('c' | 'C') => Key::Right,
        other => other,
    }
}

/// Files of `dir` (not recursive) with one of the extensions, sorted by name.
pub fn list_files(dir: &std::path::Path, exts: &[&str]) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = std::fs::read_dir(dir)
        .map(|rd| rd.flatten().map(|e| e.path()).collect())
        .unwrap_or_default();
    v.retain(|p| {
        p.is_file() && p.extension().and_then(|e| e.to_str()).is_some_and(|e| exts.iter().any(|x| x.eq_ignore_ascii_case(e)))
    });
    v.sort();
    v
}

fn no_cartridge(lang: Lang) -> &'static str {
    loc!(lang, fr: "Aucune cartouche", en: "No cartridge", ja: "カートがありません", zh: "没有卡带")
}

fn describe(p: &std::path::Path) -> String {
    let size = std::fs::metadata(p).map(|m| m.len()).unwrap_or(0);
    format!("{}  ({} KiB)", p.file_name().unwrap_or_default().to_string_lossy(), size / 1024)
}

fn confirm_text_md(file: &std::path::Path, lang: Lang) -> Vec<String> {
    let name = file.file_name().unwrap_or_default().to_string_lossy().into_owned();
    let size = std::fs::metadata(file).map(|m| m.len()).unwrap_or(0);
    let kib = size / 1024;
    let mut v = vec![
        loc!(lang,
            fr: "ATTENTION : la ROM de la cartouche Mega Drive va être effacée (seuls les secteurs qui diffèrent)".to_string(),
            en: "WARNING: the ROM of the Mega Drive cartridge will be erased (only the sectors that differ)".to_string(),
            ja: "警告：メガドライブカートのROMを消去します（異なるセクターのみ）".to_string(),
            zh: "警告：将擦除Mega Drive卡带的ROM（仅限不同的扇区）".to_string()),
        loc!(lang, fr: format!("Écrire : {name} ({kib} Ko)"), en: format!("Write: {name} ({kib} KiB)"), ja: format!("書き込み：{name}（{kib} KiB）"), zh: format!("写入：{name}（{kib} KiB）")),
        loc!(lang,
            fr: "L'ancienne ROM est d'abord sauvegardée dans backups. Ne pas débrancher pendant l'écriture.".to_string(),
            en: "The old ROM is saved to backups first. Do not unplug while writing.".to_string(),
            ja: "旧ROMはまずbackupsに保存されます。書き込み中は抜かないでください。".to_string(),
            zh: "旧ROM会先备份到backups。写入期间请勿拔出。".to_string()),
    ];
    if size > 4 << 20 {
        v.push(
            loc!(lang,
                fr: "Plus de 4 Mo : la moitié haute de la puce est testée avant d'écrire (refusée si inaccessible).",
                en: "Over 4 MiB: the upper half of the chip is tested before writing (refused if unreachable).",
                ja: "4 MiB超：書き込み前にチップ上半分をテストします（使えない場合は中止）。",
                zh: "超过4 MiB：写入前会测试芯片上半部分（无法访问则拒绝）。")
            .into(),
        );
    }
    v
}

fn confirm_text(action: Action, file: &std::path::Path, gba: bool, lang: Lang) -> Vec<String> {
    let name = file.file_name().unwrap_or_default().to_string_lossy().into_owned();
    let write = loc!(lang, fr: format!("Écrire : {name}"), en: format!("Write: {name}"), ja: format!("書き込み：{name}"), zh: format!("写入：{name}"));
    match action {
        Action::FlashRom if gba => vec![
            loc!(lang,
                fr: "ATTENTION : TOUTE la puce flash de la cartouche va être effacée".into(),
                en: "WARNING: the WHOLE flash chip of the cartridge will be erased".into(),
                ja: "警告：カートのフラッシュチップ全体を消去します".into(),
                zh: "警告：将擦除卡带的整个闪存芯片".into()),
            write,
            loc!(lang,
                fr: "Toute la puce est d'abord sauvegardée dans backups (plusieurs minutes). Ne pas débrancher pendant l'écriture.".into(),
                en: "The whole chip is saved to backups first (several minutes). Do not unplug while writing.".into(),
                ja: "チップ全体をまずbackupsに保存します（数分）。書き込み中は抜かないでください。".into(),
                zh: "整个芯片会先备份到backups（需数分钟）。写入期间请勿拔出。".into()),
        ],
        Action::FlashRom => vec![
            loc!(lang,
                fr: "ATTENTION : la ROM de la cartouche va être effacée".into(),
                en: "WARNING: the ROM of the cartridge will be erased".into(),
                ja: "警告：カートのROMを消去します".into(),
                zh: "警告：将擦除卡带的ROM".into()),
            write,
            loc!(lang,
                fr: "L'ancienne ROM est d'abord sauvegardée dans le dossier backups. Ne pas débrancher pendant l'écriture.".into(),
                en: "The old ROM is saved to the backups folder first. Do not unplug while writing.".into(),
                ja: "旧ROMはまずbackupsフォルダに保存されます。書き込み中は抜かないでください。".into(),
                zh: "旧ROM会先备份到backups文件夹。写入期间请勿拔出。".into()),
        ],
        _ => vec![
            loc!(lang,
                fr: "ATTENTION : la sauvegarde de la cartouche va être remplacée".into(),
                en: "WARNING: the cartridge's save will be replaced".into(),
                ja: "警告：カートのセーブを上書きします".into(),
                zh: "警告：将替换卡带的存档".into()),
            write,
            loc!(lang,
                fr: "L'ancienne sauvegarde est gardée dans backups si elle est lisible.".into(),
                en: "The old save is kept in backups if it can be read.".into(),
                ja: "旧セーブは読み取れればbackupsに保存されます。".into(),
                zh: "旧存档若可读取，会保留在backups中。".into()),
        ],
    }
}

