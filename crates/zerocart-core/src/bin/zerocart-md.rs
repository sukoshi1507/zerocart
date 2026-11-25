// SPDX-License-Identifier: GPL-3.0-or-later
//! `zerocart-md`: read-only test tool for the Krikzz FlashKit MD programmer (Mega Drive cartridges).
//!
//!   zerocart-md --info                     the port, its USB strings, the handshake with the programmer
//!   zerocart-md --header                   the "SEGA" header of the inserted cartridge
//!   zerocart-md --dump FILE [--size N]     dump the ROM (size from the header unless --size is given)
//!   zerocart-md --save FILE                read the save RAM declared by the header
//!   zerocart-md --identify                 identify the flash chip (CFI query + autoselect + reset commands only)
//!   zerocart-md --flash FILE --backup OUT  DESTRUCTIVE: write FILE to a flash cartridge; a dry run unless --yes
//!                                          (--buffered: use the chip's write buffer, faster, NOT yet checked on hardware)
//!   zerocart-md --sector-test OFFSET [--yes]  DIAGNOSTIC: erase, program and restore ONE sector, reading back at each step
//!   common: --port /dev/ttyUSB0 (default: the first FTDI 0403:6001 found)
//!
//! Except with `--flash ... --yes`, it never erases, programs or writes data to a cartridge: the only bus writes are
//! the bank register (0xA13000: it maps the ROM or the save RAM at 0x200000) and the programmer's own settings. `--identify` is the exception: it
//! sends flash command sequences (CFI query, autoselect, reset), never erase or program, and leaves the chip in
//! read-array mode.
use std::io::Write;
use std::path::Path;
use zerocart_core::krikzz::{self, FlashKit};
use zerocart_core::megadrive::{self, MdHeader};
use zerocart_core::transport::SerialTransport;

fn arg(args: &[String], name: &str) -> Option<String> {
    args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).cloned()
}

/// USB descriptor strings of a tty, read from sysfs (never the serial number).
fn usb_strings(sys_tty: &Path, tty: &str) -> Vec<(String, String)> {
    let mut dir = sys_tty.join(tty).join("device");
    for _ in 0..4 {
        if dir.join("idVendor").exists() {
            return ["idVendor", "idProduct", "manufacturer", "product", "bcdDevice"]
                .iter()
                .filter_map(|k| std::fs::read_to_string(dir.join(k)).ok().map(|v| (k.to_string(), v.trim().to_string())))
                .collect();
        }
        dir = dir.join("..");
    }
    Vec::new()
}

fn main() {
    if let Err(e) = run() {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}

fn show(h: &MdHeader) {
    println!("system:   {}", h.system);
    println!("title:    {}", h.title());
    println!("domestic: {}", h.name_domestic);
    println!("serial:   {}", h.serial);
    println!("region:   {}{}", h.region, if h.is_32x { " (32X)" } else { "" });
    println!("ROM:      0x{:X}-0x{:X}{}", h.rom_start, h.rom_end, h.rom_size().map(|s| format!(" ({} KiB)", s / 1024)).unwrap_or_else(|| " (not a plausible size)".into()));
    match h.ram {
        Some(r) => println!("save RAM: 0x{:X}-0x{:X}, {} bytes{}", r.start, r.end, r.size(), if r.odd_bytes { " (odd bytes)" } else { "" }),
        None => println!("save RAM: none declared"),
    }
    println!("checksum: 0x{:04X} (declared)", h.checksum);
    println!("header:   {}", if h.is_valid() { "valid" } else { "NOT a valid Mega Drive header (empty connector, blank flash, or another system)" });
}

fn run() -> Result<(), String> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let info = args.iter().any(|a| a == "--info");
    let header = args.iter().any(|a| a == "--header");
    let dump = arg(&args, "--dump");
    let save = arg(&args, "--save");
    let identify = args.iter().any(|a| a == "--identify");
    let flash = arg(&args, "--flash");
    let yes = args.iter().any(|a| a == "--yes");
    let sector_test = arg(&args, "--sector-test");
    if !(sector_test.is_some() || info || header || identify || dump.is_some() || save.is_some() || flash.is_some()) {
        println!("usage: zerocart-md --info | --header | --identify | --dump FILE [--size N] | --save FILE   [--port /dev/ttyUSB0]");
        println!("       zerocart-md --flash FILE --backup BACKUP.bin [--yes]   (a dry run unless --yes)");
        println!("nothing is erased or programmed without --flash ... --yes; --identify only sends the chip's query, identifier and reset commands.");
        return Ok(());
    }
    let ports = krikzz::find_ports();
    let port = match arg(&args, "--port") {
        Some(p) => p,
        None => ports.first().cloned().ok_or("no FTDI programmer found (USB 0403:6001, /dev/ttyUSB*): is it plugged in?")?,
    };
    println!("port: {port}");
    for (k, v) in usb_strings(Path::new("/sys/class/tty"), port.trim_start_matches("/dev/")) {
        println!("  {k}: {v}");
    }
    let t = SerialTransport::open(&port, 9600).map_err(|e| format!("{port}: {e}"))?;
    let mut kit = FlashKit::connect(t).map_err(|e| format!("handshake: {e}"))?;
    println!("handshake: FlashKit MD programmer answered with a valid id");
    if info && !(header || identify || dump.is_some() || save.is_some() || flash.is_some()) {
        return Ok(());
    }

    let bytes = kit.read_header_bytes().map_err(|e| format!("reading the header: {e}"))?;
    let h = megadrive::parse_header(&bytes).ok_or("short header")?;
    show(&h);

    if identify {
        match kit.probe_flash().map_err(|e| format!("identify: {e}"))? {
            None => println!("flash: no chip answered the CFI query or the identifier command (mask ROM, or not a flash cartridge)"),
            Some(f) => {
                println!("flash: CFI {}, size {}", if f.cfi_ok { "OK" } else { "not answered" },
                    if f.size_bytes == 0 { "unknown".to_string() } else { format!("{} KiB", f.size_bytes / 1024) });
                println!("  manufacturer 0x{:04X}, device {}{}", f.manufacturer,
                    f.device_id.iter().map(|d| format!("{d:04X}")).collect::<Vec<_>>().join("/"),
                    if f.byte_wide { " (a pair of 8-bit chips, ids are byte values)" } else { "" });
            }
        }
    }

    if let Some(file) = dump {
        if !h.is_valid() && arg(&args, "--size").is_none() {
            return Err("no valid header: the ROM size is unknown, refusing to guess (give --size)".into());
        }
        let size = match arg(&args, "--size") {
            Some(s) => {
                let s = s.trim();
                let n = match s.strip_prefix("0x") {
                    Some(hex) => usize::from_str_radix(hex, 16),
                    None => s.parse(),
                };
                n.map_err(|e| format!("--size: {e}"))?
            }
            None => h.rom_size().unwrap(),
        };
        if Path::new(&file).exists() {
            return Err(format!("{file} already exists: refusing to overwrite"));
        }
        let mut out = std::io::BufWriter::new(std::fs::File::create(&file).map_err(|e| e.to_string())?);
        let t0 = std::time::Instant::now();
        let d = kit
            .dump_rom(size, &mut out, &mut |done| {
                eprint!("\r{} / {} KiB ({:.1} KiB/s)", done / 1024, size / 1024, done as f64 / 1024.0 / t0.elapsed().as_secs_f64().max(0.001));
                true
            })
            .map_err(|e| format!("\ndump: {e}"))?;
        eprintln!();
        out.flush().map_err(|e| e.to_string())?;
        println!("dumped {size} bytes to {file}");
        println!("CRC32 {:08x}; Mega Drive checksum {:04X} {} the header's {:04X}", d.crc32, d.checksum16,
            if d.checksum16 == h.checksum { "matches" } else { "does NOT match" }, h.checksum);
    }

    if let Some(file) = save {
        let ram = h.ram.ok_or("the header declares no save RAM: nothing to read")?;
        if Path::new(&file).exists() {
            return Err(format!("{file} already exists: refusing to overwrite"));
        }
        let stream = kit.read_save_stream(ram.size()).map_err(|e| format!("save: {e}"))?;
        // The file keeps the bus's word stream (data on the odd bytes), as the reference client saves it.
        std::fs::write(&file, &stream).map_err(|e| e.to_string())?;
        let data: Vec<u8> = stream.chunks(2).map(|w| w[1]).collect();
        println!("save RAM: {} bytes of data ({} bytes with the even bytes) written to {file}", data.len(), stream.len());
        println!("  note: a cartridge without a (battery-backed) save RAM reads noise here; this reader cannot cut the power to tell");
    }
    if let Some(off) = sector_test {
        let off = match off.trim().strip_prefix("0x") {
            Some(hex) => u32::from_str_radix(hex, 16),
            None => off.trim().parse(),
        }
        .map_err(|e| format!("--sector-test: {e}"))?;
        return sector_test_run(&mut kit, off, yes);
    }
    if let Some(file) = flash {
        if args.iter().any(|a| a == "--buffered") {
            kit.set_buffered(true);
            println!("write buffer programming requested (falls back to word by word on a chip without a write buffer)");
        }
        flash_cartridge(&mut kit, &h, &file, arg(&args, "--backup"), yes)?;
        return Ok(());
    }
    println!("nothing was written to the cartridge (only the bank register and the programmer's settings).");
    Ok(())
}

fn flash_cartridge<T: zerocart_core::Transport>(
    kit: &mut FlashKit<T>,
    h: &MdHeader,
    file: &str,
    backup: Option<String>,
    yes: bool,
) -> Result<(), String> {
    let backup = backup.ok_or("--backup FILE is required: what is overwritten is saved there first")?;
    if Path::new(&backup).exists() {
        return Err(format!("{backup} already exists: refusing to overwrite a backup"));
    }
    let image = std::fs::read(file).map_err(|e| format!("{file}: {e}"))?;
    let geo = kit
        .read_geometry()
        .map_err(|e| e.to_string())?
        .ok_or("the flash chip does not answer the CFI query (mask ROM or not a flash cartridge): nothing is written")?;
    println!("chip: {} KiB, {} sectors, command set {}", geo.size_bytes / 1024, geo.sectors.len(), geo.command_set);
    if geo.command_set != 2 {
        return Err("only the AMD/Spansion command set is supported".into());
    }
    if image.is_empty() || image.len() % 2 != 0 || image.len() > geo.size_bytes {
        return Err(format!("the file is {} bytes: it must be even and fit the {} bytes of the chip", image.len(), geo.size_bytes));
    }
    let used = geo.used_sectors(image.len());
    let extent = geo.extent(image.len());
    println!("file: {file}, {} bytes; it covers {} sector(s), {} KiB of the chip", image.len(), used.len(), extent / 1024);
    if !megadrive::parse_header(&image).is_some_and(|f| f.is_valid()) {
        println!("  warning: the file has no valid Mega Drive header");
    }
    println!("cartridge now: {} ({})", h.title(), if h.is_valid() { "valid header" } else { "no valid header" });

    println!("reading the {} KiB that will be overwritten, twice, for the backup...", extent / 1024);
    let current = kit.read_stable(extent).map_err(|e| format!("{e}: refusing to write"))?;
    std::fs::write(&backup, &current).map_err(|e| e.to_string())?;
    println!("backup written: {backup} (CRC32 {:08x})", zerocart_core::db::crc32(&current));
    let mut want = image.clone();
    want.resize(extent, 0xFF);
    let differing = used.iter().filter(|&&(o, sz)| current[o as usize..(o + sz) as usize] != want[o as usize..(o + sz) as usize]).count();
    println!("{differing} of {} sector(s) differ from the file", used.len());
    if !yes {
        println!("dry run: nothing was erased or written. Add --yes to write.");
        return Ok(());
    }
    if differing == 0 {
        println!("the cartridge already holds the file: nothing to do.");
        return Ok(());
    }
    let t0 = std::time::Instant::now();
    let report = kit
        .flash_rom(&geo, &image, &mut |m, done, total| eprintln!("  [{done}/{total}] {m}"), &|| false)
        .map_err(|e| format!("flash: {e} ; the previous content is in {backup}"))?;
    println!("{} sector(s) rewritten, {} unchanged, {} retried, in {:.1} s", report.rewritten, report.unchanged, report.retried, t0.elapsed().as_secs_f32());
    println!("verifying the whole {} KiB...", extent / 1024);
    let after = kit.read(0, extent).map_err(|e| e.to_string())?;
    if after == want {
        println!("VERIFIED: the cartridge holds the file (CRC32 of the file {:08x}).", zerocart_core::db::crc32(&image));
        Ok(())
    } else {
        let n = after.iter().zip(&want).filter(|(a, b)| a != b).count();
        Err(format!("MISMATCH: {n} byte(s) differ from the file; the previous content is in {backup}"))
    }
}

/// DIAGNOSTIC for one sector: erase, program a pattern, restore the original content, reading back at each step with
/// both bus delays. The original content is read first and put back at the end.
fn sector_test_run<T: zerocart_core::Transport>(kit: &mut FlashKit<T>, offset: u32, yes: bool) -> Result<(), String> {
    let geo = kit.read_geometry().map_err(|e| e.to_string())?.ok_or("the chip does not answer the CFI query")?;
    let &(off, size) = geo.sectors.iter().find(|s| offset >= s.0 && offset < s.0 + s.1).ok_or("offset beyond the chip")?;
    println!("sector at 0x{off:X}, {size} bytes (index {})", geo.sectors.iter().position(|s| s.0 == off).unwrap());
    let rd = |kit: &mut FlashKit<T>, delay: u8| -> Result<Vec<u8>, String> {
        kit.set_delay(delay).map_err(|e| e.to_string())?;
        kit.read(off, size as usize).map_err(|e| e.to_string())
    };
    let orig = rd(kit, 1)?;
    let orig0 = rd(kit, 0)?;
    println!("original content: {} non-FF bytes; the same read at delay 0: {}", orig.iter().filter(|&&b| b != 0xFF).count(),
        if orig == orig0 { "identical" } else { "DIFFERENT" });
    if !yes {
        println!("dry run: nothing was erased. Add --yes to run the test (the sector is restored at the end).");
        return Ok(());
    }
    kit.erase_sector(off).map_err(|e| format!("erase: {e}"))?;
    let (b1, b0) = (rd(kit, 1)?, rd(kit, 0)?);
    println!("after the erase: delay 1 {} non-FF, delay 0 {} non-FF", b1.iter().filter(|&&b| b != 0xFF).count(), b0.iter().filter(|&&b| b != 0xFF).count());
    let pattern: Vec<u8> = (0..256usize).map(|i| (i as u8) ^ 0xA5).collect();
    kit.set_delay(0).map_err(|e| e.to_string())?;
    kit.program(off, &pattern).map_err(|e| format!("program: {e}"))?;
    let (p1, p0) = (rd(kit, 1)?, rd(kit, 0)?);
    println!("after programming 256 bytes: delay 1 {}, delay 0 {}", if p1[..256] == pattern[..] { "matches" } else { "DIFFERS" }, if p0[..256] == pattern[..] { "matches" } else { "DIFFERS" });
    if p1[..256] != pattern[..] {
        let first = p1[..256].iter().zip(&pattern).position(|(a, b)| a != b).unwrap();
        println!("  first difference at +{first}: read {:02X?} expected {:02X?}", &p1[first..(first + 8).min(256)], &pattern[first..(first + 8).min(256)]);
    }
    println!("restoring the original content...");
    kit.set_delay(0).map_err(|e| e.to_string())?;
    kit.erase_sector(off).map_err(|e| format!("erase: {e}"))?;
    if orig.iter().any(|&b| b != 0xFF) {
        kit.program(off, &orig).map_err(|e| format!("restore: {e}"))?;
    }
    let back = rd(kit, 1)?;
    println!("restored: {}", if back == orig { "identical to the original" } else { "DIFFERS from the original" });
    if back != orig {
        return Err("the sector could not be restored".into());
    }
    Ok(())
}
