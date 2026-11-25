// SPDX-License-Identifier: GPL-3.0-or-later
//! `zerocart-chis`: read-only test tool for the Chisflash reader (original firmware, USB 0483:0721).
//!
//!   zerocart-chis --info
//!   zerocart-chis --gba-header [--db DIR]
//!   zerocart-chis --gba-dump FILE [--db DIR]
//!   zerocart-chis --gb-header [--db DIR]
//!   zerocart-chis --gb-dump FILE   (ROM banks are chosen by writing the mapper registers, as any dump does)
//!
//! It never erases, programs or writes anything to a cartridge: only the read commands of the reader are used.
use zerocart_core::chis::{self, Chis, BAUD};
use zerocart_core::db::{agb_save_name, crc32, Database};
use zerocart_core::header::{parse_agb, parse_dmg};
use zerocart_core::mapper::{DmgMapper, ROM_BANK_SIZE};
use zerocart_core::transport::SerialTransport;
use std::io::Write;
use std::path::{Path, PathBuf};

fn arg(args: &[String], name: &str) -> Option<String> {
    args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).cloned()
}

/// USB descriptor strings of a tty (`manufacturer`, `product`, `serial`, ids), read from sysfs.
fn usb_strings(sys_tty: &Path, tty: &str) -> Vec<(String, String)> {
    let mut dir = sys_tty.join(tty).join("device");
    for _ in 0..4 {
        if dir.join("idVendor").exists() {
            return ["idVendor", "idProduct", "manufacturer", "product", "serial", "bcdDevice"]
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

fn run() -> Result<(), String> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let info = args.iter().any(|a| a == "--info");
    let gba_header = args.iter().any(|a| a == "--gba-header");
    let gb_header = args.iter().any(|a| a == "--gb-header");
    let gba_dump = arg(&args, "--gba-dump");
    let gb_dump = arg(&args, "--gb-dump");
    if !(info || gba_header || gb_header || gba_dump.is_some() || gb_dump.is_some()) {
        println!("usage: zerocart-chis --info | --gba-header | --gb-header | --gba-dump FILE | --gb-dump FILE   [--db DIR]");
        println!("read-only: nothing is erased or written to the cartridge.");
        return Ok(());
    }
    let ports = chis::find_ports();
    if ports.is_empty() {
        return Err("no Chisflash found (USB 0483:0721, /dev/ttyACM*): is it plugged in?".into());
    }
    for p in &ports {
        let name = p.trim_start_matches("/dev/");
        println!("{p}");
        for (k, v) in usb_strings(Path::new("/sys/class/tty"), name) {
            println!("  {k}: {v}");
        }
    }
    let port = &ports[0];
    let mut reader = Chis::new(SerialTransport::open(port, BAUD).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
    println!("opened {port} at {BAUD} baud, command buffer reset (DTR/RTS)");
    let db_dir = PathBuf::from(arg(&args, "--db").unwrap_or_else(|| "configs".into()));

    if gba_header || gba_dump.is_some() {
        let header = reader.read_rom(0, 0xC0).map_err(|e| format!("reading the GBA header: {e}"))?;
        let h = parse_agb(&header).ok_or("header too short")?;
        println!("GBA header: title {:?}, code {:?}, maker {:?}, version {}", h.title, h.game_code, h.maker_code, h.version);
        println!("  logo {}, header checksum {}", if h.logo_ok { "OK" } else { "INVALID" }, if h.header_checksum_ok { "OK" } else { "INVALID" });
        if !h.logo_ok {
            println!("  (no valid GBA cartridge: is it a Game Boy cartridge, or is the reader empty?)");
        }
        // The database wants the first 0x180 bytes.
        let full = reader.read_rom(0, 0x180).map_err(|e| format!("reading the header: {e}"))?;
        let entry = Database::load(&db_dir.join("db_AGB.json")).ok().and_then(|db| db.lookup_agb(&full).cloned());
        match &entry {
            Some(e) => println!("database: {} {} - ROM {} KiB, CRC32 {:08x}, save {} ({} KiB)", e.gn, e.ne, e.rs / 1024, e.rc, agb_save_name(e.st), e.ss / 1024),
            None => println!("database: no entry for this header"),
        }
        if let Some(file) = &gba_dump {
            let e = entry.ok_or("unknown game: the ROM size is not known, refusing to guess")?;
            let size = e.rs as usize;
            let mut out = std::io::BufWriter::new(std::fs::File::create(file).map_err(|e| e.to_string())?);
            let t0 = std::time::Instant::now();
            let mut all = Vec::with_capacity(size);
            while all.len() < size {
                let n = 0x10000.min(size - all.len());
                let block = reader.read_rom(all.len() as u32, n).map_err(|e| format!("at 0x{:X}: {e}", all.len()))?;
                out.write_all(&block).map_err(|e| e.to_string())?;
                all.extend(block);
                eprint!("\r{} / {} KiB ({:.1} KiB/s)", all.len() / 1024, size / 1024, all.len() as f64 / 1024.0 / t0.elapsed().as_secs_f64());
            }
            eprintln!();
            out.flush().map_err(|e| e.to_string())?;
            let crc = crc32(&all);
            println!("CRC32 {crc:08x}: {}", if crc == e.rc { "matches the database: dump verified" } else { "does NOT match the database: dump may be corrupt" });
        }
    }
    if gb_header || gb_dump.is_some() {
        let header = reader.gbc_read(0, 0x180).map_err(|e| format!("reading the Game Boy header: {e}"))?;
        let h = parse_dmg(&header).ok_or("header too short")?;
        println!("Game Boy header: title {:?}, mapper {}, ROM {} KiB, RAM {} KiB", h.title, h.mapper, h.rom_size / 1024, h.ram_size / 1024);
        println!("  logo {}, header checksum {}", if h.logo_ok { "OK" } else { "INVALID" }, if h.header_checksum_ok { "OK" } else { "INVALID" });
        if let Some(file) = &gb_dump {
            let mapper = DmgMapper::from_cart_type(h.cart_type).ok_or_else(|| format!("mapper '{}' is not supported yet", h.mapper))?;
            if !h.logo_ok || h.rom_size == 0 {
                return Err("no valid Game Boy header: refusing to dump".into());
            }
            let size = h.rom_size;
            let mut out = std::io::BufWriter::new(std::fs::File::create(file).map_err(|e| e.to_string())?);
            let (mut crc, mut sum, mut pos) = (zerocart_core::db::Crc32::new(), 0u16, 0usize);
            let t0 = std::time::Instant::now();
            for bank in 0..size.div_ceil(ROM_BANK_SIZE) {
                let (writes, start) = mapper.select_rom_bank(bank);
                for (a, v) in writes {
                    reader.gbc_write(a, &[v]).map_err(|e| format!("selecting bank {bank}: {e}"))?;
                }
                let n = ROM_BANK_SIZE.min(size - pos);
                let data = reader.gbc_read(start, n).map_err(|e| format!("bank {bank}: {e}"))?;
                for (i, &b) in data.iter().enumerate() {
                    if pos + i != 0x14E && pos + i != 0x14F {
                        sum = sum.wrapping_add(b as u16);
                    }
                }
                crc.update(&data);
                out.write_all(&data).map_err(|e| e.to_string())?;
                pos += n;
                eprint!("\r{} / {} KiB ({:.1} KiB/s)", pos / 1024, size / 1024, pos as f64 / 1024.0 / t0.elapsed().as_secs_f64());
            }
            eprintln!();
            out.flush().map_err(|e| e.to_string())?;
            println!("CRC32 {:08x}; global checksum {:04x} {} the header's {:04x}", crc.finalize(), sum,
                if sum == h.global_checksum { "matches" } else { "does NOT match" }, h.global_checksum);
        }
    }
    println!("nothing was written to the cartridge (only mapper registers were selected for the dump).");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn usb_strings_are_found_by_walking_up_from_the_tty() {
        let root = std::env::temp_dir().join(format!("zerocart-chis-test-{}", std::process::id()));
        let usb = root.join("devices/usb1/1-1");
        let iface = usb.join("1-1:1.0/tty/ttyACM0/device");
        std::fs::create_dir_all(&iface).unwrap();
        let sys_tty = root.join("class/tty");
        std::fs::create_dir_all(sys_tty.join("ttyACM0")).unwrap();
        // sys/class/tty/ttyACM0/device -> the interface directory; its parent holds the USB device files
        std::os::unix::fs::symlink(usb.join("1-1:1.0"), sys_tty.join("ttyACM0/device")).unwrap();
        std::fs::write(usb.join("idVendor"), "0483\n").unwrap();
        std::fs::write(usb.join("idProduct"), "0721\n").unwrap();
        std::fs::write(usb.join("product"), "Beggar Socket\n").unwrap();
        let s = usb_strings(&sys_tty, "ttyACM0");
        assert!(s.contains(&("idVendor".into(), "0483".into())) && s.contains(&("product".into(), "Beggar Socket".into())), "{s:?}");
        assert!(usb_strings(&sys_tty, "ttyACM9").is_empty());
        let _ = std::fs::remove_dir_all(&root);
    }
}
