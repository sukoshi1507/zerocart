// SPDX-License-Identifier: GPL-3.0-or-later
//! `zerocart-chis-flash`: flashes a Game Boy (MBC5) cartridge in the Chisflash reader (original firmware).
//!
//!   zerocart-chis-flash --flash-rom --rom FILE [--current FILE] --backup BACKUP.gb [--yes]
//!   zerocart-chis-flash --offset 0x3FE000 --size 0x2000 --rom FILE [--current FILE] --backup BACKUP.gb [--slow] [--yes]
//!   zerocart-chis-flash --offset .. --size .. --rom FILE --backup BACKUP.gb --try-gbc-program [--sample N] [--yes]
//!
//! `--flash-rom` writes the whole ROM, rewriting only the sectors whose content differs; the second form rewrites
//! ONE sector. `--rom` is what must be written, `--current` what the whole cartridge holds now (default: the same
//! file, so nothing changes in the end): the cartridge must hold exactly that, and a backup of two agreeing reads is
//! written before anything is erased. Without `--yes` it only checks and shows the plan.
//! `--try-gbc-program` (a sector that is blank in the ROM) tries the reader's programming command on harmless
//! bytes and restores the sector. `--slow` programs one byte at a time with plain bus writes (about 180 B/s)
//! instead of the reader's command (about 20 KiB/s).
use zerocart_core::chis::{self, Chis, BAUD};
use zerocart_core::header::parse_dmg;
use zerocart_core::mapper::{DmgMapper, ROM_BANK_SIZE};
use zerocart_core::transport::SerialTransport;
use zerocart_flashchips::cfi::Cfi;
use zerocart_flashchips::chis_gb::{flash_rom, read_cart, read_sector, rewrite_sector, select_bank};
use zerocart_flashchips::detect::FlashBus;
use zerocart_flashchips::program::{erase_sector, plan_rewrite, program_single, read_until_stable, Plan};
use zerocart_flashchips::{detect, load_dir, Profile};
use std::time::Duration;

type Reader = Chis<SerialTransport>;

fn arg(args: &[String], name: &str) -> Option<String> {
    args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).cloned()
}

fn has(args: &[String], name: &str) -> bool {
    args.iter().any(|a| a == name)
}

fn num(s: &str) -> Result<u64, String> {
    match s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        Some(h) => u64::from_str_radix(h, 16),
        None => s.parse(),
    }
    .map_err(|_| format!("bad number '{s}'"))
}

fn main() {
    if let Err(e) = run() {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}

/// Everything the three modes check before they touch the cartridge.
struct Setup {
    r: Reader,
    profile: Profile,
    cfi: Cfi,
    rom: Vec<u8>,
    rom_path: String,
    held: Vec<u8>,
    backup: String,
}

fn setup(args: &[String]) -> Result<Setup, String> {
    let rom_path = arg(args, "--rom").ok_or("--rom FILE is required")?;
    let backup = arg(args, "--backup").ok_or("--backup FILE is required: the cartridge is saved there first")?;
    let rom = std::fs::read(&rom_path).map_err(|e| format!("{rom_path}: {e}"))?;
    let held = match arg(args, "--current") {
        Some(f) => std::fs::read(&f).map_err(|e| format!("{f}: {e}"))?,
        None => rom.clone(),
    };
    if held.len() != rom.len() {
        return Err("--current and --rom must have the same size".into());
    }
    if std::path::Path::new(&backup).exists() {
        return Err(format!("{backup} already exists: refusing to overwrite a backup"));
    }
    let (profiles, _) = load_dir(std::path::Path::new(&arg(args, "--configs").unwrap_or_else(|| "configs".into())));
    let port = chis::find_ports().into_iter().next().ok_or("no Chisflash found (USB 0483:0721)")?;
    let mut r = Chis::new(SerialTransport::open(&port, BAUD).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
    println!("Chisflash on {port}");

    let header = r.gbc_read(0, 0x180).map_err(|e| e.to_string())?;
    let h = parse_dmg(&header).filter(|h| h.logo_ok).ok_or("no valid Game Boy cartridge in the reader (is it a GBA cartridge, or is the reader empty?)")?;
    if DmgMapper::from_cart_type(h.cart_type) != Some(DmgMapper::Mbc5) {
        return Err(format!("mapper '{}' is not supported: MBC5 only", h.mapper));
    }
    println!("cartridge: {} ({}, {} KiB)", h.title, h.mapper, h.rom_size / 1024);

    // The chip must answer exactly one profile, and its CFI table gives the sectors.
    let found = detect(&mut r, &profiles, "DMG")?;
    let (Some(cfi), [idx]) = (found.cfi.clone(), found.matches.as_slice()) else {
        return Err("the flash chip is not identified by exactly one profile: refusing to write".into());
    };
    let profile = profiles[*idx].clone();
    println!("chip: {} ({})", profile.name, profile.file);
    if rom.len() as u64 != cfi.device_size {
        return Err(format!("the file is {} bytes, the chip holds {}", rom.len(), cfi.device_size));
    }
    Ok(Setup { r, profile, cfi, rom, rom_path, held, backup })
}

/// Reads the whole cartridge twice (the reads must agree), saves it, and requires it to equal `held`.
fn backup_and_check(s: &mut Setup) -> Result<(), String> {
    println!("reading the whole cartridge twice for the backup...");
    let t0 = std::time::Instant::now();
    let size = s.rom.len();
    let current = read_until_stable(|| read_cart(&mut s.r, size, &mut |_| true))?;
    println!("  done in {:.0} s", t0.elapsed().as_secs_f32());
    std::fs::write(&s.backup, &current).map_err(|e| e.to_string())?;
    println!("backup written: {} (CRC32 {:08x})", s.backup, zerocart_core::db::crc32(&current));
    if current != s.held {
        let first = current.iter().zip(&s.held).position(|(a, b)| a != b).unwrap_or(0);
        return Err(format!("the cartridge differs from the file it should hold (first difference at 0x{first:X}): refusing to erase"));
    }
    println!("the cartridge holds what it should");
    Ok(())
}

fn verify_whole(s: &mut Setup) -> Result<(), String> {
    println!("verifying the whole cartridge...");
    let after = read_cart(&mut s.r, s.rom.len(), &mut |_| true)?;
    if after == s.rom {
        println!("VERIFIED: the cartridge now holds exactly {} (CRC32 {:08x})", s.rom_path, zerocart_core::db::crc32(&after));
        Ok(())
    } else {
        let diffs = after.iter().zip(&s.rom).filter(|(a, b)| a != b).count();
        Err(format!("MISMATCH: {diffs} byte(s) differ from {}; the backup is {}", s.rom_path, s.backup))
    }
}

fn run() -> Result<(), String> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let whole = has(&args, "--flash-rom");
    let sector_mode = arg(&args, "--offset").is_some() && arg(&args, "--size").is_some();
    if !whole && !sector_mode {
        println!("usage: zerocart-chis-flash --flash-rom --rom FILE [--current FILE] --backup BACKUP.gb [--yes]");
        println!("       zerocart-chis-flash --offset 0x3FE000 --size 0x2000 --rom FILE [--current FILE] --backup BACKUP.gb [--slow] [--yes]");
        println!("Game Boy (MBC5) cartridges in the Chisflash; a dry run unless --yes.");
        return Ok(());
    }
    let yes = has(&args, "--yes");
    let mut s = setup(&args)?;

    if whole {
        return flash_whole(&mut s, yes);
    }
    let off = num(&arg(&args, "--offset").unwrap())? as usize;
    let size = num(&arg(&args, "--size").unwrap())? as usize;
    if !s.cfi.sectors().contains(&(off as u64, size as u32)) {
        return Err(format!("0x{off:X} + 0x{size:X} is not a sector of this chip (CFI map)"));
    }
    let plan = plan_rewrite(&s.profile, off as u64, size)?;
    println!("sector: offset 0x{off:X}, size 0x{size:X}, erased at bus address 0x{:X}, banks {:?}", plan.sector_address,
        plan.segments.iter().map(|g| g.bank).collect::<Vec<_>>());
    backup_and_check(&mut s)?;
    let outside = s.held.iter().zip(&s.rom).enumerate().filter(|(i, (a, b))| a != b && !(off..off + size).contains(i)).count();
    if outside > 0 {
        return Err(format!("{outside} bytes outside the sector differ between --current and --rom: refusing"));
    }
    let changing = s.held[off..off + size].iter().zip(&s.rom[off..off + size]).filter(|(a, b)| a != b).count();
    println!("{changing} byte(s) of the sector change");
    if has(&args, "--try-gbc-program") {
        let n = arg(&args, "--sample").map(|v| num(&v)).transpose()?.unwrap_or(1) as usize;
        return try_gbc_program(&mut s, &plan, off, size, yes, n);
    }
    if !yes {
        println!("dry run: nothing was erased. Add --yes to erase and rewrite this sector.");
        return Ok(());
    }
    println!("ERASING and rewriting the sector...");
    let data = s.rom[off..off + size].to_vec();
    let t1 = std::time::Instant::now();
    if has(&args, "--slow") {
        select_bank(&mut s.r, plan.segments[0].bank)?;
        erase_sector(&mut s.r, &s.profile, plan.sector_address, Duration::from_millis(50), 100)?;
        for seg in &plan.segments {
            select_bank(&mut s.r, seg.bank)?;
            program_single(&mut s.r, &s.profile, 0x4000 + seg.start as u32, &data[seg.data_at..seg.data_at + seg.len], 200)?;
        }
        s.r.write_cmds(&s.profile.reset)?;
    } else {
        rewrite_sector(&mut s.r, &s.profile, off as u64, &data, &mut |m| println!("  {m}"))?;
    }
    println!("  sector rewritten in {:.1} s", t1.elapsed().as_secs_f32());
    verify_whole(&mut s)
}

fn flash_whole(s: &mut Setup, yes: bool) -> Result<(), String> {
    let sectors = s.cfi.sectors();
    let used = zerocart_flashchips::chis_gb::used_sectors(&sectors, s.rom.len());
    backup_and_check(s)?;
    let changing: Vec<_> = used
        .iter()
        .filter(|&&(o, sz)| {
            let (o, e) = (o as usize, (o as usize + sz as usize).min(s.rom.len()));
            s.held[o..e] != s.rom[o..e]
        })
        .collect();
    println!("{} sectors cover the ROM, {} of them differ from the cartridge", used.len(), changing.len());
    if !yes {
        println!("dry run: nothing was erased. Add --yes to write.");
        return Ok(());
    }
    let t0 = std::time::Instant::now();
    let mut last = String::new();
    let report = flash_rom(&mut s.r, &s.profile, &sectors, &s.rom, &mut |i, n, m| {
        let line = format!("  [{i}/{n}] {m}");
        if !m.ends_with("(unchanged)") && line != last {
            println!("{line}");
        }
        last = line;
    }, &|| false)?;
    println!("{} sectors rewritten, {} unchanged, in {:.0} s", report.rewritten, report.unchanged, t0.elapsed().as_secs_f32());
    verify_whole(s)
}

/// Experiment: the reader's own Game Boy programming command (0xFC) on a sector that is blank in the ROM.
fn try_gbc_program(s: &mut Setup, plan: &Plan, off: usize, size: usize, yes: bool, n: usize) -> Result<(), String> {
    if n == 0 || n > size || plan.sector_address as usize % ROM_BANK_SIZE + n > ROM_BANK_SIZE {
        return Err("the sample must fit in the sector and inside one 16 KiB bank window".into());
    }
    if s.held[off..off + size].iter().any(|&b| b != 0xFF) {
        return Err("the sector is not blank in the ROM: this experiment needs a blank sector".into());
    }
    // each byte has a single bit cleared: the least damage if the firmware programs somewhere else
    let sample: Vec<u8> = (0..n).map(|i| !(1u8 << (i % 8))).collect();
    println!("experiment: gbc_program(0x{:X}, buffer 0, {} byte(s) {:02X?}...)", plan.sector_address, n, &sample[..n.min(16)]);
    if !yes {
        println!("dry run: nothing was programmed. Add --yes to run the experiment.");
        return Ok(());
    }
    select_bank(&mut s.r, plan.segments[0].bank)?;
    let t0 = std::time::Instant::now();
    let sent = s.r.gbc_program(plan.sector_address, 0, &sample);
    let took = t0.elapsed();
    match &sent {
        Ok(()) => println!("the reader acknowledged the command after {:.3} s ({:.0} bytes/s)", took.as_secs_f32(), n as f32 / took.as_secs_f32().max(0.0001)),
        Err(e) => {
            println!("the reader answered with an error: {e}");
            s.r.resync().map_err(|e| e.to_string())?;
        }
    }
    s.r.write_cmds(&s.profile.reset)?;
    let back = read_sector(&mut s.r, plan)?;
    let mut expected = vec![0xFFu8; size];
    expected[..sample.len()].copy_from_slice(&sample);
    let wrong = back.iter().zip(&expected).filter(|(a, b)| a != b).count();
    println!("sector after the command: first {} bytes {:02X?}; {wrong} byte(s) differ from a correct programming", n.min(16), &back[..n.min(16)]);
    println!("reading the whole cartridge to see whether anything else changed...");
    let after = read_cart(&mut s.r, s.held.len(), &mut |_| true)?;
    let outside: Vec<usize> = after.iter().zip(&s.held).enumerate().filter(|(i, (a, b))| a != b && !(off..off + size).contains(i)).map(|(i, _)| i).collect();
    if outside.is_empty() {
        println!("  nothing changed outside the sector");
    } else {
        let blocks: std::collections::BTreeSet<usize> = outside.iter().map(|i| i / 0x10000).collect();
        println!("  !! {} byte(s) changed OUTSIDE the sector, in 64 KiB blocks {:?}, first at 0x{:X}", outside.len(), blocks, outside[0]);
    }
    println!("restoring the sector (erase)...");
    select_bank(&mut s.r, plan.segments[0].bank)?;
    erase_sector(&mut s.r, &s.profile, plan.sector_address, Duration::from_millis(50), 100)?;
    let blank = read_sector(&mut s.r, plan)?.iter().all(|&b| b == 0xFF);
    println!("  sector blank again: {blank}");
    println!("{}", if wrong == 0 && outside.is_empty() && sent.is_ok() && blank {
        "RESULT: gbc_program programmed exactly what was asked, and nothing else changed."
    } else {
        "RESULT: gbc_program does NOT behave like a correct programming of this chip (see above)."
    });
    Ok(())
}
