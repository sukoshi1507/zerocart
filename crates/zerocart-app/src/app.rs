//! Application state: tabs, selection, running transfer. Keys in, commands out (to the worker), events in.
use crate::worker::{Cmd, Event};
use zerocart_ui::{Action, CartView, Overlay, ProgressView, ReaderView, Tab, View};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::Arc;
use crate::input::{Key, Routed};
use zerocart_ui::Lang;

/// A write action being set up: first a file is picked, then it is confirmed with Y.
enum Flow {
    None,
    Picking { action: Action, files: Vec<PathBuf>, selected: usize },
    Confirming { action: Action, file: PathBuf },
}

pub struct App {
    pub lang: Lang,
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
    analysis: Option<Vec<String>>,
    analysis_scroll: usize,
    analysing: bool,
    /// The mode of the last detection (Game Boy or GBA), to say what to try when no cartridge is found.
    last_dmg: bool,
}

impl App {
    pub fn new(lang: Lang, tx: Sender<Cmd>, cancel: Arc<AtomicBool>, out_dir: PathBuf) -> Self {
        let mut a = Self {
            lang, tab: Tab::Info, action_sel: 0, reader: None, cart: None, scanning: false, progress: None,
            banner: None, want_quit: false, dirty: true, tx, cancel, out_dir, flow: Flow::None,
            picker_items: Vec::new(), confirm_lines: Vec::new(), flow_gba: false,
            analysis: None, analysis_scroll: 0, analysing: false, last_dmg: false,
        };
        a.detect(false);
        a
    }

    fn fr(&self) -> bool {
        self.lang == Lang::Fr
    }

    fn detect(&mut self, dmg: bool) {
        self.last_dmg = dmg;
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
            Key::Esc => self.want_quit = true,
            Key::Right | Key::Tab => self.tab = self.tab.next(),
            Key::Left => self.tab = self.tab.prev(),
            Key::Char('r') | Key::Char('R') if !self.scanning => self.detect(false),
            Key::Char('g') | Key::Char('G') if !self.scanning => {
                let no_voltage = self.reader.as_ref().is_some_and(|r| r.voltage.is_empty());
                self.detect(true);
                self.banner = Some((
                    match (no_voltage, self.fr()) {
                        (true, true) => "Lecture en mode Game Boy",
                        (true, false) => "Reading in Game Boy mode",
                        (false, true) => "Mode Game Boy : 5 V sur la cartouche",
                        (false, false) => "Game Boy mode: 5 V on the cartridge",
                    }
                    .into(),
                    false,
                ));
            }
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
            self.banner = Some(((if self.fr() { "Aucune cartouche" } else { "No cartridge" }).into(), true));
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
        let fr = self.fr();
        let Some(cart) = &self.cart else {
            self.banner = Some(((if fr { "Aucune cartouche" } else { "No cartridge" }).into(), true));
            return;
        };
        self.banner = None;
        if action.writes() {
            let gba = cart.system == "GBA";
            let (exts, what): (&[&str], &str) = match action {
                Action::FlashRom if gba => (&["gba"], "ROM"),
                Action::FlashRom => (&["gb", "gbc"], "ROM"),
                _ => (&["sav"], "sauvegarde"),
            };
            self.flow_gba = gba;
            let files = list_files(&self.out_dir, exts);
            if files.is_empty() {
                self.banner = Some((
                    format!("Aucun fichier .{} dans {}", exts[0], self.out_dir.display()),
                    true,
                ));
                let _ = what;
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
                self.confirm_lines = confirm_text(action, &file, self.flow_gba);
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
                    let fr = self.fr();
                    self.banner = Some((
                        match (self.last_dmg, fr) {
                            (false, true) => "Pas de cartouche GBA : une Game Boy ? (G)",
                            (false, false) => "No GBA cartridge: a Game Boy one? (G)",
                            (true, true) => "Pas de cartouche Game Boy : une GBA ? (R)",
                            (true, false) => "No Game Boy cartridge: a GBA one? (R)",
                        }
                        .into(),
                        true,
                    ));
                }
                self.cart = cart;
                self.analysis = None; // it described the previous cartridge or mode
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

    pub fn view(&self) -> View<'_> {
        let overlay = match &self.flow {
            Flow::None => None,
            Flow::Picking { action, selected, .. } => Some(Overlay::Picker {
                title: if *action == Action::FlashRom { "ROM à écrire sur la cartouche :" } else { "Sauvegarde à écrire :" },
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

    fn app() -> (App, Receiver<Cmd>) {
        let (tx, rx) = channel();
        (App::new(Lang::Fr, tx, Arc::new(AtomicBool::new(false)), std::env::temp_dir().join("zerocart-test-none")), rx)
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
    fn starts_by_scanning_in_the_safe_gba_mode() {
        let (a, rx) = app();
        assert!(a.scanning);
        assert!(matches!(rx.try_recv().unwrap(), Cmd::Detect { dmg: false }));
    }

    #[test]
    fn game_boy_5v_mode_only_on_an_explicit_key() {
        let (mut a, rx) = app();
        rx.try_recv().unwrap();
        a.on_event(Event::Detected(Ok((reader(), Some(cart())))));
        assert!(rx.try_recv().is_err());
        down(&mut a, Key::Char('g'));
        assert!(matches!(rx.try_recv().unwrap(), Cmd::Detect { dmg: true }));
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
    fn no_cartridge_says_which_mode_to_try_instead_of_waiting_silently() {
        let (mut a, _rx) = app();
        // the application starts in GBA mode
        a.on_event(Event::Detected(Ok((reader(), None))));
        assert_eq!(a.banner, Some(("Pas de cartouche GBA : une Game Boy ? (G)".into(), true)));
        down(&mut a, Key::Char('g'));
        a.on_event(Event::Detected(Ok((reader(), None))));
        assert_eq!(a.banner, Some(("Pas de cartouche Game Boy : une GBA ? (R)".into(), true)));
        // a cartridge found clears the message
        a.on_event(Event::Detected(Ok((reader(), Some(cart())))));
        assert!(a.cart.is_some());
        assert!(a.banner.is_none());
    }

    #[test]
    fn the_game_boy_mode_banner_has_no_voltage_for_a_reader_that_does_not_set_one() {
        let (mut a, _rx) = app();
        let chis = ReaderView { name: "Chisflash".into(), firmware: String::new(), voltage: "" };
        a.on_event(Event::Detected(Ok((chis, Some(cart())))));
        down(&mut a, Key::Char('g'));
        assert_eq!(a.banner, Some(("Lecture en mode Game Boy".into(), false)));
        let (mut b, _rx) = app();
        b.on_event(Event::Detected(Ok((reader(), Some(cart())))));
        down(&mut b, Key::Char('g'));
        assert_eq!(b.banner, Some(("Mode Game Boy : 5 V sur la cartouche".into(), false)));
    }

    fn app_in(dir: &std::path::Path) -> (App, Receiver<Cmd>) {
        let (tx, rx) = channel();
        let mut a = App::new(Lang::Fr, tx, Arc::new(AtomicBool::new(false)), dir.to_path_buf());
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
    fn escape_quits_when_idle() {
        let (mut a, _rx) = app();
        a.on_event(Event::Detected(Err("lecteur non trouvé".into())));
        down(&mut a, Key::Esc);
        assert!(a.want_quit);
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

fn describe(p: &std::path::Path) -> String {
    let size = std::fs::metadata(p).map(|m| m.len()).unwrap_or(0);
    format!("{}  ({} Ko)", p.file_name().unwrap_or_default().to_string_lossy(), size / 1024)
}

fn confirm_text(action: Action, file: &std::path::Path, gba: bool) -> Vec<String> {
    let name = file.file_name().unwrap_or_default().to_string_lossy().into_owned();
    match action {
        Action::FlashRom if gba => vec![
            "ATTENTION : TOUTE la puce flash de la cartouche va être effacée".into(),
            format!("Écrire : {name}"),
            "Toute la puce est d'abord sauvegardée dans backups (plusieurs minutes). Ne pas débrancher pendant l'écriture.".into(),
        ],
        Action::FlashRom => vec![
            "ATTENTION : la ROM de la cartouche va être effacée".into(),
            format!("Écrire : {name}"),
            "L'ancienne ROM est d'abord sauvegardée dans le dossier backups. Ne pas débrancher pendant l'écriture.".into(),
        ],
        _ => vec![
            "ATTENTION : la sauvegarde de la cartouche va être remplacée".into(),
            format!("Écrire : {name}"),
            "L'ancienne sauvegarde est gardée dans backups si elle est lisible.".into(),
        ],
    }
}
