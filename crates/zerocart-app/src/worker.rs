//! The thread that talks to the reader. LVGL-style rule: the UI thread never blocks on USB; it sends commands
//! and receives events over channels.
use zerocart_core::db::{agb_save_name, Crc32, Database, Entry};
use zerocart_core::header::{parse_agb, parse_dmg};
use zerocart_core::mapper::DmgMapper;
use zerocart_core::chis::{self, Chis};
use zerocart_core::transport::SerialTransport;
use zerocart_core::{find_ports, CartMode, CartReader, GbxCart};
use zerocart_flashchips::FlashBus;
use crate::msg;
use zerocart_ui::{loc, CartView, Lang, ReaderView};
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
    /// The ROM to write and whether to back up what it overwrites first (the user can go without).
    FlashRom(PathBuf, bool),
    /// Game Boy: write the save file to the cartridge RAM (the current save is backed up first, if it is stable).
    WriteSave(PathBuf),
    /// Serial flash mode: erase, write and verify the ROM on the cartridge, without a backup (the cartridges are
    /// blank), then power it down so that it can be taken out. Answered by `Event::BatchResult`.
    BatchFlash { path: PathBuf, dmg: bool },
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
    /// `Ok(true)`: a flash cartridge answers; `Ok(false)`: the connector is empty; `Err`: something is in it that must
    /// not be flashed in this mode (a Game Boy Advance cartridge while Game Boy was chosen).
    /// Serial flash mode: the cartridge was not started (no chip answers, wrong cartridge): nothing was touched
    BatchSkipped(String),
    BatchResult(Result<String, String>),
}

/// The Krikzz programmer and the header of the cartridge in it.
struct MdState {
    kit: zerocart_core::krikzz::FlashKit<SerialTransport>,
    header: Option<zerocart_core::megadrive::MdHeader>,
    /// Whether the chip can be written above 0x400000: `None` until it was tested (the test is made when an image
    /// that big is written), `Some(false)` on a cartridge wired as a 4 MiB one.
    upper: Option<bool>,
}

/// The cartridge taken as blank: no header to read, so the system comes from the mode, a Game Boy cartridge is taken as
/// an MBC5 one (the only mapper that can be flashed) and its title from the ROM about to be written.
fn blank_cart(dmg: bool, rom: Option<&Path>) -> Cart {
    if dmg {
        let title = rom.and_then(|p| std::fs::read(p).ok()).and_then(|r| parse_dmg(&r).map(|h| h.title)).unwrap_or_default();
        Cart { mode: CartMode::Dmg, entry: None, rom_size: 0, title, dmg: Some((Some(DmgMapper::Mbc5), 0)), ram_size: 0 }
    } else {
        Cart { mode: CartMode::Agb, entry: None, rom_size: 0, title: String::new(), dmg: None, ram_size: 0 }
    }
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
    /// The language of the messages, switched by the user on the start screen.
    pub lang: Arc<std::sync::atomic::AtomicU8>,
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

    /// The cartridge bus as the flash profiles of GBA cartridges see it (the Chisflash needs its own ROM-bus commands).
    fn agb_bus(&mut self) -> Box<dyn FlashBus + '_> {
        match self {
            Dev::Gbx(d) => Box::new(d),
            Dev::Chis(d) => Box::new(zerocart_flashchips::chis_agb::AgbBus(d)),
        }
    }

    /// The reader of this kind (the Krikzz Mega Drive programmer is not a cartridge reader of this application yet).
    fn open_kind(kind: zerocart_core::ReaderKind, l: Lang) -> Result<Dev, String> {
        use zerocart_core::ReaderKind;
        match kind {
            ReaderKind::GbxCart => {
                let port = find_ports().into_iter().next().ok_or_else(|| msg::not_found(l, "GBFlash / GBxCart RW"))?;
                let t = SerialTransport::open(&port, 1_000_000).map_err(|e| e.to_string())?;
                GbxCart::connect(t, Duration::from_millis(200)).map(Dev::Gbx).map_err(|e| msg::no_answer(l, &e.to_string()))
            }
            ReaderKind::Chisflash => {
                let port = chis::find_ports().into_iter().next().ok_or_else(|| msg::not_found(l, "Chisflash"))?;
                let t = SerialTransport::open(&port, chis::BAUD).map_err(|e| e.to_string())?;
                Chis::new(t)
                    .map(|mut c| {
                        let _ = c.probe_boost(); // the Boost firmware says so; the original one stays silent
                        Dev::Chis(c)
                    })
                    .map_err(|e| msg::no_answer(l, &e.to_string()))
            }
            other => Err(msg::soon(l, other.label())),
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
    fn lang(&self) -> Lang {
        Lang::from_index(self.lang.load(Ordering::SeqCst))
    }

    pub fn run(self, rx: Receiver<Cmd>, tx: Sender<Event>) {
        let mut dev: Option<Dev> = None;
        let mut cart: Option<Cart> = None;
        let mut kind: Option<zerocart_core::ReaderKind> = None;
        // The mode of the last look at the cartridge: a blank one has no header to say which system it is.
        let mut dmg_mode = true;
        // The Krikzz Mega Drive programmer is not a CartReader: it has its own state and commands.
        let mut md: Option<MdState> = None;
        while let Ok(cmd) = rx.recv() {
            self.cancel.store(false, Ordering::SeqCst);
            if let Some(state) = md.as_mut() {
                if matches!(cmd, Cmd::Detect { .. } | Cmd::ReadRom | Cmd::ReadSave | Cmd::Analyze | Cmd::FlashRom(..) | Cmd::WriteSave(_) | Cmd::BatchFlash { .. }) {
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
                    // a blank cartridge has no header: the chip is what is looked at
                    let r = match dev.as_mut() {
                        Some(d) => match cart.as_ref() {
                            Some(c) => self.analyze(d, c),
                            None => self.analyze(d, &blank_cart(dmg_mode, None)),
                        },
                        None => Err(msg::no_cartridge(self.lang())),
                    };
                    let _ = tx.send(Event::Analysis(r));
                }
                Cmd::FlashRom(path, backup) => {
                    // A blank cartridge (or one a failed flash left blank) has no header: flashing only needs the chip
                    // to answer, and the flash code looks at it itself. A Game Boy cartridge is taken as an MBC5 one.
                    let r = match dev.as_mut() {
                        Some(d) => match cart.as_ref() {
                            Some(c) => self.flash_rom(d, c, &path, backup, &tx),
                            None => self.flash_rom(d, &blank_cart(dmg_mode, Some(&path)), &path, backup, &tx),
                        },
                        None => Err(msg::no_cartridge(self.lang())),
                    };
                    self.after_write(&mut dev, kind, &mut cart, dmg_mode, r, &tx);
                }
                Cmd::WriteSave(path) => {
                    let r = match (dev.as_mut(), cart.as_ref()) {
                        (Some(d), Some(c)) => self.write_save(d, c, &path, &tx),
                        _ => Err(msg::no_cartridge(self.lang())),
                    };
                    self.after_write(&mut dev, kind, &mut cart, dmg_mode, r, &tx);
                }
                Cmd::BatchFlash { path, dmg } => {
                    let r = self.batch_run(&mut dev, kind, &path, dmg, &tx);
                    let _ = tx.send(r);
                }
                Cmd::Quit => {
                    if let Some(d) = dev.as_mut() {
                        let _ = d.r().close();
                    }
                    return;
                }
                Cmd::Detect { dmg } => {
                    dmg_mode = dmg;
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
                        _ => Err(msg::no_cartridge(self.lang())),
                    };
                    let _ = tx.send(Event::Finished(r));
                }
                Cmd::ReadSave => {
                    let r = match (dev.as_mut(), cart.as_ref()) {
                        (Some(d), Some(c)) => self.read_save(d, c, &tx),
                        _ => Err(msg::no_cartridge(self.lang())),
                    };
                    let _ = tx.send(Event::Finished(r));
                }
            }
        }
    }

    /// Opens the Krikzz programmer (the handshake refuses any other device) and keeps it for the commands that follow.
    fn md_connect(&self) -> Result<(zerocart_core::krikzz::FlashKit<SerialTransport>, ReaderView), String> {
        let l = self.lang();
        let port = zerocart_core::krikzz::find_ports().into_iter().next().ok_or_else(|| msg::not_found(l, "MD Flasher"))?;
        let t = SerialTransport::open(&port, 9600).map_err(|e| format!("{port} : {e}"))?;
        let kit = zerocart_core::krikzz::FlashKit::connect(t).map_err(|e| msg::md_board_no_answer(l, &e.to_string()))?;
        Ok((kit, ReaderView { name: "MD Flasher".into(), firmware: String::new(), voltage: "" }))
    }

    /// The commands of the Mega Drive system. Reading only: writing is refused until it is wired with its safeguards.
    fn md_command(&self, md: &mut MdState, cmd: Cmd, tx: &Sender<Event>) {
        match cmd {
            Cmd::Detect { .. } => {
                let r = self.md_detect(md);
                let _ = tx.send(Event::Detected(r.map(|view| (ReaderView { name: "MD Flasher".into(), firmware: String::new(), voltage: "" }, view))));
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
            Cmd::FlashRom(path, backup) => {
                let r = self.md_flash_rom(md, &path, backup, tx);
                // the cartridge changed (or the attempt may have): read its header again, then report
                if let Ok(view) = self.md_detect(md) {
                    let reader = ReaderView { name: "MD Flasher".into(), firmware: String::new(), voltage: "" };
                    let _ = tx.send(Event::Detected(Ok((reader, view))));
                }
                let _ = tx.send(Event::Finished(r));
            }
            Cmd::WriteSave(path) => {
                let r = self.md_write_save(md, &path, tx);
                let _ = tx.send(Event::Finished(r));
            }
            Cmd::BatchFlash { path, .. } => match md.kit.read_geometry() {
                Ok(Some(_)) => {
                    let r = self.md_flash_rom(md, &path, false, tx);
                    let _ = tx.send(Event::BatchResult(r));
                }
                Ok(None) => {
                    let _ = tx.send(Event::BatchSkipped(msg::batch_no_chip(self.lang())));
                }
                Err(e) => {
                    let _ = tx.send(Event::BatchSkipped(e.to_string()));
                }
            },
            _ => {
                let _ = tx.send(Event::Finished(Err(msg::md_save_write_unsupported(self.lang()))));
            }
        }
    }

    /// Writes a save file to the save RAM of a Mega Drive cartridge: the file is a word stream (`2 * size` bytes) or the
    /// plain data (`size` bytes); a RAM must answer; the current save is kept first (two reads that agree) and the
    /// result is read back. The reader cannot cut the power, so the result is always a warning ("not verified").
    fn md_write_save(&self, md: &mut MdState, path: &Path, tx: &Sender<Event>) -> Result<String, String> {
        let l = self.lang();
        let h = md.header.clone().ok_or_else(|| msg::no_cartridge(l))?;
        let ram = h.ram.ok_or_else(|| msg::md_no_save_declared(l))?;
        let file = std::fs::read(path).map_err(|e| e.to_string())?;
        let stream = zerocart_core::megadrive::save_file_to_stream(&file, ram.size()).ok_or_else(|| msg::save_size_mismatch(l, file.len(), ram.size()))?;
        if !md.kit.ram_present().map_err(|e| e.to_string())? {
            return Err(msg::md_no_sram(l));
        }
        let progress = |label: &str, done: usize, total: usize| {
            let _ = tx.send(Event::Progress { label: label.to_string(), done: done as u64, total: total as u64, kib_s: 0.0 });
        };
        // 1. keep the current save when it reads the same twice
        progress(msg::backing_up_old_save(l), 0, 3);
        let a = md.kit.read_save_stream(ram.size()).map_err(|e| e.to_string())?;
        progress(msg::backing_up_old_save(l), 1, 3);
        let b = md.kit.read_save_stream(ram.size()).map_err(|e| e.to_string())?;
        let note = if a == b {
            let dir = self.out_dir.join("backups");
            std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
            let title = if h.title().is_empty() { "megadrive".to_string() } else { sanitize(h.title()) };
            let backup = unique_path(&dir, &unverified_stem(&format!("{title} avant écriture")), "sav");
            std::fs::write(&backup, &a).map_err(|e| e.to_string())?;
            msg::note_old_save(l, &backup.file_name().unwrap_or_default().to_string_lossy())
        } else {
            msg::note_old_save_unstable(l)
        };
        // 2. write, then read back in the same session
        progress(msg::writing_save(l), 2, 3);
        md.kit.write_save_stream(&stream).map_err(|e| format!("{e}{note}"))?;
        let back = md.kit.read_save_stream(ram.size()).map_err(|e| e.to_string())?;
        if zerocart_core::megadrive::stream_to_data(&back) != zerocart_core::megadrive::stream_to_data(&stream) {
            return Err(msg::save_readback_differs(l, &note));
        }
        Ok(msg::save_written_unverified(l, &note))
    }

    /// Writes a ROM file to the flash cartridge: checks, backup of what is overwritten (two reads that must agree),
    /// programming of the sectors that differ, then a full read back. From the programming on it cannot be cancelled.
    fn md_flash_rom(&self, md: &mut MdState, path: &Path, backup: bool, tx: &Sender<Event>) -> Result<String, String> {
        let l = self.lang();
        use zerocart_core::krikzz::UPPER_HALF;
        let cancelled = |e: zerocart_core::Error| if matches!(e, zerocart_core::Error::Cancelled) { msg::cancelled(l) } else { e.to_string() };
        let image = std::fs::read(path).map_err(|e| e.to_string())?;
        if image.len() < 0x200 || image.len() % 2 != 0 {
            return Err(msg::md_bad_size(l));
        }
        let file_header = zerocart_core::megadrive::parse_header(&image).filter(|h| h.sega_ok);
        let Some(file_header) = file_header else {
            return Err(msg::md_not_a_rom(l));
        };
        let geo = md
            .kit
            .read_geometry()
            .map_err(|e| e.to_string())?
            .ok_or_else(|| msg::chip_not_recognised_md(l))?;
        if geo.command_set != 2 {
            return Err(msg::md_only_amd(l));
        }
        if image.len() > geo.size_bytes {
            return Err(msg::rom_exceeds_chip(l, &size_label(image.len()), &size_label(geo.size_bytes)));
        }
        // Above 4 MiB the chip is not always reachable: a standard cartridge is wired for 4 MiB only. Say so instead of
        // failing half way (the test leaves the cartridge as it was).
        if image.len() > UPPER_HALF {
            let _ = tx.send(Event::Progress { label: msg::md_testing_upper(l), done: 0, total: 0, kib_s: 0.0 });
            let ok = md.kit.upper_half_writable(&geo).map_err(|e| e.to_string())?;
            md.upper = Some(ok);
            if !ok {
                return Err(msg::md_limited_4mb(l, &size_label(image.len())));
            }
        }
        let extent = geo.extent(image.len());
        let started = Instant::now();

        // 1. Back up what will be overwritten: two reads that must agree.
        let mut passes: Vec<Vec<u8>> = Vec::new();
        for pass in 0..if backup { 2usize } else { 0 } {
            let mut v = Vec::with_capacity(extent);
            md.kit
                .dump_rom(extent, &mut v, &mut |done| {
                    let total = 2 * extent;
                    let done = pass * extent + done;
                    let kib_s = done as f32 / 1024.0 / started.elapsed().as_secs_f32().max(0.001);
                    let _ = tx.send(Event::Progress { label: msg::backup_cart_progress(l), done: done as u64, total: total as u64, kib_s });
                    !self.cancel.load(Ordering::SeqCst)
                })
                .map_err(cancelled)?;
            passes.push(v);
        }
        if backup && passes[0] != passes[1] {
            return Err(msg::reads_differ(l));
        }
        let name = if backup {
            let backups = self.out_dir.join("backups");
            std::fs::create_dir_all(&backups).map_err(|e| e.to_string())?;
            let title = if file_header.title().is_empty() { "megadrive".to_string() } else { sanitize(file_header.title()) };
            let file = unique_path(&backups, &format!("{title} avant flash"), "bin");
            std::fs::write(&file, &passes[0]).map_err(|e| e.to_string())?;
            Some(file.file_name().unwrap_or_default().to_string_lossy().into_owned())
        } else {
            None
        };

        // 2. Erase and program the sectors that differ. From here on it cannot be cancelled.
        let t1 = Instant::now();
        let mut on_progress = |msg: &str, done: usize, total: usize| {
            let bytes: usize = geo.sectors.iter().take(done).map(|s| s.1 as usize).sum();
            let _ = tx.send(Event::Progress {
                label: msg::writing_sectors(l, done, total, msg),
                done: done as u64,
                total: total as u64,
                kib_s: bytes as f32 / 1024.0 / t1.elapsed().as_secs_f32().max(0.001),
            });
        };
        let report = md
            .kit
            .flash_rom(&geo, &image, &mut on_progress, &|| false)
            .map_err(|e| msg::old_rom_in(l, &e.to_string(), name.as_deref()))?;

        // 3. Read everything back.
        let mut after = Vec::with_capacity(extent);
        md.kit
            .dump_rom(extent, &mut after, &mut |done| {
                let _ = tx.send(Event::Progress { label: msg::verifying(l), done: done as u64, total: extent as u64, kib_s: 0.0 });
                true
            })
            .map_err(|e| e.to_string())?;
        let mut want = image.clone();
        want.resize(extent, 0xFF);
        if after == want {
            return Ok(msg::md_written_ok(l, report.rewritten, report.unchanged, name.as_deref()));
        }
        // Say what differs: a second read tells a bad write (same bytes again) from a bad contact (other bytes).
        let first = after.iter().zip(&want).position(|(a, w)| a != w).unwrap_or(0);
        let count = after.iter().zip(&want).filter(|(a, w)| a != w).count();
        let mut again = Vec::with_capacity(extent);
        md.kit.dump_rom(extent, &mut again, &mut |_| true).map_err(|e| e.to_string())?;
        if again == want {
            return Ok(msg::written_reread_ok(l, first, count));
        }
        let (want_b, got_b) = (want[first], after[first]);
        Err(msg::written_diff_detail(l, count, first, want_b, got_b, again == after))
    }

    /// Reads the "SEGA" header of the inserted cartridge. `None`: no valid Mega Drive header (empty connector, blank
    /// flash cartridge, another system).
    fn md_detect(&self, md: &mut MdState) -> Result<Option<CartView>, String> {
        let l = self.lang();
        let header = md.kit.read_header().map_err(|e| e.to_string())?;
        md.header = header.clone().filter(|h| h.is_valid());
        Ok(md.header.as_ref().map(|h| {
            let save = match h.ram {
                Some(r) => format!(
                    "{} {}{}",
                    r.size() / 1024,
                    msg::ko(l),
                    if r.odd_bytes { loc!(l, fr: " (octets impairs)", en: " (odd bytes)", ja: "（奇数バイト）", zh: "（奇数字节）") } else { "" }
                ),
                None => loc!(l, fr: "aucune", en: "none", ja: "なし", zh: "无").into(),
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
        let l = self.lang();
        let h = md.header.clone().ok_or_else(|| msg::no_md_cartridge(l))?;
        let size = h.rom_size().ok_or_else(|| msg::md_unknown_size(l))?;
        std::fs::create_dir_all(&self.out_dir).map_err(|e| e.to_string())?;
        let title = if h.title().is_empty() { "megadrive".to_string() } else { sanitize(h.title()) };
        let path = unique_path(&self.out_dir, &title, h.extension());
        let mut out = std::io::BufWriter::new(std::fs::File::create(&path).map_err(|e| e.to_string())?);
        let started = Instant::now();
        let label = msg::reading_rom(l, h.title());
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
                return Err(if matches!(e, zerocart_core::Error::Cancelled) { msg::cancelled(l) } else { e.to_string() });
            }
        };
        out.flush().map_err(|e| e.to_string())?;
        let file = path.file_name().unwrap_or_default().to_string_lossy().into_owned();
        Ok(if dump.checksum16 == h.checksum {
            msg::md_rom_read_ok(l, &file, dump.crc32)
        } else {
            // Homebrew and some repros carry a wrong checksum in their header: the CRC32 is the real proof.
            msg::md_rom_read_checksum_differs(l, &file, dump.crc32)
        })
    }

    fn md_read_save(&self, md: &mut MdState, tx: &Sender<Event>) -> Result<String, String> {
        let l = self.lang();
        let h = md.header.clone().ok_or_else(|| msg::no_md_cartridge(l))?;
        let ram = h.ram.ok_or_else(|| msg::md_no_save_declared(l))?;
        let _ = tx.send(Event::Progress { label: msg::reading_save_progress(l), done: 0, total: ram.size() as u64, kib_s: 0.0 });
        let stream = md.kit.read_save_stream(ram.size()).map_err(|e| e.to_string())?;
        std::fs::create_dir_all(&self.out_dir).map_err(|e| e.to_string())?;
        let title = if h.title().is_empty() { "megadrive".to_string() } else { sanitize(h.title()) };
        // The file keeps the bus's word stream (data on the odd bytes), as the reference client saves it.
        let path = unique_path(&self.out_dir, &format!("{title} (non vérifiée)"), "sav");
        std::fs::write(&path, &stream).map_err(|e| e.to_string())?;
        let file = path.file_name().unwrap_or_default().to_string_lossy().into_owned();
        Ok(msg::unverified_save(l, &file))
    }

    /// Read-only identification of the flash chip (CFI query, identifiers, sectors).
    fn md_analyze(&self, md: &mut MdState) -> Result<Vec<String>, String> {
        let l = self.lang();
        let mut lines = vec![msg::md_board(l)];
        match md.kit.probe_flash().map_err(|e| e.to_string())? {
            None => {
                lines.extend(msg::no_flash_answers(l));
            }
            Some(f) => {
                lines.push(msg::cfi_answers(l, f.cfi_ok));
                if f.size_bytes > 0 {
                    lines.push(msg::chip_size(l, f.size_bytes / (1 << 20)));
                }
                let ids = f.device_id.iter().map(|d| format!("{d:04X}")).collect::<Vec<_>>().join("/");
                lines.push(msg::maker_and_chip(l, f.manufacturer, &ids));
                if f.byte_wide {
                    lines.push(msg::byte_wide_pair(l));
                }
                match md.upper {
                    Some(true) => lines.push(msg::upper_writable(l)),
                    Some(false) => lines.push(msg::upper_dead(l)),
                    None if f.size_bytes > 4 << 20 => lines.push(msg::upper_untested(l)),
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
                    let desc: Vec<String> = runs.iter().map(|(sz, n)| format!("{n} × {} {}", sz / 1024, msg::ko(l))).collect();
                    lines.push(msg::sectors_line(l, &desc.join(" + ")));
                }
            }
        }
        Ok(lines)
    }

    /// Opens the reader of `kind` and asks it who it is. The Krikzz programmer is only handshaken (nothing is read
    /// from a cartridge yet) and closed again.
    fn connect(&self, kind: zerocart_core::ReaderKind) -> Result<(Option<Dev>, ReaderView), String> {
        let l = self.lang();
        use zerocart_core::ReaderKind;
        if kind == ReaderKind::KrikzzMd {
            let port = zerocart_core::krikzz::find_ports().into_iter().next().ok_or_else(|| msg::not_found(l, "MD Flasher"))?;
            let t = SerialTransport::open(&port, 9600).map_err(|e| format!("{port} : {e}"))?;
            zerocart_core::krikzz::FlashKit::connect(t).map_err(|e| msg::md_board_no_answer(l, &e.to_string()))?;
            return Ok((None, ReaderView { name: "MD Flasher".into(), firmware: String::new(), voltage: "" }));
        }
        let mut d = Dev::open_kind(kind, l)?;
        let info = d.r().info(CartMode::Agb);
        let view = ReaderView { name: info.name, firmware: info.firmware, voltage: info.voltage };
        Ok((Some(d), view))
    }

    /// Serial flash mode, one cartridge: look for the flash chip (a blank cartridge has no header, so the chip answering
    /// is the only proof of a cartridge), and only then erase, write and verify. Without an answer nothing is touched
    /// and `BatchSkipped` says why. The cartridge is unpowered afterwards.
    fn batch_run(&self, dev: &mut Option<Dev>, kind: Option<zerocart_core::ReaderKind>, path: &Path, dmg: bool, tx: &Sender<Event>) -> Event {
        let l = self.lang();
        let opened = match dev.take() {
            Some(d) => Ok(d),
            None => kind.ok_or_else(|| msg::no_cartridge(l)).and_then(|k| Dev::open_kind(k, l)),
        };
        let mut d = match opened {
            Ok(d) => d,
            Err(e) => return Event::BatchSkipped(e),
        };
        let event = match self.batch_check(&mut d, dmg) {
            Ok(true) => Event::BatchResult(self.batch_flash(&mut d, path, dmg, tx)),
            Ok(false) => Event::BatchSkipped(msg::batch_no_chip(l)),
            Err(e) => Event::BatchSkipped(e),
        };
        let _ = d.r().close(); // the cartridge is unpowered: it can be taken out
        event
    }

    fn batch_check(&self, d: &mut Dev, dmg: bool) -> Result<bool, String> {
        use zerocart_flashchips::{detect, load_dir};
        // Same guard as the single-cartridge mode: a reader that sets the voltage looks at 3.3 V first, and never
        // puts 5 V on a Game Boy Advance cartridge.
        if dmg && !d.r().info(CartMode::Dmg).voltage.is_empty() {
            if let Ok(h) = d.r().read_header(CartMode::Agb) {
                if gba_cart_inside(&h) {
                    return Err(msg::batch_gba_inside(self.lang()));
                }
            }
        }
        let mode = if dmg { CartMode::Dmg } else { CartMode::Agb };
        d.r().read_header(mode).map_err(|e| e.to_string())?;
        let (profiles, _) = load_dir(&self.db_dir);
        if profiles.is_empty() {
            return Err(msg::profiles_missing(self.lang(), &self.db_dir.display().to_string()));
        }
        let found = if dmg { detect(d.bus(), &profiles, "DMG")? } else { detect(&mut *d.agb_bus(), &profiles, "AGB")? };
        Ok(found.cfi.is_some())
    }

    /// Serial flash mode: the cartridge is taken as blank (no backup). A Game Boy cartridge is taken as an MBC5 one,
    /// the only mapper that can be flashed, since a blank chip has no header to say so.
    fn batch_flash(&self, d: &mut Dev, path: &Path, dmg: bool, tx: &Sender<Event>) -> Result<String, String> {
        let cart = blank_cart(dmg, Some(path));
        self.flash_rom(d, &cart, path, false, tx)
    }

    /// After a write the cartridge may have changed: identify it again, then report the result.
    fn after_write(&self, dev: &mut Option<Dev>, kind: Option<zerocart_core::ReaderKind>, cart: &mut Option<Cart>, dmg: bool, result: Result<String, String>, tx: &Sender<Event>) {
        if let Ok((reader, found)) = self.detect(dev, kind, dmg) {
            let view = found.as_ref().map(|(v, _)| v.clone());
            *cart = found.map(|(_, c)| c);
            let _ = tx.send(Event::Detected(Ok((reader, view))));
        }
        let _ = tx.send(Event::Finished(result));
    }

    /// Identifies the flash chip and reads its CFI table, as a list of short lines for the Tools tab.
    fn analyze(&self, d: &mut Dev, c: &Cart) -> Result<Vec<String>, String> {
        let l = self.lang();
        use zerocart_flashchips::{agb, detect, load_dir};
        let agb_mode = c.mode == CartMode::Agb;
        let (profiles, _) = load_dir(&self.db_dir);
        if profiles.is_empty() {
            return Err(msg::profiles_missing(l, &self.db_dir.display().to_string()));
        }
        let found = if agb_mode { detect(&mut *d.agb_bus(), &profiles, "AGB")? } else { detect(d.bus(), &profiles, "DMG")? };
        let volts = matches!(d, Dev::Gbx(_));
        let mut lines = vec![msg::mode_line(l, agb_mode, volts)];
        if found.id_lines.is_empty() && found.cfi.is_none() {
            lines.extend(msg::no_flash_answers(l));
            return Ok(lines);
        }
        for line in found.id_lines.iter().take(3) {
            lines.push(msg::chip_answer(l, line));
        }
        match found.matches.as_slice() {
            [] => {
                let probable = if agb_mode {
                    agb::choose_profile(&mut *d.agb_bus(), &profiles, &found.matches, &found.answered).ok()
                } else {
                    None
                };
                match probable {
                    Some(i) => {
                        lines.push(msg::no_exact_profile(l));
                        lines.push(format!("  {}", profiles[i].name));
                    }
                    None => lines.push(msg::no_known_profile(l)),
                }
            }
            m => {
                lines.push(msg::profile_recognised(l, m.len() == 1));
                for &i in m.iter().take(3) {
                    lines.push(format!("  {}", profiles[i].name));
                }
                if m.len() > 3 {
                    lines.push(msg::more_profiles(l, m.len() - 3));
                }
            }
        }
        if let Some(w) = found.we_pin {
            lines.push(msg::write_pin(l, w));
        }
        match &found.cfi {
            None => lines.push(msg::cfi_unreadable(l)),
            Some(cfi) => {
                lines.push(msg::cfi_summary(l, cfi.device_size / (1 << 20), cfi.vdd_min as f64, cfi.vdd_max as f64));
                lines.push(msg::buffer_write(l, cfi.buffer_size.map(|b| b as usize)));
                let runs: Vec<String> = cfi
                    .sector_runs()
                    .iter()
                    .map(|(size, n)| format!("{n} × {} {}", size / 1024, msg::ko(l)))
                    .collect();
                lines.push(msg::sectors_line(l, &runs.join(" + ")));
                if !cfi.swapped.is_empty() {
                    let pins: Vec<String> = cfi.swapped.iter().map(|(a, b)| format!("D{a}↔D{b}")).collect();
                    lines.push(msg::crossed_data_lines(l, &pins.join(", ")));
                }
            }
        }
        Ok(lines)
    }

    /// GBA: erase the whole flash chip and write the ROM (profile chosen from what the chip answers), after a
    /// backup of the whole chip made of two reads that must agree.
    fn flash_rom_agb(&self, d: &mut Dev, path: &Path, backup: bool, tx: &Sender<Event>) -> Result<String, String> {
        let l = self.lang();
        use zerocart_flashchips::{agb, detect, load_dir, program};
        let rom = std::fs::read(path).map_err(|e| e.to_string())?;
        if !parse_agb(&rom).is_some_and(|h| h.logo_ok && h.header_checksum_ok) {
            return Err(msg::gba_bad_file(l));
        }
        let (profiles, _) = load_dir(&self.db_dir);
        let found = detect(&mut *d.agb_bus(), &profiles, "AGB")?;
        let cfi = found.cfi.as_ref().ok_or_else(|| msg::chip_not_identified(l))?;
        if rom.len() as u64 > cfi.device_size {
            return Err(msg::rom_exceeds_chip(l, &format!("{} {}", rom.len() / 1024, msg::ko(l)), &format!("{} {}", cfi.device_size / 1024, msg::ko(l))));
        }
        let idx = agb::choose_profile(&mut *d.agb_bus(), &profiles, &found.matches, &found.answered)?;
        let profile = &profiles[idx];
        let chip = cfi.device_size as usize;
        let started = Instant::now();

        // 1. Back up the whole chip (it is all erased): two reads that must agree.
        let name = if backup {
            let mut pass = 0u64;
            let current = program::read_until_stable(|| {
                let base = pass * chip as u64;
                pass += 1;
                let mut v = Vec::with_capacity(chip);
                while v.len() < chip {
                    if self.cancel.load(Ordering::SeqCst) {
                        return Err(msg::cancelled(l));
                    }
                    let n = 0x4000.min(chip - v.len());
                    let at = v.len() as u32;
                    v.extend(match d {
                        Dev::Gbx(g) => g.read_rom(at, n),
                        Dev::Chis(c) => c.read_rom(at, n.min(chis::CHUNK)),
                    }
                    .map_err(|e| e.to_string())?);
                    let done = base + v.len() as u64;
                    let _ = tx.send(Event::Progress {
                        label: msg::backup_chip_progress(l),
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
            Some(backup.file_name().unwrap_or_default().to_string_lossy().into_owned())

        } else {
            None
        };

        // 2. Erase and write. From here on the transfer cannot be cancelled.
        let t1 = Instant::now();
        let mut on_progress = |done: usize, total: usize, m: &str| {
            let _ = tx.send(Event::Progress {
                label: msg::writing_gba(l, m),
                done: done as u64,
                total: total as u64,
                kib_s: done as f32 / 1024.0 / t1.elapsed().as_secs_f32().max(0.001),
            });
        };
        let report = match d {
            Dev::Gbx(g) => agb::flash_rom(g, profile, &rom, &mut on_progress),
            Dev::Chis(c) => zerocart_flashchips::chis_agb::flash_rom(c, profile, &rom, &mut on_progress),
        }
        .map_err(|e| msg::old_chip_in(l, &e.to_string(), name.as_deref()))?;
        Ok(msg::gba_written_ok(l, report.attempts as usize, report.repaired as usize, name.as_deref()))
    }

    fn flash_rom(&self, d: &mut Dev, c: &Cart, path: &Path, backup: bool, tx: &Sender<Event>) -> Result<String, String> {
        let l = self.lang();
        use zerocart_flashchips::{detect, load_dir, program};
        let mapper = match c.dmg {
            Some((Some(m), _)) if m == DmgMapper::Mbc5 => m,
            Some(_) => return Err(msg::only_mbc5(l)),
            None => {
                return self.flash_rom_agb(d, path, backup, tx);
            }
        };
        if matches!(d, Dev::Chis(_)) && mapper != DmgMapper::Mbc5 {
            return Err(msg::only_mbc5(l));
        }
        let rom = std::fs::read(path).map_err(|e| e.to_string())?;
        if !rom.len().is_power_of_two() || rom.len() < 0x8000 {
            return Err(msg::gb_bad_size(l));
        }
        if !parse_dmg(&rom).is_some_and(|h| h.logo_ok) {
            return Err(msg::gb_bad_file(l));
        }
        let (profiles, _) = load_dir(&self.db_dir);
        let found = detect(d.bus(), &profiles, "DMG")?;
        let (Some(cfi), [idx]) = (found.cfi.as_ref(), found.matches.as_slice()) else {
            return Err(msg::chip_not_recognised(l));
        };
        let profile = &profiles[*idx];
        if rom.len() as u64 > cfi.device_size {
            return Err(msg::rom_exceeds_chip(l, &format!("{} {}", rom.len() / 1024, msg::ko(l)), &format!("{} {}", cfi.device_size / 1024, msg::ko(l))));
        }
        let chip = cfi.device_size as usize;
        let started = Instant::now();

        // 1. Back up the cartridge as it is (two reads that must agree).
        let backup_file = if backup {
            let mut pass = 0u64;
            let current = program::read_until_stable(|| {
                let label = msg::backup_cart_progress(l);
                let mut v = Vec::new();
                let base = pass * chip as u64;
                pass += 1;
                d.r().dump_dmg_rom(mapper, chip, &mut v, &mut |done| {
                    let kib_s = (base as f32 + done as f32) / 1024.0 / started.elapsed().as_secs_f32().max(0.001);
                    let _ = tx.send(Event::Progress { label: label.clone(), done: base + done as u64, total: 2 * chip as u64, kib_s });
                    !self.cancel.load(Ordering::SeqCst)
                })
                .map_err(|e| if matches!(e, zerocart_core::Error::Cancelled) { msg::cancelled(l) } else { e.to_string() })?;
                Ok(v)
            })?;
            let backups = self.out_dir.join("backups");
            std::fs::create_dir_all(&backups).map_err(|e| e.to_string())?;
            let file = unique_path(&backups, &format!("{} avant flash", sanitize(&c.title)), "gb");
            std::fs::write(&file, &current).map_err(|e| e.to_string())?;
            Some(file)

        } else {
            None
        };

        // 2. Write, only the sectors that differ. From here on the transfer cannot be cancelled.
        let sectors = cfi.sectors();
        let t1 = Instant::now();
        let mut on_progress = |i: usize, n: usize, m: &str| {
            let done = sectors.iter().filter(|s| s.0 < rom.len() as u64).nth(i).map_or(rom.len() as u64, |s| s.0);
            let _ = tx.send(Event::Progress {
                label: msg::writing_sectors(l, i.min(n), n, m),
                done,
                total: rom.len() as u64,
                kib_s: done as f32 / 1024.0 / t1.elapsed().as_secs_f32().max(0.001),
            });
        };
        let report = match d {
            Dev::Gbx(g) => program::flash_rom(g, profile, &sectors, &rom, &mut on_progress, &|| false),
            Dev::Chis(c) => zerocart_flashchips::chis_gb::flash_rom(c, profile, &sectors, &rom, &mut on_progress, &|| false),
        }
        .map_err(|e| msg::failed_backup_in(l, &e.to_string(), backup_file.as_ref().map(|f| f.display().to_string()).as_deref()))?;

        // 3. Verify. A read of a Game Boy cartridge can be wrong now and then (a burst of bad bytes that is not in the
        // cartridge: seen on hardware, the next read was perfect). Only a difference that shows up in two reads in a row
        // is a failed write; one wrong read is read again.
        let name = backup_file.as_ref().map(|f| f.file_name().unwrap_or_default().to_string_lossy().into_owned());
        for _ in 0..2 {
            let mut after = Vec::new();
            d.r().dump_dmg_rom(mapper, rom.len(), &mut after, &mut |_| true).map_err(|e| e.to_string())?;
            if after == rom {
                return Ok(msg::gb_written_ok(l, report.rewritten as usize, name.as_deref()));
            }
        }
        Err(msg::written_mismatch(l, name.as_deref()))
    }

    fn write_save(&self, d: &mut Dev, c: &Cart, path: &Path, tx: &Sender<Event>) -> Result<String, String> {
        let l = self.lang();
        // Game Boy: the RAM size comes from the header and the mapper; GBA: the database gives the SRAM size.
        let (size, mapper) = match c.dmg {
            Some((Some(_), _)) if c.ram_size == 0 => return Err(msg::no_save_on_cart(l)),
            Some((Some(m), _)) => (c.ram_size, Some(m)),
            Some(_) => return Err(msg::mapper_unsupported(l)),
            None => {
                let e = c.entry.as_ref().ok_or_else(|| msg::game_not_in_database(l))?;
                if !matches!(e.st, 3 | 7 | 8) {
                    return Err(msg::save_type_unsupported(l, agb_save_name(e.st)));
                }
                (e.ss as usize, None)
            }
        };
        let data = std::fs::read(path).map_err(|e| e.to_string())?;
        if data.len() != size {
            return Err(msg::save_size_mismatch(l, data.len(), size));
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
        let p_ = progress(msg::backing_up_old_save(l), 0, total);
        let a = read_save_data(d, mapper, size, l, &mut |n| p_(n, &self.cancel, tx))?;
        let p_ = progress(msg::backing_up_old_save(l), size, total);
        let b = read_save_data(d, mapper, size, l, &mut |n| p_(n, &self.cancel, tx))?;
        
        let note: String = if a == b {
            let dir = self.out_dir.join("backups");
            std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
            let mut stem = format!("{} avant écriture", sanitize(&c.title));
            if !d.r().can_power_cycle() {
                stem = unverified_stem(&stem);
            }
            let file = unique_path(&dir, &stem, "sav");
            std::fs::write(&file, &a).map_err(|e| e.to_string())?;
            msg::note_old_save(l, &file.file_name().unwrap_or_default().to_string_lossy())
        } else {
            msg::note_old_save_unstable(l)
        };
        // 2. Write, read back in the same power session.
        let p_ = progress(msg::writing_save(l), 2 * size, total);
        write_save_data(d, mapper, &data, l, &mut |n| p_(n, &self.cancel, tx))?;
        let back = read_save_data(d, mapper, data.len(), l, &mut |_| true)?;
        if back != data {
            return Err(msg::save_readback_differs(l, &note));
        }
        // 3. Does it survive a power cycle? (A dead battery does not keep it.)
        match d.r().power_cycle(Duration::from_millis(500)).map_err(|e| e.to_string())? {
            false => Ok(msg::save_written_unverified(l, &note)),
            true => {
                let again = read_save_data(d, mapper, data.len(), l, &mut |_| true)?;
                if again == data {
                    Ok(msg::save_written_ok(l, &note))
                } else {
                    Err(msg::save_lost_at_cut(l, &note))
                }
            }
        }
    }

    fn detect(&self, dev: &mut Option<Dev>, kind: Option<zerocart_core::ReaderKind>, dmg: bool) -> Result<(ReaderView, Option<(CartView, Cart)>), String> {
        if let Some(mut d) = dev.take() {
            let _ = d.r().close(); // leaves the cartridge unpowered before reconnecting
        }
        let mut d = match kind {
            Some(k) => Dev::open_kind(k, self.lang())?,
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
        let l = self.lang();
        if c.rom_size == 0 {
            return Err(msg::rom_size_unknown_db(l));
        }
        let name = file_stem(c);
        let ext = if c.mode == CartMode::Agb { "gba" } else { "gb" };
        let path = unique_path(&self.out_dir, &name, ext);
        std::fs::create_dir_all(&self.out_dir).map_err(|e| e.to_string())?;
        let mut out = std::io::BufWriter::new(std::fs::File::create(&path).map_err(|e| e.to_string())?);
        let started = Instant::now();
        let label = msg::reading_rom(l, &c.title);
        let total = c.rom_size as u64;
        let progress = |done: usize| {
            let kib_s = done as f32 / 1024.0 / started.elapsed().as_secs_f32().max(0.001);
            let _ = tx.send(Event::Progress { label: label.clone(), done: done as u64, total, kib_s });
            !self.cancel.load(Ordering::SeqCst)
        };
        // Game Boy only: how the reads agreed (how many passes, banks put right by a vote, banks nobody agreed on)
        let mut gb_note: Option<GbReadNote> = None;
        let result: Result<(u32, Option<bool>), String> = match c.dmg {
            Some((mapper, global)) => {
                let mapper = mapper.ok_or_else(|| msg::mapper_unsupported(l))?;
                self.read_gb_rom(d, mapper, c, global, tx, &mut out).map(|(crc, ok, note)| {
                    gb_note = Some(note);
                    (crc, Some(ok))
                })
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
                        res = Err(msg::cancelled(l));
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
                return Err(if e == "cancelled" { msg::cancelled(l) } else { e });
            }
        };
        out.flush().map_err(|e| e.to_string())?;
        drop(out);
        let file = path.file_name().unwrap_or_default().to_string_lossy().into_owned();
        let note = gb_note.unwrap_or_default();
        match (&c.entry, header_ok) {
            (Some(e), _) if e.rc == got => Ok(msg::rom_verified_crc(l, &file)),
            (Some(_), _) if note.unresolved == 0 && note.passes >= 2 && note.identical => Ok(msg::rom_stable_checksum_differs(l, &file)),
            (Some(_), _) => Err(msg::rom_crc_differs(l, &file)),
            (None, Some(true)) if note.corrected > 0 => Ok(msg::rom_corrected(l, &file, note.corrected)),
            (None, Some(true)) => Ok(msg::rom_verified_header(l, &file)),
            // two reads that agree with each other: the cartridge says what it says, its header is what is wrong
            (None, Some(false)) if note.unresolved == 0 && note.passes >= 2 && note.identical => Ok(msg::rom_stable_checksum_differs(l, &file)),
            (None, Some(false)) => Err(msg::rom_header_checksum_wrong(l, &file)),
            (None, None) => Ok(msg::rom_unverifiable(l, &file)),
        }
    }

    /// Reads a Game Boy ROM so that a burst of wrong bytes (seen on hardware, now and then) does not end in a bad
    /// file: one pass; if its checksum matches the header (or the database), that is all. Otherwise a second pass; if the
    /// two agree the cartridge really holds that; if not, a third pass and a vote per bank, bank by bank. Writes the
    /// result to `out` and returns its CRC-32, whether its Game Boy checksum matches the header's, and what happened.
    fn read_gb_rom(
        &self,
        d: &mut Dev,
        mapper: DmgMapper,
        c: &Cart,
        header_checksum: u16,
        tx: &Sender<Event>,
        out: &mut dyn Write,
    ) -> Result<(u32, bool, GbReadNote), String> {
        let l = self.lang();
        let started = Instant::now();
        let total = c.rom_size as u64;
        let mut passes: Vec<Vec<u8>> = Vec::new();
        let good = |data: &[u8]| -> bool {
            gb_global_checksum(data) == header_checksum || c.entry.as_ref().is_some_and(|e| e.rc == crc32_of(data))
        };
        for pass in 1..=3usize {
            let label = if pass == 1 { msg::reading_rom(l, &c.title) } else { msg::reading_rom_pass(l, &c.title, pass) };
            let mut data = Vec::with_capacity(c.rom_size);
            d.r()
                .dump_dmg_rom(mapper, c.rom_size, &mut data, &mut |done| {
                    let kib_s = done as f32 / 1024.0 / started.elapsed().as_secs_f32().max(0.001);
                    let _ = tx.send(Event::Progress { label: label.clone(), done: done as u64, total, kib_s });
                    !self.cancel.load(Ordering::SeqCst)
                })
                .map_err(|e| if matches!(e, zerocart_core::Error::Cancelled) { msg::cancelled(l) } else { e.to_string() })?;
            let ok = good(&data);
            passes.push(data);
            if ok && pass == 1 {
                break; // the usual case: nothing to put right
            }
            if pass == 2 && passes[0] == passes[1] {
                break; // two reads agree
            }
        }
        let identical = passes.windows(2).all(|w| w[0] == w[1]);
        let made = passes.len();
        let (data, corrected, unresolved) = if passes.len() == 3 {
            let refs: Vec<&[u8]> = passes.iter().map(|p| p.as_slice()).collect();
            vote_banks(&refs, ROM_BANK)
        } else {
            (passes.swap_remove(0), 0, 0)
        };
        out.write_all(&data).map_err(|e| e.to_string())?;
        let crc = crc32_of(&data);
        let note = GbReadNote { passes: made, corrected, unresolved, identical };
        Ok((crc, good(&data), note))
    }

    fn read_save(&self, d: &mut Dev, c: &Cart, tx: &Sender<Event>) -> Result<String, String> {
        let l = self.lang();
        let size = match c.dmg {
            Some((Some(_), _)) if c.ram_size == 0 => return Err(msg::no_save_on_cart(l)),
            Some((None, _)) => return Err(msg::mapper_unsupported(l)),
            Some(_) => c.ram_size,
            None => {
                let e = c.entry.as_ref().ok_or_else(|| msg::game_not_in_database(l))?;
                if !matches!(e.st, 3 | 7 | 8) {
                    return Err(msg::save_type_unsupported(l, agb_save_name(e.st)));
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
            let label = msg::reading_save_two_passes(l, &c.title);
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
            .map_err(|e| if matches!(e, zerocart_core::Error::Cancelled) { msg::cancelled(l) } else { e.to_string() })?;
            reads.push(data);
        }
        if reads[0] != reads[1] {
            let diff = reads[0].iter().zip(&reads[1]).filter(|(a, b)| a != b).count();
            return Err(if cycled { msg::save_lost_after_cut(l, diff) } else { msg::save_unstable(l, diff) });
        }
        std::fs::create_dir_all(&self.out_dir).map_err(|e| e.to_string())?;
        let stem = if cycled { file_stem(c) } else { unverified_stem(&file_stem(c)) };
        let path = unique_path(&self.out_dir, &stem, "sav");
        std::fs::write(&path, &reads[0]).map_err(|e| e.to_string())?;
        let file = path.file_name().unwrap_or_default().to_string_lossy().into_owned();
        Ok(if cycled {
            msg::save_read_ok(l, &file)
        } else {
            // A leading '!' makes the application show it as a warning, not as a success.
            msg::unverified_save(l, &file)
        })
    }
}

/// A header read at 3.3 V that is a valid GBA header (fixed byte and complement checksum): a Game Boy cartridge
/// read this way gives junk, which almost never passes both checks.
pub fn gba_cart_inside(header: &[u8]) -> bool {
    parse_agb(header).is_some_and(|h| h.logo_ok && h.header_checksum_ok)
}

/// Reads the whole save (Game Boy RAM through the mapper, or GBA SRAM). `progress(done)` returns false to cancel.
fn read_save_data(d: &mut Dev, mapper: Option<DmgMapper>, size: usize, l: Lang, progress: &mut dyn FnMut(usize) -> bool) -> Result<Vec<u8>, String> {
    let fail = |e: zerocart_core::Error| if matches!(e, zerocart_core::Error::Cancelled) { msg::cancelled(l) } else { e.to_string() };
    match mapper {
        Some(m) => d.r().read_dmg_ram(m, size, progress).map_err(fail),
        None => {
            let mut data = Vec::with_capacity(size);
            while data.len() < size {
                let n = 0x2000.min(size - data.len());
                data.extend(d.r().read_agb_sram(data.len() as u32, n).map_err(fail)?);
                if !progress(data.len()) {
                    return Err(msg::cancelled(l));
                }
            }
            Ok(data)
        }
    }
}

/// Writes the whole save (Game Boy RAM through the mapper, or GBA SRAM).
fn write_save_data(d: &mut Dev, mapper: Option<DmgMapper>, data: &[u8], l: Lang, progress: &mut dyn FnMut(usize) -> bool) -> Result<(), String> {
    let fail = |e: zerocart_core::Error| if matches!(e, zerocart_core::Error::Cancelled) { msg::cancelled(l) } else { e.to_string() };
    match mapper {
        Some(m) => d.r().write_dmg_ram(m, data, progress).map_err(fail),
        None => d.r().write_agb_sram(data, progress).map_err(fail),
    }
}

/// The size of a Game Boy ROM bank, the unit in which a bad read is put right.
const ROM_BANK: usize = 0x4000;

/// How the reads of a Game Boy ROM went.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct GbReadNote {
    /// How many reads of the whole ROM were made (1 to 3).
    passes: usize,
    /// Banks whose first read was wrong and was put right by the vote.
    corrected: usize,
    /// Banks on which no two reads agreed.
    unresolved: usize,
    /// All the reads gave the same bytes.
    identical: bool,
}

fn crc32_of(data: &[u8]) -> u32 {
    let mut crc = Crc32::new();
    crc.update(data);
    crc.finalize()
}

/// The Game Boy global checksum: the sum of every byte except the two of the checksum itself (0x14E-0x14F).
fn gb_global_checksum(rom: &[u8]) -> u16 {
    rom.iter().enumerate().filter(|(i, _)| *i != 0x14E && *i != 0x14F).fold(0u16, |s, (_, &b)| s.wrapping_add(b as u16))
}

/// A vote between reads of the same ROM, bank by bank: a bank's content is the one that at least two reads agree on.
/// Returns the ROM, how many banks differ from the first read, and how many banks have no majority (the first read's
/// bank is kept there).
fn vote_banks(reads: &[&[u8]], bank: usize) -> (Vec<u8>, usize, usize) {
    let len = reads[0].len();
    let mut out = Vec::with_capacity(len);
    let (mut corrected, mut unresolved) = (0, 0);
    for start in (0..len).step_by(bank) {
        let end = (start + bank).min(len);
        let slices: Vec<&[u8]> = reads.iter().map(|r| &r[start..end]).collect();
        let winner = slices.iter().find(|s| slices.iter().filter(|o| *o == *s).count() >= 2);
        match winner {
            Some(w) => {
                if *w != slices[0] {
                    corrected += 1;
                }
                out.extend_from_slice(w);
            }
            None => {
                unresolved += 1;
                out.extend_from_slice(slices[0]);
            }
        }
    }
    (out, corrected, unresolved)
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
    #[test]
    fn a_vote_between_three_reads_puts_a_bad_bank_right() {
        let good: Vec<u8> = (0..0x10000usize).map(|i| (i * 7 + 3) as u8).collect();
        let mut bad = good.clone();
        bad[0x4100..0x4180].fill(0x6E); // a burst of wrong bytes inside bank 1
        let (out, corrected, unresolved) = vote_banks(&[&bad, &good, &good], ROM_BANK);
        assert_eq!((out, corrected, unresolved), (good.clone(), 1, 0), "the first read was the wrong one");
        let (out, corrected, unresolved) = vote_banks(&[&good, &bad, &good], ROM_BANK);
        assert_eq!((out, corrected, unresolved), (good.clone(), 0, 0), "a wrong second read changes nothing");
        // three reads that disagree on one bank: no majority, the first read is kept and it is counted
        let (mut a, mut b) = (good.clone(), good.clone());
        a[0x8000] ^= 1;
        b[0x8000] ^= 2;
        let (out, corrected, unresolved) = vote_banks(&[&a, &b, &good], ROM_BANK);
        assert_eq!((corrected, unresolved), (0, 1));
        assert_eq!(out[0x8000], a[0x8000]);
        // the last bank may be shorter than a bank
        let short = &good[..0xC100];
        assert_eq!(vote_banks(&[short, short, short], ROM_BANK).0, short);
    }

    #[test]
    fn the_game_boy_checksum_skips_its_own_two_bytes() {
        let mut rom = vec![1u8; 0x200];
        let before = gb_global_checksum(&rom);
        rom[0x14E] = 0xAA;
        rom[0x14F] = 0xBB;
        assert_eq!(gb_global_checksum(&rom), before, "the checksum bytes do not count");
        rom[0x150] = 9;
        assert_eq!(gb_global_checksum(&rom), before.wrapping_add(8));
        assert_eq!(gb_global_checksum(&[]), 0);
        assert_eq!(crc32_of(b"123456789"), 0xCBF43926, "the standard CRC-32 check value");
    }
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
