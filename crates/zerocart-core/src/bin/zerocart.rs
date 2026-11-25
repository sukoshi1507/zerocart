//! Test CLI: `zerocart --info [--port /dev/ttyUSB0] [--agb]`, `zerocart --dump-rom FILE [--agb]`.
use zerocart_core::db::{agb_save_name, crc32, Crc32, Database};
use zerocart_core::mapper::DmgMapper;
use zerocart_core::header::{parse_agb, parse_dmg};
use zerocart_core::transport::SerialTransport;
use zerocart_core::{find_ports, CartMode, GbxCart};
use std::io::Write;
use std::time::Duration;

fn arg_value(args: &[String], name: &str) -> Option<String> {
    args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).cloned()
}

fn main() {
    if let Err(e) = run() {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let agb = args.iter().any(|a| a == "--agb");
    let dump = arg_value(&args, "--dump-rom");
    let dump_save = arg_value(&args, "--dump-save");
    let write_save = arg_value(&args, "--write-save");
    if !args.iter().any(|a| a == "--info") && dump.is_none() && dump_save.is_none() && write_save.is_none() {
        println!("usage: zerocart --info | --dump-rom FILE | --dump-save FILE (GBA SRAM)   [--port DEV] [--agb]  (read-only)");
        return Ok(());
    }
    let port = match arg_value(&args, "--port") {
        Some(p) => p,
        None => find_ports().into_iter().next().ok_or("no GBxCart RW found (is it plugged in?)")?,
    };
    let mut dev = GbxCart::connect(SerialTransport::open(&port, 1_000_000)?, Duration::from_millis(200))?;
    println!(
        "{} {} firmware {} on {}",
        dev.fw.pcb_name,
        dev.fw.pcb_label(),
        dev.fw.version_string(),
        port
    );
    if let Some(n) = arg_value(&args, "--chunk").and_then(|v| v.parse::<usize>().ok()) {
        dev.max_read = n; // experiment: bytes per read request
    }
    let mode = if agb { CartMode::Agb } else { CartMode::Dmg };
    dev.set_mode(mode)?;
    dev.prepare_header()?;
    let header = dev.read_rom(0, 0x180)?;
    match mode {
        CartMode::Dmg => match parse_dmg(&header) {
            Some(h) => println!("{h:#?}"),
            None => println!("header unreadable"),
        },
        CartMode::Agb => match parse_agb(&header) {
            Some(h) => println!("{h:#?}"),
            None => println!("header unreadable"),
        },
    }
    let db_dir = arg_value(&args, "--db").unwrap_or_else(|| "configs".into());
    let db_file = if agb { "db_AGB.json" } else { "db_DMG.json" };
    let entry = Database::load(&std::path::Path::new(&db_dir).join(db_file)).ok().and_then(|db| {
        (if agb { db.lookup_agb(&header) } else { db.lookup_dmg(&header) }).cloned()
    });
    match &entry {
        Some(e) => println!(
            "database: {} {} - ROM {} KiB, CRC32 {:08x}, save {} ({} KiB)",
            e.gn, e.ne, e.rs / 1024, e.rc, if agb { agb_save_name(e.st) } else { "see mapper" }, e.ss / 1024
        ),
        None => println!("database: no entry for this header (looked in {db_dir}/{db_file})"),
    }
    if let Some(path) = write_save {
        // Game Boy only. Writes the file to the cartridge RAM, then reads it back in the same power session.
        if agb {
            return Err("save writing is not implemented for GBA".into());
        }
        let h = parse_dmg(&header).ok_or("header unreadable")?;
        let mapper = DmgMapper::from_cart_type(h.cart_type).ok_or_else(|| format!("mapper '{}' is not supported yet", h.mapper))?;
        let data = std::fs::read(&path)?;
        if h.ram_size == 0 || data.len() != h.ram_size {
            return Err(format!("the file is {} bytes, the cartridge RAM is {}", data.len(), h.ram_size).into());
        }
        if !args.iter().any(|a| a == "--yes") {
            println!("dry run: would write {} bytes to the cartridge RAM. Add --yes.", data.len());
            return Ok(());
        }
        dev.write_dmg_ram(mapper, &data, &mut |_| true)?;
        let back = dev.read_dmg_ram(mapper, data.len(), &mut |_| true)?;
        let diff = back.iter().zip(&data).filter(|(a, b)| a != b).count();
        println!("{}", if diff == 0 { "VERIFIED: the RAM reads back exactly as written".to_string() } else { format!("MISMATCH: {diff} bytes differ") });
    }
    if let Some(path) = dump_save {
        let data = if agb {
            let e = entry.as_ref().ok_or("unknown cartridge: save type needed from the database")?;
            if !matches!(e.st, 3 | 7 | 8) {
                return Err(format!("save type '{}' is not supported yet (SRAM only)", agb_save_name(e.st)).into());
            }
            dev.read_agb_sram(0, e.ss as usize)?
        } else {
            let h = parse_dmg(&header).ok_or("header unreadable")?;
            let mapper = DmgMapper::from_cart_type(h.cart_type)
                .ok_or_else(|| format!("mapper '{}' is not supported yet", h.mapper))?;
            if h.ram_size == 0 {
                return Err("this cartridge has no save RAM".into());
            }
            dev.read_dmg_ram(mapper, h.ram_size, &mut |_| true)?
        };
        if args.iter().any(|a| a == "--retention-test") {
            // Power the cartridge off and on, read again: a working battery keeps the data.
            if !dev.power_cycle(Duration::from_millis(500))? {
                return Err("this reader cannot power-cycle the cartridge".into());
            }
            let again = if agb {
                dev.read_agb_sram(0, data.len())?
            } else {
                let h = parse_dmg(&header).ok_or("header unreadable")?;
                dev.read_dmg_ram(DmgMapper::from_cart_type(h.cart_type).ok_or("mapper")?, data.len(), &mut |_| true)?
            };
            let diff = data.iter().zip(&again).filter(|(a, b)| a != b).count();
            println!("retention test: {diff} of {} bytes differ after a power cycle ({})", data.len(),
                if diff == 0 { "data kept: battery OK" } else { "data NOT kept: battery dead?" });
        }
        std::fs::write(&path, &data)?;
        println!("save: {} bytes, CRC32 {:08x}, distinct bytes {}", data.len(), crc32(&data),
            data.iter().collect::<std::collections::HashSet<_>>().len());
    }
    if let Some(path) = dump {
        let size = match mode {
            _ if entry.is_some() => entry.as_ref().unwrap().rs as usize,
            CartMode::Dmg => parse_dmg(&header).map(|h| h.rom_size).filter(|&s| s > 0).ok_or("unknown ROM size")?,
            CartMode::Agb => {
                eprintln!("warning: unknown cartridge, ROM size is a guess (32 MiB read)");
                0x200_0000
            }
        };
        let mut out = std::io::BufWriter::new(std::fs::File::create(&path)?);
        println!("ROM size: {} KiB", size / 1024);
        let t0 = std::time::Instant::now();
        let show = |done: usize| {
            eprint!("\r{} / {} KiB ({:.1} KiB/s)", done / 1024, size / 1024, done as f64 / 1024.0 / t0.elapsed().as_secs_f64());
        };
        let crc = match mode {
            CartMode::Dmg => {
                let h = parse_dmg(&header).ok_or("header unreadable")?;
                let mapper = DmgMapper::from_cart_type(h.cart_type)
                    .ok_or_else(|| format!("mapper '{}' is not supported yet", h.mapper))?;
                let sums = dev.dump_dmg_rom(mapper, size, &mut out, &mut |d| { show(d); true })?;
                eprintln!();
                if sums.checksum16 == h.global_checksum {
                    println!("global checksum {:04x} matches the header: dump verified", sums.checksum16);
                } else {
                    println!("global checksum {:04x} does NOT match the header ({:04x}): dump may be corrupt", sums.checksum16, h.global_checksum);
                }
                sums.crc32
            }
            CartMode::Agb => {
                let mut crc = Crc32::new();
                let (mut addr, block) = (0usize, 0x4000usize);
                while addr < size {
                    let n = block.min(size - addr);
                    let data = dev.read_rom(addr as u32, n)?;
                    crc.update(&data);
                    out.write_all(&data)?;
                    addr += n;
                    show(addr);
                }
                eprintln!();
                crc.finalize()
            }
        };
        out.flush()?;
        drop(out);
        println!("CRC32 {crc:08x}");
        if let Some(e) = &entry {
            if crc == e.rc {
                println!("matches the database: dump verified");
            } else {
                println!("does NOT match the database ({:08x}): dump may be corrupt", e.rc);
            }
        }
    }
    dev.close()?;
    Ok(())
}
