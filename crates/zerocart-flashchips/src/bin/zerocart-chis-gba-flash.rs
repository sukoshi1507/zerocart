// SPDX-License-Identifier: GPL-3.0-or-later
//! `zerocart-chis-gba-flash`: flashes a GBA ROM on a rewritable cartridge in the Chisflash reader (original firmware).
//!
//!   zerocart-chis-gba-flash --rom FILE --backup BACKUP.gba [--configs DIR] [--yes]
//!
//! Identifies the chip, saves the whole chip first (two reads that must agree), then (only with `--yes`) erases it,
//! programs the ROM and verifies. Without `--yes` it stops after the backup and shows the plan.
use std::time::Instant;
use zerocart_core::chis::{self, Chis, BAUD};
use zerocart_core::header::parse_agb;
use zerocart_core::transport::SerialTransport;
use zerocart_flashchips::chis_agb::{can_program, flash_rom, read_all, AgbBus};
use zerocart_flashchips::program::read_until_stable;
use zerocart_flashchips::{agb, detect, load_dir};

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
    let (Some(rom_path), Some(backup)) = (arg(&args, "--rom"), arg(&args, "--backup")) else {
        println!("usage: zerocart-chis-gba-flash --rom FILE --backup BACKUP.gba [--configs DIR] [--yes]");
        return Ok(());
    };
    let yes = args.iter().any(|a| a == "--yes");
    let rom = std::fs::read(&rom_path).map_err(|e| format!("{rom_path}: {e}"))?;
    let h = parse_agb(&rom).filter(|h| h.logo_ok && h.header_checksum_ok).ok_or("the file is not a valid GBA ROM (logo / header checksum)")?;
    println!("ROM: \"{}\" ({} KiB, CRC32 {:08x})", h.title, rom.len() / 1024, zerocart_core::db::crc32(&rom));
    if std::path::Path::new(&backup).exists() {
        return Err(format!("{backup} already exists: refusing to overwrite a backup"));
    }
    let (profiles, _) = load_dir(std::path::Path::new(&arg(&args, "--configs").unwrap_or_else(|| "configs".into())));
    let port = chis::find_ports().into_iter().next().ok_or("no Chisflash found (USB 0483:0721)")?;
    let mut r = Chis::new(SerialTransport::open(&port, BAUD).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
    println!("Chisflash on {port}");

    let cart = parse_agb(&r.read_rom(0, 0x180).map_err(|e| e.to_string())?).filter(|h| h.logo_ok).ok_or("no valid GBA cartridge in the reader")?;
    println!("cartridge now: \"{}\"", cart.title);
    let found = detect(&mut AgbBus(&mut r), &profiles, "AGB")?;
    let cfi = found.cfi.clone().ok_or("the flash chip was not identified (no CFI table)")?;
    println!("chip: {} MiB, CFI buffer {:?}", cfi.device_size / (1 << 20), cfi.buffer_size);
    if rom.len() as u64 > cfi.device_size {
        return Err(format!("the ROM ({} bytes) does not fit the chip ({} bytes)", rom.len(), cfi.device_size));
    }
    let idx = agb::choose_profile(&mut AgbBus(&mut r), &profiles, &found.matches, &found.answered)?;
    let profile = &profiles[idx];
    println!("profile: {} ({})", profile.name, profile.file);
    match can_program(profile) {
        Ok(()) => println!("programming: the reader's own command (fast)"),
        Err(_) => println!("programming: word by word with the profile's own sequence (slow, about 4.5 ms a word)"),
    }

    println!("reading the whole chip twice for the backup...");
    let chip = cfi.device_size as usize;
    let t0 = Instant::now();
    let current = read_until_stable(|| read_all(&mut r, chip, &mut |_| {}))?;
    std::fs::write(&backup, &current).map_err(|e| e.to_string())?;
    println!("backup written: {backup} ({:.0} s, CRC32 {:08x})", t0.elapsed().as_secs_f32(), zerocart_core::db::crc32(&current));
    if !yes {
        println!("dry run: nothing was erased. Add --yes to erase the chip and write the ROM.");
        return Ok(());
    }
    let t1 = Instant::now();
    let mut last = String::new();
    let report = flash_rom(&mut r, profile, &rom, &mut |done, total, m| {
        let line = format!("{m} {}/{} KiB", done / 1024, total / 1024);
        if line != last && (m != "programming" || done % 0x40000 == 0 || done == total) {
            println!("  {line}");
            last = line;
        }
    })
    .map_err(|e| format!("{e} (the previous content is in {backup})"))?;
    println!("flashed in {:.0} s: {} attempt(s), {} byte(s) repaired", t1.elapsed().as_secs_f32(), report.attempts, report.repaired);
    let after = read_all(&mut r, rom.len(), &mut |_| {})?;
    if after == rom {
        println!("VERIFIED: the cartridge holds exactly {rom_path}");
        Ok(())
    } else {
        Err(format!("MISMATCH after the final read; the previous content is in {backup}"))
    }
}
