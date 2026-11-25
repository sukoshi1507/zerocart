// SPDX-License-Identifier: GPL-3.0-or-later
//! `zerocart-md`: read-only test tool for the Krikzz FlashKit MD programmer (Mega Drive cartridges).
//!
//!   zerocart-md --info                     the port, its USB strings, the handshake with the programmer
//!   zerocart-md --header                   the "SEGA" header of the inserted cartridge
//!   zerocart-md --dump FILE [--size N]     dump the ROM (size from the header unless --size is given)
//!   zerocart-md --save FILE                read the save RAM declared by the header
//!   common: --port /dev/ttyUSB0 (default: the first FTDI 0403:6001 found)
//!
//! It never erases, programs or writes data to a cartridge. The only bus writes are the bank register (0xA13000: it
//! maps the ROM or the save RAM at 0x200000) and the programmer's own settings. The flash chip is not probed: the
//! identification sends command sequences (CFI query, autoselect, reset) and is left to a later, explicit option.
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
    if !(info || header || dump.is_some() || save.is_some()) {
        println!("usage: zerocart-md --info | --header | --dump FILE [--size N] | --save FILE   [--port /dev/ttyUSB0]");
        println!("read-only: nothing is erased, programmed or written to the cartridge.");
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
    if info && !(header || dump.is_some() || save.is_some()) {
        return Ok(());
    }

    let bytes = kit.read_header_bytes().map_err(|e| format!("reading the header: {e}"))?;
    let h = megadrive::parse_header(&bytes).ok_or("short header")?;
    show(&h);

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
    println!("nothing was written to the cartridge (only the bank register and the programmer's settings).");
    Ok(())
}
