//! `zerocart-flash --probe [--agb] [--configs DIR]`: identifies the flash chip of the inserted cartridge.
//! Writes only reset / unlock / identifier-read commands; nothing is erased or programmed.
use zerocart_core::transport::SerialTransport;
use zerocart_core::{find_ports, CartMode, GbxCart};
use zerocart_flashchips::{detect, load_dir, FlashBus};
use std::time::Duration;

struct Dev(GbxCart<SerialTransport>);

impl FlashBus for Dev {
    fn write_cmds(&mut self, cmds: &[(u32, u16)]) -> Result<(), String> {
        if cmds.is_empty() { return Ok(()); }
        self.0.flash_write(cmds, true).map_err(|e| e.to_string())
    }
    fn read(&mut self, address: u32, len: usize) -> Result<Vec<u8>, String> {
        self.0.read_rom(address, len).map_err(|e| e.to_string())
    }
    fn set_we_pin(&mut self, pin: u8) -> Result<(), String> {
        self.0.set_we_pin(pin).map_err(|e| e.to_string())
    }
}

fn arg(args: &[String], name: &str) -> Option<String> {
    args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).cloned()
}

fn main() {
    if let Err(e) = run() {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), String> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let rewrite = arg(&args, "--rewrite-sector");
    let flash_rom = arg(&args, "--flash-rom");
    if let Some(file) = arg(&args, "--fw-verify") {
        return fw_verify(&file, arg(&args, "--fw-ini").as_deref(), None);
    }
    if args.iter().any(|a| a == "--fw-info") {
        return fw_info(&args);
    }
    if !args.iter().any(|a| a == "--probe") && arg(&args, "--rewrite-sector").is_none() && flash_rom.is_none() {
        println!("usage: zerocart-flash --probe [--agb] [--configs DIR]   (identification only, nothing is erased or written)");
        println!("       zerocart-flash --rewrite-sector N --rom FILE [--yes]   (Game Boy: erase sector N and write it back from FILE;");
        println!("                                        without --yes it only checks and shows the plan)");
        println!("       zerocart-flash --fw-info [--fw-bin fw.bin [--fw-ini fw.ini]]   (reader firmware: read-only; compares with a file)");
        println!("       zerocart-flash --fw-verify fw.bin [--fw-ini fw.ini]   (checks a GBFlash firmware file; no reader needed)");
        println!("       zerocart-flash --flash-rom FILE --backup BACKUP.gb [--yes]   (Game Boy: write a whole ROM, only the sectors");
        println!("                                        that differ; BACKUP receives the current content first)");
        return Ok(());
    }
    let agb = args.iter().any(|a| a == "--agb");
    let (profiles, errors) = load_dir(std::path::Path::new(&arg(&args, "--configs").unwrap_or_else(|| "configs".into())));
    if !errors.is_empty() {
        eprintln!("warning: {} profile(s) could not be read: {}", errors.len(), errors.join("; "));
    }
    println!("{} flash profiles loaded", profiles.len());
    let port = find_ports().into_iter().next().ok_or("no reader found")?;
    let t = SerialTransport::open(&port, 1_000_000).map_err(|e| e.to_string())?;
    let mut dev = GbxCart::connect(t, Duration::from_millis(200)).map_err(|e| e.to_string())?;
    println!("{} {}", dev.fw.pcb_name, dev.fw.version_string());
    dev.set_mode(if agb { CartMode::Agb } else { CartMode::Dmg }).map_err(|e| e.to_string())?;
    dev.prepare_header().map_err(|e| e.to_string())?;
    let mut dev = Dev(dev);
    let kind = if agb { "AGB" } else { "DMG" };
    let d = detect(&mut dev, &profiles, kind)?;
    for l in &d.id_lines {
        println!("identifier read: {l}");
    }
    if d.matches.is_empty() {
        println!("no known flash chip answered (probably a mask ROM, or an unsupported chip)");
    }
    for &i in &d.matches {
        let p = &profiles[i];
        println!("match: {}  ({}, {} V, {} MiB)", p.name, p.file, p.voltage, p.flash_size / 1048576);
    }
    if let Some(w) = d.we_pin {
        println!("write-enable pin: {w}");
    }
    match &d.cfi {
        Some(c) => {
            println!("CFI: {}", c.describe());
            let sectors = c.sectors();
            if let (Some(f), Some(l)) = (sectors.first(), sectors.last()) {
                println!("sector map: {} sectors; first at 0x{:X} size 0x{:X}; last at 0x{:X} size 0x{:X}", sectors.len(), f.0, f.1, l.0, l.1);
            }
        }
        None => println!("CFI: none"),
    }
    if let (Some(file), true) = (flash_rom.as_ref(), agb) {
        let c = d.cfi.as_ref().ok_or("no CFI data: the chip size is unknown, refusing to write")?;
        let chosen = match arg(&args, "--profile") {
            Some(name) => profiles.iter().find(|p| p.kind == "AGB" && p.name == name).ok_or("no GBA profile with that name")?,
            None if d.matches.len() == 1 => &profiles[d.matches[0]],
            None => return Err("the chip matched no profile: choose one with --profile \"NAME\" (see the probe output)".into()),
        };
        return flash_agb_cmd(&mut dev.0, chosen, c, file, &args);
    }
    if let Some(file) = flash_rom {
        let (Some(c), Some(&m)) = (d.cfi.as_ref(), d.matches.first()) else { return Err("chip not identified: refusing to write".into()) };
        if d.matches.len() != 1 || agb {
            return Err("exactly one profile must match, in Game Boy mode: refusing to write".into());
        }
        return flash_rom_cmd(&mut dev.0, &profiles[m], c, &file, &args);
    }
    if let Some(n) = rewrite {
        let (Some(c), Some(&m)) = (d.cfi.as_ref(), d.matches.first()) else { return Err("chip not identified: refusing to write".into()) };
        if d.matches.len() != 1 || agb {
            return Err("exactly one profile must match, in Game Boy mode: refusing to write".into());
        }
        return rewrite_sector_cmd(&mut dev.0, &profiles[m], c, n.parse().map_err(|_| "bad sector number")?, &args);
    }
    dev.0.close().map_err(|e| e.to_string())
}

fn rewrite_sector_cmd(
    dev: &mut GbxCart<SerialTransport>,
    profile: &zerocart_flashchips::Profile,
    cfi: &zerocart_flashchips::cfi::Cfi,
    n: usize,
    args: &[String],
) -> Result<(), String> {
    use zerocart_core::mapper::DmgMapper;
    let yes = args.iter().any(|a| a == "--yes");
    let rom_path = arg(args, "--rom").ok_or("--rom FILE is required")?;
    let rom = std::fs::read(&rom_path).map_err(|e| e.to_string())?;
    // --current FILE: what the cartridge holds now (default: the same file, so the rewrite changes nothing).
    let held = match arg(args, "--current") {
        Some(f) => std::fs::read(f).map_err(|e| e.to_string())?,
        None => rom.clone(),
    };
    if held.len() != rom.len() {
        return Err("--current and --rom must have the same size".into());
    }
    let sectors = cfi.sectors();
    let &(off, size) = sectors.get(n).ok_or("no such sector")?;
    let range = off as usize..off as usize + size as usize;
    if rom.len() as u64 != cfi.device_size {
        return Err(format!("the file is {} bytes, the chip holds {}", rom.len(), cfi.device_size));
    }
    println!("sector {n}: offset 0x{off:X}, size 0x{size:X}; banks {}..{}", off / 0x4000, (off + size as u64) / 0x4000);

    // Guard 1: the cartridge must already hold exactly this file, so that rewriting a sector is a no-op in the end.
    println!("reading the whole cartridge to compare it with the file...");
    let mut current = Vec::new();
    dev.dump_dmg_rom(DmgMapper::Mbc5, rom.len(), &mut current, &mut |_| true).map_err(|e| e.to_string())?;
    if current != held {
        let first = current.iter().zip(&held).position(|(a, b)| a != b).unwrap_or(0);
        return Err(format!("the cartridge differs from the file it should hold (first difference at 0x{first:X}): refusing to erase anything"));
    }
    println!("the cartridge holds what it should");
    // Only the chosen sector may differ between what it holds and what is written.
    let outside = held.iter().zip(&rom).enumerate().filter(|(i, (a, b))| a != b && !range.contains(i)).count();
    if outside > 0 {
        return Err(format!("{outside} bytes outside sector {n} differ between --current and --rom: refusing"));
    }
    println!("{} bytes of sector {n} change", held[range.clone()].iter().zip(&rom[range.clone()]).filter(|(a, b)| a != b).count());
    zerocart_flashchips::program::plan_rewrite(profile, off, size as usize)?;
    if !yes {
        println!("dry run: nothing was erased. Add --yes to erase and rewrite this sector.");
        return dev.close().map_err(|e| e.to_string());
    }

    println!("ERASING and rewriting sector {n}...");
    let t0 = std::time::Instant::now();
    zerocart_flashchips::program::rewrite_sector(dev, profile, off, &rom[range], &mut |m| println!("  {m}"))?;
    println!("done in {:.1} s; verifying the whole cartridge...", t0.elapsed().as_secs_f32());
    let mut after = Vec::new();
    dev.dump_dmg_rom(DmgMapper::Mbc5, rom.len(), &mut after, &mut |_| true).map_err(|e| e.to_string())?;
    if after == rom {
        println!("VERIFIED: the cartridge is identical to the file again");
    } else {
        let diffs = after.iter().zip(&rom).filter(|(a, b)| a != b).count();
        let first = after.iter().zip(&rom).position(|(a, b)| a != b).unwrap_or(0);
        println!("MISMATCH: {diffs} bytes differ, first at 0x{first:X}");
    }
    dev.close().map_err(|e| e.to_string())
}

fn flash_rom_cmd(
    dev: &mut GbxCart<SerialTransport>,
    profile: &zerocart_flashchips::Profile,
    cfi: &zerocart_flashchips::cfi::Cfi,
    file: &str,
    args: &[String],
) -> Result<(), String> {
    use zerocart_core::mapper::DmgMapper;
    let yes = args.iter().any(|a| a == "--yes");
    let rom = std::fs::read(file).map_err(|e| e.to_string())?;
    if !rom.len().is_power_of_two() || rom.len() < 0x8000 {
        return Err("the file size is not a Game Boy ROM size".into());
    }
    let backup = arg(args, "--backup").ok_or("--backup FILE is required: the current content is saved there first")?;
    if std::path::Path::new(&backup).exists() {
        return Err(format!("{backup} already exists: refusing to overwrite a backup"));
    }
    let sectors = cfi.sectors();
    println!("reading the whole cartridge ({} KiB) twice for the backup...", cfi.device_size / 1024);
    let current = zerocart_flashchips::program::read_until_stable(|| {
        let mut v = Vec::new();
        dev.dump_dmg_rom(DmgMapper::Mbc5, cfi.device_size as usize, &mut v, &mut |_| true).map_err(|e| e.to_string())?;
        Ok(v)
    })?;
    std::fs::write(&backup, &current).map_err(|e| e.to_string())?;
    println!("backup written: {backup} (CRC32 {:08x})", zerocart_core::db::crc32(&current));
    let used: Vec<_> = sectors.iter().filter(|s| s.0 < rom.len() as u64).collect();
    let changing = used
        .iter()
        .filter(|&&&(o, sz)| {
            let (o, e) = (o as usize, (o as usize + sz as usize).min(rom.len()));
            current[o..e] != rom[o..e]
        })
        .count();
    println!("{} sectors cover the ROM, {changing} of them differ from the cartridge", used.len());
    if !yes {
        println!("dry run: nothing was erased. Add --yes to write.");
        return dev.close().map_err(|e| e.to_string());
    }
    let t0 = std::time::Instant::now();
    let report = zerocart_flashchips::program::flash_rom(
        dev,
        profile,
        &sectors,
        &rom,
        &mut |i, n, m| println!("  [{i}/{n}] {m}"),
        &|| false,
    )?;
    println!("{} sectors rewritten, {} unchanged, in {:.0} s; verifying...", report.rewritten, report.unchanged, t0.elapsed().as_secs_f32());
    let mut after = Vec::new();
    dev.dump_dmg_rom(DmgMapper::Mbc5, rom.len(), &mut after, &mut |_| true).map_err(|e| e.to_string())?;
    if after == rom {
        println!("VERIFIED: the cartridge now holds exactly {file} (CRC32 {:08x})", zerocart_core::db::crc32(&after));
    } else {
        let diffs = after.iter().zip(&rom).filter(|(a, b)| a != b).count();
        println!("MISMATCH: {diffs} bytes differ");
    }
    dev.close().map_err(|e| e.to_string())
}

fn flash_agb_cmd(
    dev: &mut GbxCart<SerialTransport>,
    profile: &zerocart_flashchips::Profile,
    cfi: &zerocart_flashchips::cfi::Cfi,
    file: &str,
    args: &[String],
) -> Result<(), String> {
    let yes = args.iter().any(|a| a == "--yes");
    let rom = std::fs::read(file).map_err(|e| e.to_string())?;
    if !zerocart_core::header::parse_agb(&rom).is_some_and(|h| h.logo_ok) {
        return Err("the file is not a valid GBA ROM (header check failed)".into());
    }
    if rom.len() as u64 > cfi.device_size {
        return Err(format!("the ROM ({} bytes) does not fit the chip ({} bytes)", rom.len(), cfi.device_size));
    }
    let backup = arg(args, "--backup").ok_or("--backup FILE is required: the whole chip is saved there first")?;
    if std::path::Path::new(&backup).exists() {
        return Err(format!("{backup} already exists: refusing to overwrite a backup"));
    }
    println!("profile: {}", profile.name);
    let id = zerocart_flashchips::agb::verify_profile(dev, profile)?;
    println!("the chip answers this profile (identifier {:02X?})", &id[..4]);
    let size = cfi.device_size as usize;
    println!("reading the whole chip ({} KiB) twice for the backup...", size / 1024);
    let current = zerocart_flashchips::program::read_until_stable(|| {
        let mut v = Vec::with_capacity(size);
        while v.len() < size {
            let n = 0x4000.min(size - v.len());
            v.extend(dev.read_rom(v.len() as u32, n).map_err(|e| e.to_string())?);
        }
        Ok(v)
    })?;
    std::fs::write(&backup, &current).map_err(|e| e.to_string())?;
    println!("backup written: {backup} (CRC32 {:08x}); the first {} KiB {} the ROM", zerocart_core::db::crc32(&current),
        rom.len() / 1024, if current[..rom.len()] == rom[..] { "already equal" } else { "differ from" });
    if !yes {
        println!("dry run: nothing was erased. Add --yes to ERASE THE WHOLE CHIP and write the ROM.");
        return dev.close().map_err(|e| e.to_string());
    }
    let t0 = std::time::Instant::now();
    let mut last = String::new();
    let report = zerocart_flashchips::agb::flash_rom(dev, profile, &rom, &mut |done, total, m| {
        let line = format!("{m} {}%", done * 100 / total.max(1));
        if line != last && (done % 0x100000 == 0 || !m.starts_with("programming")) {
            println!("  {line}");
        }
        last = line;
    })?;
    println!("done in {:.0} s ({} attempt(s), {} byte(s) repaired in place); verifying again...", t0.elapsed().as_secs_f32(), report.attempts, report.repaired);
    let mut after = Vec::new();
    while after.len() < rom.len() {
        let n = 0x4000.min(rom.len() - after.len());
        after.extend(dev.read_rom(after.len() as u32, n).map_err(|e| e.to_string())?);
    }
    println!("{}", if after == rom { format!("VERIFIED: the cartridge now holds exactly {file}") } else { "MISMATCH after the final read".to_string() });
    dev.close().map_err(|e| e.to_string())
}

/// Checks a firmware file (and compares it with the reader's build time when that is known).
fn fw_verify(file: &str, ini: Option<&str>, reader_ts: Option<u32>) -> Result<(), String> {
    use zerocart_core::fwupdate::{compare, format_ts, Comparison, FirmwareFile};
    let data = std::fs::read(file).map_err(|e| format!("{file}: {e}"))?;
    let ini_text = match ini {
        Some(i) => Some(std::fs::read_to_string(i).map_err(|e| format!("{i}: {e}"))?),
        None => None,
    };
    let f = FirmwareFile::check(data, ini_text.as_deref())?;
    println!("{file}: {} bytes, CRC16 {:04X}, CRC32 {:08x}", f.data.len(), zerocart_core::fwupdate::crc16(&f.data), zerocart_core::db::crc32(&f.data));
    match (&f.version, f.build_ts) {
        (Some(v), Some(ts)) => println!("firmware {v}, built {}", format_ts(ts)),
        _ => println!("no fw.ini given: version and build time unknown"),
    }
    if let Some(rts) = reader_ts {
        println!(
            "compared with the reader: {}",
            match compare(rts, &f) {
                Comparison::Same => "same build",
                Comparison::Newer => "NEWER than the reader's firmware",
                Comparison::Older => "OLDER than the reader's firmware",
                Comparison::Unknown => "unknown (no build time in the file)",
            }
        );
    }
    println!("this check changes nothing: no firmware is written by this tool.");
    Ok(())
}

/// Read-only: what the connected reader says about its firmware.
fn fw_info(args: &[String]) -> Result<(), String> {
    use zerocart_core::fwupdate::format_ts;
    let port = find_ports().into_iter().next().ok_or("no reader found")?;
    let t = SerialTransport::open(&port, 1_000_000).map_err(|e| e.to_string())?;
    let mut dev = GbxCart::connect(t, Duration::from_millis(200)).map_err(|e| e.to_string())?;
    let fw = dev.fw.clone();
    println!("reader: {} {} on {port}", fw.pcb_name, fw.pcb_label());
    println!("firmware: {} (PCB id {}), built {}", fw.version_string(), fw.pcb_ver, format_ts(fw.fw_ts));
    println!("restart into the bootloader on command: {}", if fw.bootloader_reset { "supported" } else { "NOT supported by this firmware" });
    println!("serial number: {}", match (fw.serial_supported, dev.read_device_serial().map_err(|e| e.to_string())?) {
        (_, Some(s)) => s,
        (true, None) => "supported but empty".into(),
        (false, None) => "not supported by this firmware".into(),
    });
    if let Some(file) = arg(args, "--fw-bin") {
        println!();
        fw_verify(&file, arg(args, "--fw-ini").as_deref(), Some(fw.fw_ts))?;
    }
    println!("nothing was written to the reader.");
    Ok(())
}
