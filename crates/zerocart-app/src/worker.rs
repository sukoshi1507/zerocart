//! The thread that talks to the reader. LVGL-style rule: the UI thread never blocks on USB; it sends commands
//! and receives events over channels.
use zerocart_core::db::{agb_save_name, Crc32, Database, Entry};
use zerocart_core::header::{parse_agb, parse_dmg};
use zerocart_core::mapper::DmgMapper;
use zerocart_core::chis::{self, Chis};
use zerocart_core::transport::SerialTransport;
use zerocart_core::{find_ports, CartMode, CartReader, GbxCart};
use zerocart_flashchips::FlashBus;
use zerocart_ui::{CartView, ReaderView};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::Arc;
use std::time::{Duration, Instant};

#[derive(Debug)]
pub enum Cmd {
    /// Opens the reader the user confirmed and checks that it answers (the handshake).
    Connect(zerocart_core::ReaderKind),
    /// Closes the reader and forgets the cartridge.
    Disconnect,
    /// `dmg`: Game Boy mode (5 V). The default, safe mode is GBA (3.3 V).
    Detect { dmg: bool },
    ReadRom,
    ReadSave,
    /// Identify the flash chip of the inserted cartridge and read its CFI data (no erase, no program).
    Analyze,
    /// Game Boy: write the ROM file to the flash chip (the cartridge is backed up first).
    FlashRom(PathBuf),
    /// Game Boy: write the save file to the cartridge RAM (the current save is backed up first, if it is stable).
    WriteSave(PathBuf),
    Quit,
}

#[derive(Debug)]
pub enum Event {
    /// The answer to `Connect`: what the top bar shows about the reader, or why it did not answer.
    Connected(Result<ReaderView, String>),
    Detected(Result<(ReaderView, Option<CartView>), String>),
    Progress { label: String, done: u64, total: u64, kib_s: f32 },
    Finished(Result<String, String>),
    /// The lines of a flash chip analysis; a line starting with `!` is a warning.
    Analysis(Result<Vec<String>, String>),
}

/// The Krikzz programmer and the header of the cartridge in it.
struct MdState {
    kit: zerocart_core::krikzz::FlashKit<SerialTransport>,
    header: Option<zerocart_core::megadrive::MdHeader>,
    /// Whether the chip can be written above 0x400000: `None` until it was tested (the test is made when an image
    /// that big is written), `Some(false)` on a cartridge wired as a 4 MiB one.
    upper: Option<bool>,
}

/// What the worker knows about the inserted cartridge.
struct Cart {
    mode: CartMode,
    entry: Option<Entry>,
    rom_size: usize,
    title: String,
    /// Game Boy only: how to select ROM banks and the header's global checksum.
    dmg: Option<(Option<DmgMapper>, u16)>,
    /// Game Boy cartridge RAM size from the header.
    ram_size: usize,
}

pub struct Worker {
    pub cancel: Arc<AtomicBool>,
    pub out_dir: PathBuf,
    pub db_dir: PathBuf,
}

/// The reader that is plugged in: the application talks to either one through [`CartReader`], and reaches for the
/// concrete type only for what differs (flashing).
enum Dev {
    Gbx(GbxCart<SerialTransport>),
    Chis(Chis<SerialTransport>),
}

impl Dev {
    fn r(&mut self) -> &mut dyn CartReader {
        match self {
            Dev::Gbx(d) => d,
            Dev::Chis(d) => d,
        }
    }

    fn bus(&mut self) -> &mut dyn FlashBus {
        match self {
            Dev::Gbx(d) => d,
            Dev::Chis(d) => d,
        }
    }

    /// The reader of this kind (the Krikzz Mega Drive programmer is not a cartridge reader of this application yet).
    fn open_kind(kind: zerocart_core::ReaderKind) -> Result<Dev, String> {
        use zerocart_core::ReaderKind;
        match kind {
            ReaderKind::GbxCart => {
                let port = find_ports().into_iter().next().ok_or("GBFlash / GBxCart RW non trouvé")?;
                let t = SerialTransport::open(&port, 1_000_000).map_err(|e| e.to_string())?;
                GbxCart::connect(t, Duration::from_millis(200)).map(Dev::Gbx).map_err(|e| format!("le lecteur ne répond pas : {e}"))
            }
            ReaderKind::Chisflash => {
                let port = chis::find_ports().into_iter().next().ok_or("Chisflash non trouvé")?;
                let t = SerialTransport::open(&port, chis::BAUD).map_err(|e| e.to_string())?;
                Chis::new(t).map(Dev::Chis).map_err(|e| format!("le lecteur ne répond pas : {e}"))
            }
            other => Err(format!("{} : bientôt", other.label())),
        }
    }

    /// The first reader found: GBxCart RW / GBFlash first, then the Chisflash.
    #[allow(dead_code)]
    fn open() -> Result<Dev, String> {
        if let Some(port) = find_ports().into_iter().next() {
            let t = SerialTransport::open(&port, 1_000_000).map_err(|e| e.to_string())?;
            return GbxCart::connect(t, Duration::from_millis(200)).map(Dev::Gbx).map_err(|e| e.to_string());
        }
        if let Some(port) = chis::find_ports().into_iter().next() {
            let t = SerialTransport::open(&port, chis::BAUD).map_err(|e| e.to_string())?;
            return Chis::new(t).map(Dev::Chis).map_err(|e| e.to_string());
        }
        Err("lecteur non trouvé".into())
    }
}

impl Worker {
    pub fn run(self, rx: Receiver<Cmd>, tx: Sender<Event>) {
        let mut dev: Option<Dev> = None;
        let mut cart: Option<Cart> = None;
        let mut kind: Option<zerocart_core::ReaderKind> = None;
        // The Krikzz Mega Drive programmer is not a CartReader: it has its own state and commands.
        let mut md: Option<MdState> = None;
        while let Ok(cmd) = rx.recv() {
            self.cancel.store(false, Ordering::SeqCst);
            if let Some(state) = md.as_mut() {
                if matches!(cmd, Cmd::Detect { .. } | Cmd::ReadRom | Cmd::ReadSave | Cmd::Analyze | Cmd::FlashRom(_) | Cmd::WriteSave(_)) {
                    self.md_command(state, cmd, &tx);
                    continue;
                }
            }
            match cmd {
                Cmd::Connect(zerocart_core::ReaderKind::KrikzzMd) => {
                    if let Some(mut d) = dev.take() {
                        let _ = d.r().close();
                    }
                    cart = None;
                    kind = None;
                    md = None;
                    match self.md_connect() {
                        Ok((kit, view)) => {
                            md = Some(MdState { kit, header: None, upper: None });
                            kind = Some(zerocart_core::ReaderKind::KrikzzMd);
                            let _ = tx.send(Event::Connected(Ok(view)));
                        }
                        Err(e) => {
                            let _ = tx.send(Event::Connected(Err(e)));
                        }
                    }
                }
                Cmd::Connect(k) => {
                    md = None;
                    if let Some(mut d) = dev.take() {
                        let _ = d.r().close();
                    }
                    cart = None;
                    kind = None;
                    match self.connect(k) {
                        Ok((d, view)) => {
                            dev = d;
                            kind = Some(k);
                            let _ = tx.send(Event::Connected(Ok(view)));
                        }
                        Err(e) => {
                            let _ = tx.send(Event::Connected(Err(e)));
                        }
                    }
                }
                Cmd::Disconnect => {
                    md = None;
                    if let Some(mut d) = dev.take() {
                        let _ = d.r().close();
                    }
                    cart = None;
                    kind = None;
                }
                Cmd::Analyze => {
                    let r = match (dev.as_mut(), cart.as_ref()) {
                        (Some(d), Some(c)) => self.analyze(d, c),
                        _ => Err("aucune cartouche".into()),
                    };
                    let _ = tx.send(Event::Analysis(r));
                }
                Cmd::FlashRom(path) => {
                    let r = match (dev.as_mut(), cart.as_ref()) {
                        (Some(d), Some(c)) => self.flash_rom(d, c, &path, &tx),
                        _ => Err("aucune cartouche".into()),
                    };
                    self.after_write(&mut dev, kind, &mut cart, r, &tx);
                }
                Cmd::WriteSave(path) => {
                    let r = match (dev.as_mut(), cart.as_ref()) {
                        (Some(d), Some(c)) => self.write_save(d, c, &path, &tx),
                        _ => Err("aucune cartouche".into()),
                    };
                    self.after_write(&mut dev, kind, &mut cart, r, &tx);
                }
                Cmd::Quit => {
                    if let Some(d) = dev.as_mut() {
                        let _ = d.r().close();
                    }
                    return;
                }
                Cmd::Detect { dmg } => {
                    let r = self.detect(&mut dev, kind, dmg);
                    let _ = tx.send(Event::Detected(r.map(|(rv, c)| {
                        let view = c.as_ref().map(|(v, _)| v.clone());
                        cart = c.map(|(_, c)| c);
                        (rv, view)
                    })));
                }
                Cmd::ReadRom => {
                    let r = match (dev.as_mut(), cart.as_ref()) {
                        (Some(d), Some(c)) => self.read_rom(d, c, &tx),
                        _ => Err("aucune cartouche".into()),
                    };
                    let _ = tx.send(Event::Finished(r));
                }
                Cmd::ReadSave => {
                    let r = match (dev.as_mut(), cart.as_ref()) {
                        (Some(d), Some(c)) => self.read_save(d, c, &tx),
                        _ => Err("aucune cartouche".into()),
                    };
                    let _ = tx.send(Event::Finished(r));
                }
            }
        }
    }

    /// Opens the Krikzz programmer (the handshake refuses any other device) and keeps it for the commands that follow.
    fn md_connect(&self) -> Result<(zerocart_core::krikzz::FlashKit<SerialTransport>, ReaderView), String> {
        let port = zerocart_core::krikzz::find_ports().into_iter().next().ok_or("carte Krikzz non trouvée")?;
        let t = SerialTransport::open(&port, 9600).map_err(|e| format!("{port} : {e}"))?;
        let kit = zerocart_core::krikzz::FlashKit::connect(t).map_err(|e| format!("la carte ne répond pas : {e}"))?;
        Ok((kit, ReaderView { name: "Krikzz FlashKit MD".into(), firmware: String::new(), voltage: "" }))
    }

    /// The commands of the Mega Drive system. Reading only: writing is refused until it is wired with its safeguards.
    fn md_command(&self, md: &mut MdState, cmd: Cmd, tx: &Sender<Event>) {
        match cmd {
            Cmd::Detect { .. } => {
                let r = self.md_detect(md);
                let _ = tx.send(Event::Detected(r.map(|view| (ReaderView { name: "Krikzz FlashKit MD".into(), firmware: String::new(), voltage: "" }, view))));
            }
            Cmd::ReadRom => {
                let r = self.md_read_rom(md, tx);
                let _ = tx.send(Event::Finished(r));
            }
            Cmd::ReadSave => {
                let r = self.md_read_save(md, tx);
                let _ = tx.send(Event::Finished(r));
            }
            Cmd::Analyze => {
                let r = self.md_analyze(md);
                let _ = tx.send(Event::Analysis(r));
            }
            Cmd::FlashRom(path) => {
                let r = self.md_flash_rom(md, &path, tx);
                // the cartridge changed (or the attempt may have): read its header again, then report
                if let Ok(view) = self.md_detect(md) {
                    let reader = ReaderView { name: "Krikzz FlashKit MD".into(), firmware: String::new(), voltage: "" };
                    let _ = tx.send(Event::Detected(Ok((reader, view))));
                }
                let _ = tx.send(Event::Finished(r));
            }
            _ => {
                let _ = tx.send(Event::Finished(Err("Écriture de sauvegarde Mega Drive : pas encore gérée".into())));
            }
        }
    }

    /// Writes a ROM file to the flash cartridge: checks, backup of what is overwritten (two reads that must agree),
    /// programming of the sectors that differ, then a full read back. From the programming on it cannot be cancelled.
    fn md_flash_rom(&self, md: &mut MdState, path: &Path, tx: &Sender<Event>) -> Result<String, String> {
        use zerocart_core::krikzz::UPPER_HALF;
        let cancelled = |e: zerocart_core::Error| if matches!(e, zerocart_core::Error::Cancelled) { "annulé".to_string() } else { e.to_string() };
        let image = std::fs::read(path).map_err(|e| e.to_string())?;
        if image.len() < 0x200 || image.len() % 2 != 0 {
            return Err("taille de fichier invalide pour une ROM Mega Drive".into());
        }
        let file_header = zerocart_core::megadrive::parse_header(&image).filter(|h| h.sega_ok);
        let Some(file_header) = file_header else {
            return Err("ce fichier n'est pas une ROM Mega Drive valide (en-tête SEGA absent ; les fichiers .smd entrelacés ne sont pas gérés)".into());
        };
        let geo = md
            .kit
            .read_geometry()
            .map_err(|e| e.to_string())?
            .ok_or("puce flash non reconnue (cartouche non réinscriptible) : rien n'est écrit")?;
        if geo.command_set != 2 {
            return Err("seules les puces AMD/Spansion sont gérées : rien n'est écrit".into());
        }
        if image.len() > geo.size_bytes {
            return Err(format!("la ROM ({}) dépasse la puce ({})", size_label(image.len()), size_label(geo.size_bytes)));
        }
        // Above 4 MiB the chip is not always reachable: a standard cartridge is wired for 4 MiB only. Say so instead of
        // failing half way (the test leaves the cartridge as it was).
        if image.len() > UPPER_HALF {
            let _ = tx.send(Event::Progress { label: "Test de la moitié haute de la puce".into(), done: 0, total: 0, kib_s: 0.0 });
            let ok = md.kit.upper_half_writable(&geo).map_err(|e| e.to_string())?;
            md.upper = Some(ok);
            if !ok {
                return Err(format!(
                    "Cartouche limitée à 4 Mo : la moitié haute de la puce est inaccessible. ROM de {} refusée, rien n'est effacé",
                    size_label(image.len())
                ));
            }
        }
        let extent = geo.extent(image.len());
        let started = Instant::now();

        // 1. Back up what will be overwritten: two reads that must agree.
        let mut passes: Vec<Vec<u8>> = Vec::new();
        for pass in 0..2usize {
            let mut v = Vec::with_capacity(extent);
            md.kit
                .dump_rom(extent, &mut v, &mut |done| {
                    let total = 2 * extent;
                    let done = pass * extent + done;
                    let kib_s = done as f32 / 1024.0 / started.elapsed().as_secs_f32().max(0.001);
                    let _ = tx.send(Event::Progress { label: "Sauvegarde de la cartouche avant écriture".into(), done: done as u64, total: total as u64, kib_s });
                    !self.cancel.load(Ordering::SeqCst)
                })
                .map_err(cancelled)?;
            passes.push(v);
        }
        if passes[0] != passes[1] {
            return Err("deux lectures de la cartouche diffèrent (mauvais contact ?) : rien n'est écrit".into());
        }
        let backups = self.out_dir.join("backups");
        std::fs::create_dir_all(&backups).map_err(|e| e.to_string())?;
        let title = if file_header.title().is_empty() { "megadrive".to_string() } else { sanitize(file_header.title()) };
        let backup = unique_path(&backups, &format!("{title} avant flash"), "bin");
        std::fs::write(&backup, &passes[0]).map_err(|e| e.to_string())?;
        let name = backup.file_name().unwrap_or_default().to_string_lossy().into_owned();

        // 2. Erase and program the sectors that differ. From here on it cannot be cancelled.
        let t1 = Instant::now();
        let mut on_progress = |msg: &str, done: usize, total: usize| {
            let bytes: usize = geo.sectors.iter().take(done).map(|s| s.1 as usize).sum();
            let _ = tx.send(Event::Progress {
                label: format!("Écriture ({done}/{total}) ne pas débrancher : {msg}"),
                done: done as u64,
                total: total as u64,
                kib_s: bytes as f32 / 1024.0 / t1.elapsed().as_secs_f32().max(0.001),
            });
        };
        let report = md
            .kit
            .flash_rom(&geo, &image, &mut on_progress, &|| false)
            .map_err(|e| format!("{e} ; ancienne ROM : backups/{name}"))?;

        // 3. Read everything back.
        let mut after = Vec::with_capacity(extent);
        md.kit
            .dump_rom(extent, &mut after, &mut |done| {
                let _ = tx.send(Event::Progress { label: "Vérification de l'écriture".into(), done: done as u64, total: extent as u64, kib_s: 0.0 });
                true
            })
            .map_err(|e| e.to_string())?;
        let mut want = image.clone();
        want.resize(extent, 0xFF);
        if after == want {
            Ok(format!("ROM écrite et vérifiée ({} secteurs réécrits, {} inchangés) ; ancienne ROM : backups/{name}", report.rewritten, report.unchanged))
        } else {
            Err(format!("ROM écrite mais la relecture diffère ; ancienne ROM : backups/{name}"))
        }
    }

    /// Reads the "SEGA" header of the inserted cartridge. `None`: no valid Mega Drive header (empty connector, blank
    /// flash cartridge, another system).
    fn md_detect(&self, md: &mut MdState) -> Result<Option<CartView>, String> {
        let header = md.kit.read_header().map_err(|e| e.to_string())?;
        md.header = header.clone().filter(|h| h.is_valid());
        Ok(md.header.as_ref().map(|h| {
            let save = match h.ram {
                Some(r) => format!("{} Ko{}", r.size() / 1024, if r.odd_bytes { " (octets impairs)" } else { "" }),
                None => "aucune".into(),
            };
            CartView {
                system: "Mega Drive",
                title: h.title().to_string(),
                code: if h.serial.is_empty() { h.system.clone() } else { h.serial.clone() },
                rom: h.rom_size().map_or("?".into(), size_label),
                save,
                database: Some(format!("{} ({})", h.system, h.region)),
                header_ok: h.is_valid(),
            }
        }))
    }

    fn md_read_rom(&self, md: &mut MdState, tx: &Sender<Event>) -> Result<String, String> {
        let h = md.header.clone().ok_or("aucune cartouche Mega Drive")?;
        let size = h.rom_size().ok_or("taille de ROM inconnue (en-tête invalide)")?;
        std::fs::create_dir_all(&self.out_dir).map_err(|e| e.to_string())?;
        let title = if h.title().is_empty() { "megadrive".to_string() } else { sanitize(h.title()) };
        let path = unique_path(&self.out_dir, &title, h.extension());
        let mut out = std::io::BufWriter::new(std::fs::File::create(&path).map_err(|e| e.to_string())?);
        let started = Instant::now();
        let label = format!("Lecture ROM : {}", h.title());
        let dump = md.kit.dump_rom(size, &mut out, &mut |done| {
            let kib_s = done as f32 / 1024.0 / started.elapsed().as_secs_f32().max(0.001);
            let _ = tx.send(Event::Progress { label: label.clone(), done: done as u64, total: size as u64, kib_s });
            !self.cancel.load(Ordering::SeqCst)
        });
        let dump = match dump {
            Ok(d) => d,
            Err(e) => {
                drop(out);
                let _ = std::fs::remove_file(&path);
                return Err(if matches!(e, zerocart_core::Error::Cancelled) { "annulé".into() } else { e.to_string() });
            }
        };
        out.flush().map_err(|e| e.to_string())?;
        let file = path.file_name().unwrap_or_default().to_string_lossy().into_owned();
        Ok(if dump.checksum16 == h.checksum {
            format!("ROM lue, checksum de l'en-tête OK (CRC32 {:08x}) : {file}", dump.crc32)
        } else {
            // Homebrew and some repros carry a wrong checksum in their header: the CRC32 is the real proof.
            format!("!ROM lue, mais le checksum de l'en-tête diffère (fréquent sur les ROM amateur) : {file}, CRC32 {:08x}", dump.crc32)
        })
    }

    fn md_read_save(&self, md: &mut MdState, tx: &Sender<Event>) -> Result<String, String> {
        let h = md.header.clone().ok_or("aucune cartouche Mega Drive")?;
        let ram = h.ram.ok_or("l'en-tête ne déclare aucune sauvegarde")?;
        let _ = tx.send(Event::Progress { label: "Lecture de la sauvegarde".into(), done: 0, total: ram.size() as u64, kib_s: 0.0 });
        let stream = md.kit.read_save_stream(ram.size()).map_err(|e| e.to_string())?;
        std::fs::create_dir_all(&self.out_dir).map_err(|e| e.to_string())?;
        let title = if h.title().is_empty() { "megadrive".to_string() } else { sanitize(h.title()) };
        // The file keeps the bus's word stream (data on the odd bytes), as the reference client saves it.
        let path = unique_path(&self.out_dir, &format!("{title} (non vérifiée)"), "sav");
        std::fs::write(&path, &stream).map_err(|e| e.to_string())?;
        let file = path.file_name().unwrap_or_default().to_string_lossy().into_owned();
        Ok(format!("!Attention : sauvegarde NON vérifiée, ce lecteur ne coupe pas l'alimentation (pile morte invisible) : {file}"))
    }

    /// Read-only identification of the flash chip (CFI query, identifiers, sectors).
    fn md_analyze(&self, md: &mut MdState) -> Result<Vec<String>, String> {
        let mut lines = vec!["Carte : Krikzz FlashKit MD".to_string()];
        match md.kit.probe_flash().map_err(|e| e.to_string())? {
            None => {
                lines.push("Aucune puce flash ne répond :".into());
                lines.push("ROM masquée, non réinscriptible.".into());
            }
            Some(f) => {
                lines.push(format!("CFI : {}", if f.cfi_ok { "répond" } else { "ne répond pas" }));
                if f.size_bytes > 0 {
                    lines.push(format!("Puce : {} Mo", f.size_bytes / (1 << 20)));
                }
                lines.push(format!(
                    "Fabricant 0x{:04X}, puce {}",
                    f.manufacturer,
                    f.device_id.iter().map(|d| format!("{d:04X}")).collect::<Vec<_>>().join("/")
                ));
                if f.byte_wide {
                    lines.push("Deux puces 8 bits (cartouche de copie)".into());
                }
                match md.upper {
                    Some(true) => lines.push("Au-delà de 4 Mo : accessible en écriture".into()),
                    Some(false) => lines.push("!Au-delà de 4 Mo : inaccessible (cartouche câblée en 4 Mo)".into()),
                    None if f.size_bytes > 4 << 20 => lines.push("Au-delà de 4 Mo : testé quand on écrit une ROM plus grande".into()),
                    None => {}
                }
                if let Some(g) = md.kit.read_geometry().map_err(|e| e.to_string())? {
                    let mut runs: Vec<(u32, u32)> = Vec::new();
                    for &(_, size) in &g.sectors {
                        match runs.last_mut() {
                            Some(r) if r.0 == size => r.1 += 1,
                            _ => runs.push((size, 1)),
                        }
                    }
                    let desc: Vec<String> = runs.iter().map(|(sz, n)| format!("{n} × {} Ko", sz / 1024)).collect();
                    lines.push(format!("Secteurs : {}", desc.join(" + ")));
                }
            }
        }
        Ok(lines)
    }

    /// Opens the reader of `kind` and asks it who it is. The Krikzz programmer is only handshaken (nothing is read
    /// from a cartridge yet) and closed again.
    fn connect(&self, kind: zerocart_core::ReaderKind) -> Result<(Option<Dev>, ReaderView), String> {
        use zerocart_core::ReaderKind;
        if kind == ReaderKind::KrikzzMd {
            let port = zerocart_core::krikzz::find_ports().into_iter().next().ok_or("carte Krikzz non trouvée")?;
            let t = SerialTransport::open(&port, 9600).map_err(|e| format!("{port} : {e}"))?;
            zerocart_core::krikzz::FlashKit::connect(t).map_err(|e| format!("la carte ne répond pas : {e}"))?;
            return Ok((None, ReaderView { name: "Krikzz FlashKit MD".into(), firmware: String::new(), voltage: "" }));
        }
        let mut d = Dev::open_kind(kind)?;
        let info = d.r().info(CartMode::Agb);
        let view = ReaderView { name: info.name, firmware: info.firmware, voltage: info.voltage };
        Ok((Some(d), view))
    }

    /// After a write the cartridge may have changed: identify it again, then report the result.
    fn after_write(&self, dev: &mut Option<Dev>, kind: Option<zerocart_core::ReaderKind>, cart: &mut Option<Cart>, result: Result<String, String>, tx: &Sender<Event>) {
        let dmg = cart.as_ref().is_none_or(|c| c.mode == CartMode::Dmg);
        if let Ok((reader, found)) = self.detect(dev, kind, dmg) {
            let view = found.as_ref().map(|(v, _)| v.clone());
            *cart = found.map(|(_, c)| c);
            let _ = tx.send(Event::Detected(Ok((reader, view))));
        }
        let _ = tx.send(Event::Finished(result));
    }

    /// Identifies the flash chip and reads its CFI table, as a list of short lines for the Tools tab.
    fn analyze(&self, d: &mut Dev, c: &Cart) -> Result<Vec<String>, String> {
        use zerocart_flashchips::{agb, detect, load_dir};
        let agb_mode = c.mode == CartMode::Agb;
        let (profiles, _) = load_dir(&self.db_dir);
        if profiles.is_empty() {
            return Err(format!("profils de puces introuvables dans {}", self.db_dir.display()));
        }
        let found = detect(d.bus(), &profiles, if agb_mode { "AGB" } else { "DMG" })?;
        let volts = matches!(d, Dev::Gbx(_));
        let mut lines = vec![format!(
            "Mode : {}",
            match (agb_mode, volts) {
                (true, true) => "GBA, 3,3 V",
                (false, true) => "Game Boy, 5 V",
                (true, false) => "GBA (Chisflash)",
                (false, false) => "Game Boy (Chisflash)",
            }
        )];
        if found.id_lines.is_empty() && found.cfi.is_none() {
            lines.push("Aucune puce flash ne répond :".into());
            lines.push("ROM masquée, non réinscriptible.".into());
            return Ok(lines);
        }
        for l in found.id_lines.iter().take(3) {
            lines.push(format!("Réponse {l}"));
        }
        match found.matches.as_slice() {
            [] => {
                let probable = if agb_mode {
                    agb::choose_profile(d.bus(), &profiles, &found.matches, &found.answered).ok()
                } else {
                    None
                };
                match probable {
                    Some(i) => {
                        lines.push("Aucun profil exact. Profil probable :".into());
                        lines.push(format!("  {}", profiles[i].name));
                    }
                    None => lines.push("!Aucun profil connu pour cette puce.".into()),
                }
            }
            m => {
                lines.push(if m.len() == 1 { "Profil reconnu :".into() } else { "Profils compatibles :".into() });
                for &i in m.iter().take(3) {
                    lines.push(format!("  {}", profiles[i].name));
                }
                if m.len() > 3 {
                    lines.push(format!("  (+ {} autres)", m.len() - 3));
                }
            }
        }
        if let Some(w) = found.we_pin {
            lines.push(format!("Broche d'écriture : {w}"));
        }
        match &found.cfi {
            None => lines.push("!CFI : non lue (puce muette ou non standard).".into()),
            Some(cfi) => {
                lines.push(format!(
                    "CFI : {} Mo, {:.1} à {:.1} V",
                    cfi.device_size / (1 << 20),
                    cfi.vdd_min,
                    cfi.vdd_max
                ));
                lines.push(match cfi.buffer_size {
                    Some(b) => format!("Écriture par tampon : {b} octets"),
                    None => "Écriture par tampon : non".into(),
                });
                let runs: Vec<String> = cfi
                    .sector_runs()
                    .iter()
                    .map(|(size, n)| format!("{n} × {} Ko", size / 1024))
                    .collect();
                lines.push(format!("Secteurs : {}", runs.join(" + ")));
                if !cfi.swapped.is_empty() {
                    let pins: Vec<String> = cfi.swapped.iter().map(|(a, b)| format!("D{a}↔D{b}")).collect();
                    lines.push(format!("!Lignes de données croisées : {}", pins.join(", ")));
                }
            }
        }
        Ok(lines)
    }

    /// GBA: erase the whole flash chip and write the ROM (profile chosen from what the chip answers), after a
    /// backup of the whole chip made of two reads that must agree.
    fn flash_rom_agb(&self, d: &mut GbxCart<SerialTransport>, path: &Path, tx: &Sender<Event>) -> Result<String, String> {
        use zerocart_flashchips::{agb, detect, load_dir, program};
        let rom = std::fs::read(path).map_err(|e| e.to_string())?;
        if !parse_agb(&rom).is_some_and(|h| h.logo_ok && h.header_checksum_ok) {
            return Err("ce fichier n'est pas une ROM GBA valide (en-tête incorrect)".into());
        }
        let (profiles, _) = load_dir(&self.db_dir);
        let found = detect(d, &profiles, "AGB")?;
        let cfi = found.cfi.as_ref().ok_or("puce non identifiée (pas de CFI) : rien n'est écrit")?;
        if rom.len() as u64 > cfi.device_size {
            return Err(format!("la ROM ({} Ko) dépasse la puce ({} Ko)", rom.len() / 1024, cfi.device_size / 1024));
        }
        let idx = agb::choose_profile(d, &profiles, &found.matches, &found.answered)?;
        let profile = &profiles[idx];
        let chip = cfi.device_size as usize;
        let started = Instant::now();

        // 1. Back up the whole chip (it is all erased): two reads that must agree.
        let mut pass = 0u64;
        let current = program::read_until_stable(|| {
            let base = pass * chip as u64;
            pass += 1;
            let mut v = Vec::with_capacity(chip);
            while v.len() < chip {
                if self.cancel.load(Ordering::SeqCst) {
                    return Err("annulé".to_string());
                }
                let n = 0x4000.min(chip - v.len());
                v.extend(d.read_rom(v.len() as u32, n).map_err(|e| e.to_string())?);
                let done = base + v.len() as u64;
                let _ = tx.send(Event::Progress {
                    label: "Sauvegarde de la puce avant écriture".into(),
                    done,
                    total: 2 * chip as u64,
                    kib_s: done as f32 / 1024.0 / started.elapsed().as_secs_f32().max(0.001),
                });
            }
            Ok(v)
        })?;
        let backups = self.out_dir.join("backups");
        std::fs::create_dir_all(&backups).map_err(|e| e.to_string())?;
        let title = parse_agb(&rom).map(|h| h.title).unwrap_or_default();
        let backup = unique_path(&backups, &format!("{} puce avant flash", sanitize(&title)), "gba");
        std::fs::write(&backup, &current).map_err(|e| e.to_string())?;
        let name = backup.file_name().unwrap_or_default().to_string_lossy().into_owned();

        // 2. Erase and write. From here on the transfer cannot be cancelled.
        let t1 = Instant::now();
        let report = agb::flash_rom(d, profile, &rom, &mut |done, total, m| {
            let _ = tx.send(Event::Progress {
                label: format!("Écriture GBA, ne pas débrancher : {m}"),
                done: done as u64,
                total: total as u64,
                kib_s: done as f32 / 1024.0 / t1.elapsed().as_secs_f32().max(0.001),
            });
        })
        .map_err(|e| format!("{e} ; ancienne puce : backups/{name}"))?;
        Ok(format!(
            "ROM GBA écrite et vérifiée ({} essai, {} octets réparés) ; ancienne puce : backups/{name}",
            report.attempts, report.repaired
        ))
    }

    fn flash_rom(&self, d: &mut Dev, c: &Cart, path: &Path, tx: &Sender<Event>) -> Result<String, String> {
        use zerocart_flashchips::{detect, load_dir, program};
        let mapper = match c.dmg {
            Some((Some(m), _)) if m == DmgMapper::Mbc5 => m,
            Some(_) => return Err("seul le flashage de cartouches MBC5 est géré".into()),
            None => {
                return match d {
                    Dev::Gbx(g) => self.flash_rom_agb(g, path, tx),
                    Dev::Chis(_) => Err("écriture d'une ROM GBA avec le Chisflash : pas encore gérée".into()),
                }
            }
        };
        if matches!(d, Dev::Chis(_)) && mapper != DmgMapper::Mbc5 {
            return Err("seul le flashage de cartouches MBC5 est géré".into());
        }
        let rom = std::fs::read(path).map_err(|e| e.to_string())?;
        if !rom.len().is_power_of_two() || rom.len() < 0x8000 {
            return Err("taille de fichier invalide pour une ROM Game Boy".into());
        }
        if !parse_dmg(&rom).is_some_and(|h| h.logo_ok) {
            return Err("ce fichier n'est pas une ROM Game Boy valide (logo absent)".into());
        }
        let (profiles, _) = load_dir(&self.db_dir);
        let found = detect(d.bus(), &profiles, "DMG")?;
        let (Some(cfi), [idx]) = (found.cfi.as_ref(), found.matches.as_slice()) else {
            return Err("puce flash non reconnue (ou ambiguë) : rien n'est écrit".into());
        };
        let profile = &profiles[*idx];
        if rom.len() as u64 > cfi.device_size {
            return Err(format!("la ROM ({} Ko) dépasse la puce ({} Ko)", rom.len() / 1024, cfi.device_size / 1024));
        }
        let chip = cfi.device_size as usize;
        let started = Instant::now();

        // 1. Back up the cartridge as it is (two reads that must agree).
        let mut pass = 0u64;
        let current = program::read_until_stable(|| {
            let label = "Sauvegarde de la cartouche avant écriture".to_string();
            let mut v = Vec::new();
            let base = pass * chip as u64;
            pass += 1;
            d.r().dump_dmg_rom(mapper, chip, &mut v, &mut |done| {
                let kib_s = (base as f32 + done as f32) / 1024.0 / started.elapsed().as_secs_f32().max(0.001);
                let _ = tx.send(Event::Progress { label: label.clone(), done: base + done as u64, total: 2 * chip as u64, kib_s });
                !self.cancel.load(Ordering::SeqCst)
            })
            .map_err(|e| if matches!(e, zerocart_core::Error::Cancelled) { "annulé".to_string() } else { e.to_string() })?;
            Ok(v)
        })?;
        let backups = self.out_dir.join("backups");
        std::fs::create_dir_all(&backups).map_err(|e| e.to_string())?;
        let backup = unique_path(&backups, &format!("{} avant flash", sanitize(&c.title)), "gb");
        std::fs::write(&backup, &current).map_err(|e| e.to_string())?;

        // 2. Write, only the sectors that differ. From here on the transfer cannot be cancelled.
        let sectors = cfi.sectors();
        let t1 = Instant::now();
        let mut on_progress = |i: usize, n: usize, m: &str| {
            let done = sectors.iter().filter(|s| s.0 < rom.len() as u64).nth(i).map_or(rom.len() as u64, |s| s.0);
            let _ = tx.send(Event::Progress {
                label: format!("Écriture ({}/{n}) ne pas débrancher : {m}", i.min(n)),
                done,
                total: rom.len() as u64,
                kib_s: done as f32 / 1024.0 / t1.elapsed().as_secs_f32().max(0.001),
            });
        };
        let report = match d {
            Dev::Gbx(g) => program::flash_rom(g, profile, &sectors, &rom, &mut on_progress, &|| false),
            Dev::Chis(c) => zerocart_flashchips::chis_gb::flash_rom(c, profile, &sectors, &rom, &mut on_progress, &|| false),
        }
        .map_err(|e| format!("{e} : sauvegarde dans {}", backup.display()))?;

        // 3. Verify.
        let mut after = Vec::new();
        d.r().dump_dmg_rom(mapper, rom.len(), &mut after, &mut |_| true).map_err(|e| e.to_string())?;
        let name = backup.file_name().unwrap_or_default().to_string_lossy().into_owned();
        if after == rom {
            Ok(format!("ROM écrite et vérifiée ({} secteurs) ; ancienne ROM : backups/{name}", report.rewritten))
        } else {
            Err(format!("ROM écrite mais la relecture diffère ; ancienne ROM : backups/{name}"))
        }
    }

    fn write_save(&self, d: &mut Dev, c: &Cart, path: &Path, tx: &Sender<Event>) -> Result<String, String> {
        // Game Boy: the RAM size comes from the header and the mapper; GBA: the database gives the SRAM size.
        let (size, mapper) = match c.dmg {
            Some((Some(_), _)) if c.ram_size == 0 => return Err("cette cartouche n'a pas de sauvegarde".into()),
            Some((Some(m), _)) => (c.ram_size, Some(m)),
            Some(_) => return Err("mapper pas encore géré".into()),
            None => {
                let e = c.entry.as_ref().ok_or("jeu absent de la base : type de sauvegarde inconnu")?;
                if !matches!(e.st, 3 | 7 | 8) {
                    return Err(format!("sauvegarde « {} » : pas encore gérée", agb_save_name(e.st)));
                }
                (e.ss as usize, None)
            }
        };
        let data = std::fs::read(path).map_err(|e| e.to_string())?;
        if data.len() != size {
            return Err(format!("le fichier fait {} octets, la sauvegarde {}", data.len(), size));
        }
        let started = Instant::now();
        let progress = |label: &str, base: usize, total: usize| {
            let label = label.to_string();
            move |done: usize, cancel: &AtomicBool, tx: &Sender<Event>| {
                let kib_s = (base + done) as f32 / 1024.0 / started.elapsed().as_secs_f32().max(0.001);
                let _ = tx.send(Event::Progress { label: label.clone(), done: (base + done) as u64, total: total as u64, kib_s });
                !cancel.load(Ordering::SeqCst)
            }
        };
        let total = size * 4;
        // 1. Keep the current save, when it reads the same twice.
        let p_ = progress("Sauvegarde de l'ancienne sauvegarde", 0, total);
        let a = read_save_data(d, mapper, size, &mut |n| p_(n, &self.cancel, tx))?;
        let p_ = progress("Sauvegarde de l'ancienne sauvegarde", size, total);
        let b = read_save_data(d, mapper, size, &mut |n| p_(n, &self.cancel, tx))?;
        let note: String;
        if a == b {
            let dir = self.out_dir.join("backups");
            std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
            let mut stem = format!("{} avant écriture", sanitize(&c.title));
            if !d.r().can_power_cycle() {
                stem = unverified_stem(&stem);
            }
            let file = unique_path(&dir, &stem, "sav");
            std::fs::write(&file, &a).map_err(|e| e.to_string())?;
            note = format!(" ; ancienne : backups/{}", file.file_name().unwrap_or_default().to_string_lossy());
        } else {
            note = " ; ancienne sauvegarde instable, non gardée".into();
        }
        // 2. Write, read back in the same power session.
        let p_ = progress("Écriture de la sauvegarde", 2 * size, total);
        write_save_data(d, mapper, &data, &mut |n| p_(n, &self.cancel, tx))?;
        let back = read_save_data(d, mapper, data.len(), &mut |_| true)?;
        if back != data {
            return Err(format!("la relecture diffère de ce qui a été écrit{note}"));
        }
        // 3. Does it survive a power cycle? (A dead battery does not keep it.)
        match d.r().power_cycle(Duration::from_millis(500)).map_err(|e| e.to_string())? {
            false => Ok(format!("!Attention : sauvegarde écrite et relue dans la même session, conservation NON vérifiée (pas de coupure possible){note}")),
            true => {
                let again = read_save_data(d, mapper, data.len(), &mut |_| true)?;
                if again == data {
                    Ok(format!("Sauvegarde écrite, conservée après coupure{note}"))
                } else {
                    Err(format!("écrite mais perdue à la coupure : pile morte ?{note}"))
                }
            }
        }
    }

    fn detect(&self, dev: &mut Option<Dev>, kind: Option<zerocart_core::ReaderKind>, dmg: bool) -> Result<(ReaderView, Option<(CartView, Cart)>), String> {
        if let Some(mut d) = dev.take() {
            let _ = d.r().close(); // leaves the cartridge unpowered before reconnecting
        }
        let mut d = match kind {
            Some(k) => Dev::open_kind(k)?,
            None => Dev::open()?,
        };
        let mode = if dmg { CartMode::Dmg } else { CartMode::Agb };
        // A reader that sets the voltage must never put 5 V on a GBA cartridge: when the user chose a Game Boy, look
        // first at 3.3 V whether the cartridge has a valid GBA header, and if so do not go to 5 V.
        if dmg && !d.r().info(CartMode::Dmg).voltage.is_empty() {
            if let Ok(h) = d.r().read_header(CartMode::Agb) {
                if gba_cart_inside(&h) {
                    let info = d.r().info(CartMode::Agb);
                    let reader = ReaderView { name: info.name, firmware: info.firmware, voltage: info.voltage };
                    *dev = Some(d);
                    return Ok((reader, None));
                }
            }
        }
        let header = d.r().read_header(mode).map_err(|e| e.to_string())?;
        let info = d.r().info(mode);
        let reader = ReaderView { name: info.name, firmware: info.firmware, voltage: info.voltage };
        let db_name = if dmg { "db_DMG.json" } else { "db_AGB.json" };
        let db = Database::load(&self.db_dir.join(db_name)).ok();
        let entry = db.as_ref().and_then(|db| if dmg { db.lookup_dmg(&header) } else { db.lookup_agb(&header) }).cloned();
        let database = entry.as_ref().map(|e| format!("{} {}", e.gn, e.ne).trim().to_string());
        let found = if dmg {
            parse_dmg(&header).filter(|h| h.logo_ok).map(|h| {
                let rom_size = entry.as_ref().map(|e| e.rs as usize).unwrap_or(h.rom_size);
                let save = if h.ram_size > 0 { format!("{} ({} KiB)", h.mapper, h.ram_size / 1024) } else { h.mapper.to_string() };
                (
                    CartView {
                        system: "GB/GBC",
                        title: h.title.clone(),
                        code: h.game_code.clone().map(|c| format!("DMG-{c}")).unwrap_or_else(|| format!("sum {:04X}", h.global_checksum)),
                        rom: size_label(rom_size),
                        save,
                        database,
                        header_ok: h.header_checksum_ok,
                    },
                    Cart {
                        mode, entry, rom_size, dmg: Some((DmgMapper::from_cart_type(h.cart_type), h.global_checksum)),
                        ram_size: h.ram_size,
                        title: h.title,
                    },
                )
            })
        } else {
            parse_agb(&header).filter(|h| h.logo_ok).map(|h| {
                let rom_size = entry.as_ref().map(|e| e.rs as usize).unwrap_or(0);
                let save = entry.as_ref().map(|e| format!("{} ({} KiB)", agb_save_name(e.st), e.ss / 1024)).unwrap_or_else(|| "?".into());
                (
                    CartView {
                        system: "GBA",
                        title: h.title.clone(),
                        code: format!("AGB-{}", h.game_code),
                        rom: if rom_size > 0 { size_label(rom_size) } else { "?".into() },
                        save,
                        database,
                        header_ok: h.header_checksum_ok,
                    },
                    Cart { mode, entry, rom_size, title: h.title, dmg: None, ram_size: 0 },
                )
            })
        };
        *dev = Some(d);
        Ok((reader, found))
    }

    fn read_rom(&self, d: &mut Dev, c: &Cart, tx: &Sender<Event>) -> Result<String, String> {
        if c.rom_size == 0 {
            return Err("taille de ROM inconnue (jeu absent de la base)".into());
        }
        let name = file_stem(c);
        let ext = if c.mode == CartMode::Agb { "gba" } else { "gb" };
        let path = unique_path(&self.out_dir, &name, ext);
        std::fs::create_dir_all(&self.out_dir).map_err(|e| e.to_string())?;
        let mut out = std::io::BufWriter::new(std::fs::File::create(&path).map_err(|e| e.to_string())?);
        let started = Instant::now();
        let label = format!("Lecture ROM : {}", c.title);
        let total = c.rom_size as u64;
        let mut progress = |done: usize| {
            let kib_s = done as f32 / 1024.0 / started.elapsed().as_secs_f32().max(0.001);
            let _ = tx.send(Event::Progress { label: label.clone(), done: done as u64, total, kib_s });
            !self.cancel.load(Ordering::SeqCst)
        };
        let result: Result<(u32, Option<bool>), String> = match c.dmg {
            Some((mapper, global)) => {
                let mapper = mapper.ok_or("mapper pas encore géré")?;
                d.r().dump_dmg_rom(mapper, c.rom_size, &mut out, &mut progress)
                    .map(|s| (s.crc32, Some(s.checksum16 == global)))
                    .map_err(|e| e.to_string())
            }
            None => {
                let mut crc = Crc32::new();
                let (mut addr, block) = (0usize, 0x4000usize);
                let mut res = Ok(());
                while addr < c.rom_size {
                    let n = block.min(c.rom_size - addr);
                    match d.r().read_agb_rom(addr as u32, n) {
                        Ok(data) => {
                            crc.update(&data);
                            if let Err(e) = out.write_all(&data) {
                                res = Err(e.to_string());
                                break;
                            }
                        }
                        Err(e) => {
                            res = Err(e.to_string());
                            break;
                        }
                    }
                    addr += n;
                    if !progress(addr) {
                        res = Err("annulé".into());
                        break;
                    }
                }
                res.map(|_| (crc.finalize(), None))
            }
        };
        let (got, header_ok) = match result {
            Ok(v) => v,
            Err(e) => {
                drop(out);
                let _ = std::fs::remove_file(&path);
                return Err(if e == "cancelled" { "annulé".into() } else { e });
            }
        };
        out.flush().map_err(|e| e.to_string())?;
        drop(out);
        let file = path.file_name().unwrap_or_default().to_string_lossy().into_owned();
        match (&c.entry, header_ok) {
            (Some(e), _) if e.rc == got => Ok(format!("ROM vérifiée (CRC base OK) : {file}")),
            (Some(_), _) => Err(format!("CRC différent de la base : {file} peut être corrompu")),
            (None, Some(true)) => Ok(format!("ROM vérifiée (checksum de l'en-tête OK) : {file}")),
            (None, Some(false)) => Err(format!("checksum de l'en-tête faux : {file} peut être corrompu")),
            (None, None) => Ok(format!("ROM lue, non vérifiable : {file}")),
        }
    }

    fn read_save(&self, d: &mut Dev, c: &Cart, tx: &Sender<Event>) -> Result<String, String> {
        let size = match c.dmg {
            Some((Some(_), _)) if c.ram_size == 0 => return Err("cette cartouche n'a pas de sauvegarde".into()),
            Some((None, _)) => return Err("mapper pas encore géré".into()),
            Some(_) => c.ram_size,
            None => {
                let e = c.entry.as_ref().ok_or("jeu absent de la base : type de sauvegarde inconnu")?;
                if !matches!(e.st, 3 | 7 | 8) {
                    return Err(format!("sauvegarde « {} » : pas encore gérée", agb_save_name(e.st)));
                }
                e.ss as usize
            }
        };
        let mut reads: Vec<Vec<u8>> = Vec::new();
        let started = Instant::now();
        let mut cycled = false;
        for pass in 0..2u64 {
            if pass == 1 {
                // A battery-backed RAM keeps its data through a power cycle; one with a dead battery gives
                // different data after it, while two reads in one session always agree.
                cycled = d.r().power_cycle(Duration::from_millis(500)).map_err(|e| e.to_string())?;
            }
            let label = format!("Lecture sauvegarde (2 passes) : {}", c.title);
            let mut progress = |done: usize| {
                let done = pass * size as u64 + done as u64;
                let kib_s = done as f32 / 1024.0 / started.elapsed().as_secs_f32().max(0.001);
                let _ = tx.send(Event::Progress { label: label.clone(), done, total: 2 * size as u64, kib_s });
                !self.cancel.load(Ordering::SeqCst)
            };
            let data = match c.dmg {
                Some((Some(mapper), _)) => d.r().read_dmg_ram(mapper, size, &mut progress),
                _ => {
                    let mut data = Vec::with_capacity(size);
                    let mut res = Ok(());
                    while data.len() < size {
                        let n = 0x2000.min(size - data.len());
                        match d.r().read_agb_sram(data.len() as u32, n) {
                            Ok(b) => data.extend(b),
                            Err(e) => {
                                res = Err(e);
                                break;
                            }
                        }
                        if !progress(data.len()) {
                            res = Err(zerocart_core::Error::Cancelled);
                            break;
                        }
                    }
                    res.map(|_| data)
                }
            }
            .map_err(|e| if matches!(e, zerocart_core::Error::Cancelled) { "annulé".to_string() } else { e.to_string() })?;
            reads.push(data);
        }
        if reads[0] != reads[1] {
            let diff = reads[0].iter().zip(&reads[1]).filter(|(a, b)| a != b).count();
            return Err(if cycled {
                format!("données perdues à la coupure ({diff} octets) : pile morte ? rien enregistré")
            } else {
                format!("lecture instable ({diff} octets diffèrent) : rien enregistré")
            });
        }
        std::fs::create_dir_all(&self.out_dir).map_err(|e| e.to_string())?;
        let stem = if cycled { file_stem(c) } else { unverified_stem(&file_stem(c)) };
        let path = unique_path(&self.out_dir, &stem, "sav");
        std::fs::write(&path, &reads[0]).map_err(|e| e.to_string())?;
        let file = path.file_name().unwrap_or_default().to_string_lossy().into_owned();
        Ok(if cycled {
            format!("Sauvegarde lue (identique après coupure d'alimentation) : {file}")
        } else {
            // A leading '!' makes the application show it as a warning, not as a success.
            format!("!Attention : sauvegarde NON vérifiée, ce lecteur ne coupe pas l'alimentation (pile morte invisible) : {file}")
        })
    }
}

/// A header read at 3.3 V that is a valid GBA header (fixed byte and complement checksum): a Game Boy cartridge
/// read this way gives junk, which almost never passes both checks.
pub fn gba_cart_inside(header: &[u8]) -> bool {
    parse_agb(header).is_some_and(|h| h.logo_ok && h.header_checksum_ok)
}

/// Reads the whole save (Game Boy RAM through the mapper, or GBA SRAM). `progress(done)` returns false to cancel.
fn read_save_data(d: &mut Dev, mapper: Option<DmgMapper>, size: usize, progress: &mut dyn FnMut(usize) -> bool) -> Result<Vec<u8>, String> {
    let fail = |e: zerocart_core::Error| if matches!(e, zerocart_core::Error::Cancelled) { "annulé".to_string() } else { e.to_string() };
    match mapper {
        Some(m) => d.r().read_dmg_ram(m, size, progress).map_err(fail),
        None => {
            let mut data = Vec::with_capacity(size);
            while data.len() < size {
                let n = 0x2000.min(size - data.len());
                data.extend(d.r().read_agb_sram(data.len() as u32, n).map_err(fail)?);
                if !progress(data.len()) {
                    return Err("annulé".into());
                }
            }
            Ok(data)
        }
    }
}

/// Writes the whole save (Game Boy RAM through the mapper, or GBA SRAM).
fn write_save_data(d: &mut Dev, mapper: Option<DmgMapper>, data: &[u8], progress: &mut dyn FnMut(usize) -> bool) -> Result<(), String> {
    let fail = |e: zerocart_core::Error| if matches!(e, zerocart_core::Error::Cancelled) { "annulé".to_string() } else { e.to_string() };
    match mapper {
        Some(m) => d.r().write_dmg_ram(m, data, progress).map_err(fail),
        None => d.r().write_agb_sram(data, progress).map_err(fail),
    }
}

/// File name stem of a save whose retention could not be checked (the reader cannot cut the power), so it is not
/// mistaken for a real, verified save.
fn unverified_stem(stem: &str) -> String {
    format!("{stem} (non vérifiée)")
}

fn size_label(bytes: usize) -> String {
    if bytes >= 1 << 20 { format!("{} MiB", bytes >> 20) } else { format!("{} KiB", bytes >> 10) }
}

fn file_stem(c: &Cart) -> String {
    let base = match &c.entry {
        Some(e) => format!("{} {}", e.gn, e.ne).trim().to_string(),
        None => c.title.clone(),
    };
    sanitize(&base)
}

pub fn sanitize(s: &str) -> String {
    let t: String = s.chars().map(|c| if c.is_control() || "/\\:*?\"<>|".contains(c) { '_' } else { c }).collect();
    let t = t.trim().trim_matches('.').to_string();
    if t.is_empty() { "cartouche".into() } else { t }
}

/// `dir/name.ext`, or `name (2).ext`... : an existing file is never overwritten.
pub fn unique_path(dir: &Path, name: &str, ext: &str) -> PathBuf {
    let mut p = dir.join(format!("{name}.{ext}"));
    let mut n = 2;
    while p.exists() {
        p = dir.join(format!("{name} ({n}).{ext}"));
        n += 1;
    }
    p
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_safe_and_never_overwrite() {
        assert_eq!(sanitize("Mr. Driller 2: (Europe)"), "Mr. Driller 2_ (Europe)");
        assert_eq!(sanitize("../.."), "_");
        assert_eq!(unverified_stem("Game"), "Game (non vérifiée)");
        let d = tempfile::tempdir().unwrap();
        let a = unique_path(d.path(), "Game", "gba");
        std::fs::write(&a, b"x").unwrap();
        assert_eq!(unique_path(d.path(), "Game", "gba").file_name().unwrap(), "Game (2).gba");
    }
}

#[cfg(test)]
mod guard_tests {
    use super::*;

    fn gba_header() -> Vec<u8> {
        let mut h = vec![0u8; 0x180];
        h[0xA0..0xA8].copy_from_slice(b"TESTGAME");
        h[0xB2] = 0x96;
        let sum = h[0xA0..=0xBC].iter().fold(0u8, |a, &b| a.wrapping_sub(b)).wrapping_sub(0x19);
        h[0xBD] = sum;
        h
    }

    #[test]
    fn a_valid_gba_header_is_recognised_so_that_5_v_is_not_applied() {
        assert!(gba_cart_inside(&gba_header()));
    }

    #[test]
    fn junk_and_game_boy_data_read_at_3_3_v_are_not_taken_for_a_gba_cartridge() {
        assert!(!gba_cart_inside(&vec![0xFF; 0x180]));
        assert!(!gba_cart_inside(&vec![0u8; 0x180]));
        let mut half = gba_header();
        half[0xBD] ^= 1; // fixed byte right, checksum wrong
        assert!(!gba_cart_inside(&half));
        let mut other = gba_header();
        other[0xB2] = 0x00; // checksum right for the old bytes, fixed byte wrong
        assert!(!gba_cart_inside(&other));
        assert!(!gba_cart_inside(&[1, 2, 3]), "a short read is not a header");
    }
}
