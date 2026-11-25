// SPDX-License-Identifier: GPL-3.0-or-later
//! The file sharing screen of the application (ZeroSend): the devices around, sending a dump or a save to one of them,
//! and accepting what another device sends here. The network side is `zerocart-send`; this is the state of the screen.
//!
//! Nothing leaves the device without a confirmation on Y, and nothing is received without the user accepting on Y.
//! The texts are in French and English, grouped in [`txt`] and the `format!` calls below, for the translation table.
use crate::app::arrow_alias;
use crate::input::Key;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use zerocart_send::protocol::{DeviceInfo, FileMeta};
use zerocart_send::receive::Ui;
use zerocart_send::send::{Cancel, Outcome, Outgoing, Peer, SendEvent};
use zerocart_send::service::{Config, Service};
use zerocart_ui::share_screen::{render_share, ShareBody, ShareRow};
use zerocart_ui::widgets::Painter;
use zerocart_ui::{Lang, ProgressView};

/// How long a request from another device waits for the user (then it is declined).
const ASK_TIMEOUT: Duration = Duration::from_secs(300);
/// Extensions of what can be sent: dumps, saves.
const SENDABLE: &[&str] = &["bin", "md", "gen", "gba", "gb", "gbc", "sav", "srm"];

fn txt(lang: Lang, fr: &'static str, en: &'static str) -> &'static str {
    if lang == Lang::Fr { fr } else { en }
}

/// What the screen needs from ZeroSend: the real one is a [`Service`], tests use a fake.
pub trait Backend: Send + Sync {
    fn alias(&self) -> String;
    fn port(&self) -> u16;
    fn discovering(&self) -> bool;
    fn peers(&self) -> Vec<Peer>;
    fn rescan(&self);
    fn add_manual(&self, text: &str) -> Result<Peer, String>;
    fn send(&self, peer: &Peer, files: Vec<Outgoing>, progress: &mut dyn FnMut(SendEvent), cancel: &Cancel) -> Result<Outcome, String>;
}

impl Backend for Service {
    fn alias(&self) -> String {
        Service::alias(self).to_string()
    }
    fn port(&self) -> u16 {
        Service::port(self)
    }
    fn discovering(&self) -> bool {
        Service::discovering(self)
    }
    fn peers(&self) -> Vec<Peer> {
        Service::peers(self)
    }
    fn rescan(&self) {
        Service::rescan(self)
    }
    fn add_manual(&self, text: &str) -> Result<Peer, String> {
        Service::add_manual(self, text)
    }
    fn send(&self, peer: &Peer, files: Vec<Outgoing>, progress: &mut dyn FnMut(SendEvent), cancel: &Cancel) -> Result<Outcome, String> {
        Service::send(self, peer, files, progress, cancel)
    }
}

/// Starts ZeroSend: given the user interface of the received files and a "the list of devices changed" callback.
pub type Starter = Box<dyn Fn(Arc<dyn Ui>, Arc<dyn Fn() + Send + Sync>) -> Result<Arc<dyn Backend>, String> + Send>;

/// The real ZeroSend: certificate in `~/.config/zerocart/send`, received files in `~/Downloads`.
pub fn default_starter() -> Starter {
    Box::new(|ui, on_change| {
        let home = PathBuf::from(std::env::var_os("HOME").unwrap_or_else(|| "/tmp".into()));
        let config = Config::new("ZeroCart", home.join(".config/zerocart/send"), home.join("Downloads"));
        let port = config.bind.port();
        Service::start(config, ui, on_change)
            .map(|s| Arc::new(s) as Arc<dyn Backend>)
            .map_err(|e| format!("démarrage impossible (port {port} occupé ? ZeroSend ou LocalSend ouvert ?) : {e}"))
    })
}

/// What the network threads tell the screen.
pub enum ShareEvent {
    Changed,
    Incoming { from: String, files: Vec<(String, u64)>, answer: Sender<bool> },
    Message { from: String, text: String },
    Receiving { file: String, done: u64, total: u64 },
    Saved(PathBuf),
    Sending(SendEvent),
    SendDone(Result<Outcome, String>),
    Added(Result<Peer, String>),
}

/// The user interface of the receiving side: asks the screen and waits for the answer.
struct ShareUi {
    tx: Mutex<Sender<ShareEvent>>,
}

impl ShareUi {
    fn send(&self, e: ShareEvent) {
        let _ = self.tx.lock().unwrap().send(e);
    }
}

impl Ui for ShareUi {
    fn ask(&self, from: &DeviceInfo, _ip: std::net::IpAddr, files: &[FileMeta]) -> bool {
        let (answer, wait) = channel();
        self.send(ShareEvent::Incoming { from: from.alias.clone(), files: files.iter().map(|f| (f.file_name.clone(), f.size)).collect(), answer });
        wait.recv_timeout(ASK_TIMEOUT).unwrap_or(false)
    }
    fn message(&self, from: &DeviceInfo, text: &str) {
        self.send(ShareEvent::Message { from: from.alias.clone(), text: text.to_string() });
    }
    fn peer_seen(&self, _: &DeviceInfo, _: std::net::IpAddr) {
        self.send(ShareEvent::Changed);
    }
    fn progress(&self, file: &str, done: u64, total: u64) {
        self.send(ShareEvent::Receiving { file: file.to_string(), done, total });
    }
    fn saved(&self, path: &Path) {
        self.send(ShareEvent::Saved(path.to_path_buf()));
    }
}

enum Screen {
    Devices,
    Files { peer: Peer, files: Vec<PathBuf>, items: Vec<String>, selected: usize },
    Confirm { peer: Peer, file: PathBuf, lines: Vec<String> },
    Input { text: String, busy: bool },
    Sending { to: String, progress: ProgressView, waiting: bool },
}

struct Incoming {
    lines: Vec<String>,
    answer: Sender<bool>,
}

pub struct Share {
    backend: Arc<dyn Backend>,
    tx: Sender<ShareEvent>,
    rx: Receiver<ShareEvent>,
    screen: Screen,
    selected: usize,
    cancel: Option<Arc<Cancel>>,
    incoming: Option<Incoming>,
    receiving: Option<ProgressView>,
    pub banner: Option<(String, bool)>,
    out_dir: PathBuf,
    lang: Lang,
}

fn size_text(bytes: u64) -> String {
    if bytes >= 1 << 20 { format!("{:.1} Mo", bytes as f64 / (1 << 20) as f64) } else { format!("{} Ko", bytes.div_ceil(1024)) }
}

/// What can be sent: the dumps and saves of `dir` and of `dir/backups`.
fn sendable_files(dir: &Path) -> Vec<PathBuf> {
    let mut v = Vec::new();
    for d in [dir.to_path_buf(), dir.join("backups")] {
        if let Ok(rd) = std::fs::read_dir(&d) {
            let mut here: Vec<PathBuf> = rd
                .flatten()
                .map(|e| e.path())
                .filter(|p| p.is_file() && p.extension().and_then(|e| e.to_str()).is_some_and(|e| SENDABLE.iter().any(|x| x.eq_ignore_ascii_case(e))))
                .collect();
            here.sort();
            v.extend(here);
        }
    }
    v
}

fn file_label(dir: &Path, p: &Path) -> String {
    let rel = p.strip_prefix(dir).unwrap_or(p).to_string_lossy().into_owned();
    format!("{rel}  ({})", size_text(std::fs::metadata(p).map(|m| m.len()).unwrap_or(0)))
}

impl Share {
    pub fn start(starter: &Starter, out_dir: PathBuf, lang: Lang) -> Result<Share, String> {
        let (tx, rx) = channel();
        let ui: Arc<dyn Ui> = Arc::new(ShareUi { tx: Mutex::new(tx.clone()) });
        let changed = Mutex::new(tx.clone());
        let on_change: Arc<dyn Fn() + Send + Sync> = Arc::new(move || {
            let _ = changed.lock().unwrap().send(ShareEvent::Changed);
        });
        let backend = starter(ui, on_change)?;
        Ok(Share { backend, tx, rx, screen: Screen::Devices, selected: 0, cancel: None, incoming: None, receiving: None, banner: None, out_dir, lang })
    }

    /// Takes what the network threads reported. Returns true when the screen has to be redrawn.
    pub fn poll(&mut self) -> bool {
        let mut dirty = false;
        while let Ok(e) = self.rx.try_recv() {
            dirty = true;
            self.on_event(e);
        }
        dirty
    }

    fn peers(&self) -> Vec<Peer> {
        self.backend.peers()
    }

    fn on_event(&mut self, e: ShareEvent) {
        let l = self.lang;
        match e {
            ShareEvent::Changed => {
                let n = self.peers().len();
                self.selected = self.selected.min(n.saturating_sub(1));
            }
            ShareEvent::Incoming { from, files, answer } => {
                if self.incoming.is_some() {
                    let _ = answer.send(false); // one request at a time
                    return;
                }
                let mut lines = vec![if l == Lang::Fr { format!("{from} veut vous envoyer :") } else { format!("{from} wants to send you:") }];
                for (name, size) in files.iter().take(3) {
                    lines.push(format!("{name} ({})", size_text(*size)));
                }
                if files.len() > 3 {
                    lines.push(if l == Lang::Fr { format!("... et {} autres", files.len() - 3) } else { format!("... and {} more", files.len() - 3) });
                }
                lines.push(txt(l, "Enregistré dans ~/Downloads.", "Saved in ~/Downloads.").to_string());
                self.incoming = Some(Incoming { lines, answer });
            }
            ShareEvent::Message { from, text } => {
                self.banner = Some((if l == Lang::Fr { format!("Message de {from} : {text}") } else { format!("Message from {from}: {text}") }, false));
            }
            ShareEvent::Receiving { file, done, total } => {
                self.receiving = Some(ProgressView { label: if l == Lang::Fr { format!("Réception : {file}") } else { format!("Receiving: {file}") }, done, total, kib_s: 0.0 });
            }
            ShareEvent::Saved(path) => {
                self.receiving = None;
                let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                self.banner = Some((if l == Lang::Fr { format!("Reçu : {name}") } else { format!("Received: {name}") }, false));
            }
            ShareEvent::Sending(SendEvent::Waiting) => {
                if let Screen::Sending { waiting, .. } = &mut self.screen {
                    *waiting = true;
                }
            }
            ShareEvent::Sending(SendEvent::Uploading { file, index, of, done, total }) => {
                if let Screen::Sending { progress, waiting, .. } = &mut self.screen {
                    *waiting = false;
                    progress.label = if of > 1 { format!("{file} ({index}/{of})") } else { file };
                    progress.done = done;
                    progress.total = total;
                }
            }
            ShareEvent::SendDone(result) => {
                let to = match &self.screen {
                    Screen::Sending { to, .. } => to.clone(),
                    _ => String::new(),
                };
                self.cancel = None;
                self.screen = Screen::Devices;
                self.banner = Some(match result {
                    Ok(Outcome::Sent { .. }) | Ok(Outcome::MessageDelivered) => (if l == Lang::Fr { format!("Envoyé à {to}") } else { format!("Sent to {to}") }, false),
                    Ok(Outcome::Declined) => (if l == Lang::Fr { format!("{to} a refusé") } else { format!("{to} declined") }, true),
                    Ok(Outcome::Busy) => (if l == Lang::Fr { format!("{to} est occupé avec un autre transfert") } else { format!("{to} is busy with another transfer") }, true),
                    Ok(Outcome::PinRequired) => (if l == Lang::Fr { format!("{to} demande un code PIN (non géré)") } else { format!("{to} asks for a PIN (not supported)") }, true),
                    Err(e) => (e, true),
                });
            }
            ShareEvent::Added(result) => {
                self.screen = Screen::Devices;
                self.banner = Some(match result {
                    Ok(p) => (if l == Lang::Fr { format!("Ajouté : {}", p.info.alias) } else { format!("Added: {}", p.info.alias) }, false),
                    Err(e) => (e, true),
                });
            }
        }
    }

    /// A key. Returns true when the user leaves the sharing screen.
    pub fn on_key(&mut self, raw: Key) -> bool {
        let l = self.lang;
        // a request from another device comes first: Y accepts, N or Esc declines
        if let Some(inc) = self.incoming.take() {
            match raw {
                Key::Char('y' | 'Y') => {
                    let _ = inc.answer.send(true);
                }
                Key::Char('n' | 'N') | Key::Esc => {
                    let _ = inc.answer.send(false);
                }
                _ => self.incoming = Some(inc),
            }
            return false;
        }
        // text entry: letters are letters (F Z X C are not arrows here)
        if let Screen::Input { text, busy } = &mut self.screen {
            if *busy {
                return false;
            }
            match raw {
                Key::Esc => self.screen = Screen::Devices,
                Key::Backspace => {
                    text.pop();
                }
                Key::Char(c) if (c.is_ascii_alphanumeric() || ".:-_".contains(c)) && text.len() < 60 => text.push(c),
                Key::Enter if !text.trim().is_empty() => {
                    *busy = true;
                    let (backend, tx, address) = (self.backend.clone(), self.tx.clone(), text.trim().to_string());
                    std::thread::spawn(move || {
                        let _ = tx.send(ShareEvent::Added(backend.add_manual(&address)));
                    });
                }
                _ => {}
            }
            return false;
        }
        let key = arrow_alias(raw);
        self.banner = None;
        match &mut self.screen {
            Screen::Devices => {
                let n = self.backend.peers().len();
                match key {
                    Key::Esc => return true,
                    Key::Up if n > 0 => self.selected = (self.selected + n - 1) % n,
                    Key::Down if n > 0 => self.selected = (self.selected + 1) % n,
                    Key::Char('r' | 'R') => self.backend.rescan(),
                    Key::Char('a' | 'A') => self.screen = Screen::Input { text: String::new(), busy: false },
                    Key::Enter => {
                        if let Some(peer) = self.backend.peers().get(self.selected).cloned() {
                            let files = sendable_files(&self.out_dir);
                            let items = files.iter().map(|f| file_label(&self.out_dir, f)).collect();
                            self.screen = Screen::Files { peer, files, items, selected: 0 };
                        }
                    }
                    _ => {}
                }
            }
            Screen::Files { peer, files, selected, .. } => match key {
                Key::Esc => self.screen = Screen::Devices,
                Key::Up if !files.is_empty() => *selected = (*selected + files.len() - 1) % files.len(),
                Key::Down if !files.is_empty() => *selected = (*selected + 1) % files.len(),
                Key::Enter if !files.is_empty() => {
                    let file = files[*selected].clone();
                    let size = std::fs::metadata(&file).map(|m| m.len()).unwrap_or(0);
                    let name = file.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                    let lines = vec![
                        if l == Lang::Fr { format!("Envoyer ce fichier à {} ?", peer.info.alias) } else { format!("Send this file to {}?", peer.info.alias) },
                        format!("{name} ({})", size_text(size)),
                        txt(l, "Le fichier quitte l'appareil et passe par le réseau local.", "The file leaves the device over the local network.").to_string(),
                    ];
                    let peer = peer.clone();
                    self.screen = Screen::Confirm { peer, file, lines };
                }
                _ => {}
            },
            Screen::Confirm { peer, file, .. } => match key {
                Key::Esc => {
                    let (peer, dir) = (peer.clone(), self.out_dir.clone());
                    let files = sendable_files(&dir);
                    let items = files.iter().map(|f| file_label(&dir, f)).collect();
                    self.screen = Screen::Files { peer, files, items, selected: 0 };
                }
                Key::Char('y' | 'Y') => {
                    let (peer, file) = (peer.clone(), file.clone());
                    self.start_send(peer, file);
                }
                _ => {}
            },
            Screen::Sending { .. } => {
                if key == Key::Esc {
                    if let Some(c) = &self.cancel {
                        c.cancel();
                    }
                }
            }
            Screen::Input { .. } => {}
        }
        false
    }

    fn start_send(&mut self, peer: Peer, file: PathBuf) {
        let cancel = Arc::new(Cancel::new());
        self.cancel = Some(cancel.clone());
        let name = file.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let size = std::fs::metadata(&file).map(|m| m.len()).unwrap_or(0);
        self.screen = Screen::Sending { to: peer.info.alias.clone(), progress: ProgressView { label: name, done: 0, total: size, kib_s: 0.0 }, waiting: true };
        let (backend, tx) = (self.backend.clone(), self.tx.clone());
        std::thread::spawn(move || {
            let progress_tx = tx.clone();
            let result = Outgoing::from_path(&file).map_err(|e| e.to_string()).and_then(|f| {
                backend.send(&peer, vec![f], &mut |e| { let _ = progress_tx.send(ShareEvent::Sending(e)); }, &cancel)
            });
            let _ = tx.send(ShareEvent::SendDone(result));
        });
    }

    /// Whether a transfer is running (the screen cannot be left then).
    #[cfg(test)]
    pub fn sending(&self) -> bool {
        matches!(self.screen, Screen::Sending { .. })
    }

    pub fn render(&self, p: &mut Painter) {
        let l = self.lang;
        let alias = self.backend.alias();
        let banner = self.banner.as_ref().map(|(m, e)| (m.as_str(), *e));
        if let Some(inc) = &self.incoming {
            render_share(p, l, &alias, &ShareBody::Incoming { lines: &inc.lines }, None);
            return;
        }
        match &self.screen {
            Screen::Devices => {
                let rows: Vec<ShareRow> = self
                    .backend
                    .peers()
                    .iter()
                    .map(|p| ShareRow { name: p.info.alias.clone(), detail: format!("{}  {}", p.ip, p.info.device_model.clone().unwrap_or_default()) })
                    .collect();
                render_share(
                    p,
                    l,
                    &alias,
                    &ShareBody::Devices { rows: &rows, selected: self.selected, discovering: self.backend.discovering(), port: self.backend.port(), receiving: self.receiving.as_ref() },
                    banner,
                );
            }
            Screen::Files { peer, items, selected, .. } => render_share(p, l, &alias, &ShareBody::Files { to: &peer.info.alias, items, selected: *selected }, banner),
            Screen::Confirm { lines, .. } => render_share(p, l, &alias, &ShareBody::Confirm { lines }, banner),
            Screen::Input { text, busy } => render_share(p, l, &alias, &ShareBody::Input { text, busy: *busy }, banner),
            Screen::Sending { to, progress, waiting } => render_share(p, l, &alias, &ShareBody::Sending { to, progress, waiting: *waiting }, banner),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};

    struct Fake {
        peers: Vec<Peer>,
        added: Mutex<Vec<String>>,
        sent: Mutex<Vec<(String, String)>>,
        slow: bool,
        saw_cancel: AtomicBool,
    }

    fn peer(alias: &str, fp: &str) -> Peer {
        let mut info = DeviceInfo::this_device(alias, fp, true);
        info.device_model = Some("Pixel".into());
        Peer { info, ip: "192.168.1.20".parse().unwrap(), verify: true }
    }

    impl Backend for Fake {
        fn alias(&self) -> String {
            "ZeroCart".into()
        }
        fn port(&self) -> u16 {
            53317
        }
        fn discovering(&self) -> bool {
            true
        }
        fn peers(&self) -> Vec<Peer> {
            self.peers.clone()
        }
        fn rescan(&self) {}
        fn add_manual(&self, text: &str) -> Result<Peer, String> {
            self.added.lock().unwrap().push(text.to_string());
            if text == "bad" { Err("adresse invalide".into()) } else { Ok(peer("Added", "FA")) }
        }
        fn send(&self, peer: &Peer, files: Vec<Outgoing>, progress: &mut dyn FnMut(SendEvent), cancel: &Cancel) -> Result<Outcome, String> {
            progress(SendEvent::Waiting);
            if self.slow {
                for _ in 0..100 {
                    if cancel.is_cancelled() {
                        self.saw_cancel.store(true, Ordering::SeqCst);
                        return Err("annulé".into());
                    }
                    std::thread::sleep(Duration::from_millis(20));
                }
            }
            progress(SendEvent::Uploading { file: files[0].name.clone(), index: 1, of: 1, done: files[0].size, total: files[0].size });
            self.sent.lock().unwrap().push((peer.info.alias.clone(), files[0].name.clone()));
            Ok(Outcome::Sent { files: 1 })
        }
    }

    fn share(tag: &str, slow: bool) -> (Share, Arc<Fake>, PathBuf, Arc<Mutex<Option<Arc<dyn Ui>>>>) {
        let dir = std::env::temp_dir().join(format!("zerocart-share-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("backups")).unwrap();
        std::fs::write(dir.join("dump.bin"), vec![1u8; 2000]).unwrap();
        std::fs::write(dir.join("notes.txt"), b"x").unwrap();
        std::fs::write(dir.join("backups/old.sav"), vec![2u8; 100]).unwrap();
        let fake = Arc::new(Fake { peers: vec![peer("Laptop", "L1"), peer("Phone", "P1")], added: Mutex::new(vec![]), sent: Mutex::new(vec![]), slow, saw_cancel: AtomicBool::new(false) });
        let ui_slot: Arc<Mutex<Option<Arc<dyn Ui>>>> = Arc::new(Mutex::new(None));
        let (f, slot) = (fake.clone(), ui_slot.clone());
        let starter: Starter = Box::new(move |ui, _| {
            *slot.lock().unwrap() = Some(ui);
            Ok(f.clone() as Arc<dyn Backend>)
        });
        (Share::start(&starter, dir.clone(), Lang::Fr).unwrap(), fake, dir, ui_slot)
    }

    fn wait_for(s: &mut Share, what: impl Fn(&Share) -> bool) {
        for _ in 0..200 {
            s.poll();
            if what(s) {
                return;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!("timed out");
    }

    #[test]
    fn a_file_is_sent_only_after_picking_it_and_confirming_on_y() {
        let (mut s, fake, dir, _) = share("send", false);
        assert!(!s.on_key(Key::Char('x')), "X is down"); // second device: Phone
        assert!(!s.on_key(Key::Enter));
        // the files: dumps and saves of the folder and of backups, not the .txt
        let Screen::Files { items, .. } = &s.screen else { panic!("not on the files screen") };
        assert_eq!(items.len(), 2, "{items:?}");
        assert!(items[0].starts_with("dump.bin") && items[1].starts_with("backups/old.sav"), "{items:?}");
        assert!(!s.on_key(Key::Enter));
        let Screen::Confirm { lines, .. } = &s.screen else { panic!("no confirmation") };
        assert!(lines[0].contains("Phone") && lines[2].contains("quitte l'appareil"), "{lines:?}");
        assert!(fake.sent.lock().unwrap().is_empty(), "nothing sent before Y");
        s.on_key(Key::Enter); // Enter does not confirm
        assert!(fake.sent.lock().unwrap().is_empty() && matches!(s.screen, Screen::Confirm { .. }));
        s.on_key(Key::Char('y'));
        wait_for(&mut s, |s| matches!(s.screen, Screen::Devices));
        assert_eq!(*fake.sent.lock().unwrap(), vec![("Phone".to_string(), "dump.bin".to_string())]);
        assert_eq!(s.banner, Some(("Envoyé à Phone".to_string(), false)));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn esc_goes_back_one_step_and_leaves_only_from_the_device_list() {
        let (mut s, fake, dir, _) = share("back", false);
        s.on_key(Key::Enter);
        s.on_key(Key::Enter);
        assert!(matches!(s.screen, Screen::Confirm { .. }));
        assert!(!s.on_key(Key::Esc) && matches!(s.screen, Screen::Files { .. }));
        assert!(!s.on_key(Key::Esc) && matches!(s.screen, Screen::Devices));
        assert!(s.on_key(Key::Esc), "Esc on the device list leaves");
        assert!(fake.sent.lock().unwrap().is_empty());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_request_from_another_device_waits_for_y_or_n() {
        let (mut s, _, dir, slot) = share("incoming", false);
        let ui = slot.lock().unwrap().clone().unwrap();
        let from = DeviceInfo::this_device("Pixel 8", "PX", true);
        let files = vec![FileMeta { id: "1".into(), file_name: "rom.gba".into(), size: 4 << 20, file_type: String::new(), sha256: None, preview: None }];
        let ask = |ui: Arc<dyn Ui>, from: DeviceInfo, files: Vec<FileMeta>| std::thread::spawn(move || ui.ask(&from, "10.0.0.2".parse().unwrap(), &files));
        let t = ask(ui.clone(), from.clone(), files.clone());
        wait_for(&mut s, |s| s.incoming.is_some());
        let lines = &s.incoming.as_ref().unwrap().lines;
        assert!(lines[0].contains("Pixel 8") && lines[1].contains("rom.gba") && lines[1].contains("4.0 Mo"), "{lines:?}");
        assert!(!s.on_key(Key::Enter), "Enter neither accepts nor declines");
        assert!(s.incoming.is_some());
        s.on_key(Key::Char('Y'));
        assert!(t.join().unwrap(), "Y accepts");
        let t = ask(ui.clone(), from.clone(), files.clone());
        wait_for(&mut s, |s| s.incoming.is_some());
        s.on_key(Key::Char('n'));
        assert!(!t.join().unwrap(), "N declines");
        let t = ask(ui, from, files);
        wait_for(&mut s, |s| s.incoming.is_some());
        s.on_key(Key::Esc);
        assert!(!t.join().unwrap(), "Esc declines too, and does not leave the screen");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn received_files_and_messages_show_in_the_banner_and_a_progress_bar() {
        let (mut s, _, dir, slot) = share("received", false);
        let ui = slot.lock().unwrap().clone().unwrap();
        ui.progress("save.sav", 50, 100);
        wait_for(&mut s, |s| s.receiving.is_some());
        assert_eq!(s.receiving.as_ref().unwrap().label, "Réception : save.sav");
        ui.saved(Path::new("/home/x/Downloads/save.sav"));
        wait_for(&mut s, |s| s.receiving.is_none());
        assert_eq!(s.banner, Some(("Reçu : save.sav".to_string(), false)));
        ui.message(&DeviceInfo::this_device("Pixel", "PX", true), "salut");
        wait_for(&mut s, |s| s.banner.as_ref().is_some_and(|b| b.0.contains("salut")));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn adding_a_device_by_address_types_letters_as_letters() {
        let (mut s, fake, dir, _) = share("input", false);
        s.on_key(Key::Char('a'));
        assert!(matches!(s.screen, Screen::Input { .. }));
        for c in "host-fzxc.local:1".chars() {
            s.on_key(Key::Char(c)); // F, Z, X and C must not become arrows here
        }
        s.on_key(Key::Backspace);
        s.on_key(Key::Enter);
        wait_for(&mut s, |s| matches!(s.screen, Screen::Devices));
        assert_eq!(*fake.added.lock().unwrap(), vec!["host-fzxc.local:".to_string()]);
        assert_eq!(s.banner, Some(("Ajouté : Added".to_string(), false)));
        // a failure is explained
        s.on_key(Key::Char('a'));
        for c in "bad".chars() {
            s.on_key(Key::Char(c));
        }
        s.on_key(Key::Enter);
        wait_for(&mut s, |s| matches!(s.screen, Screen::Devices));
        assert_eq!(s.banner, Some(("adresse invalide".to_string(), true)));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn esc_while_sending_cancels_the_transfer() {
        let (mut s, fake, dir, _) = share("cancel", true);
        s.on_key(Key::Enter);
        s.on_key(Key::Enter);
        s.on_key(Key::Char('y'));
        assert!(s.sending());
        std::thread::sleep(Duration::from_millis(100));
        assert!(!s.on_key(Key::Esc), "the screen is not left while sending");
        wait_for(&mut s, |s| matches!(s.screen, Screen::Devices));
        assert!(fake.saw_cancel.load(Ordering::SeqCst));
        assert_eq!(s.banner, Some(("annulé".to_string(), true)));
        assert!(fake.sent.lock().unwrap().is_empty());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn size_text_and_labels() {
        assert_eq!(size_text(100), "1 Ko");
        assert_eq!(size_text(2048), "2 Ko");
        assert_eq!(size_text(1 << 20), "1.0 Mo");
        assert_eq!(size_text(3 << 19), "1.5 Mo");
    }
}
