// SPDX-License-Identifier: GPL-3.0-or-later
//! GBFlash firmware: checks on a firmware file, and the bootloader update protocol ported from FlashGBX
//! (`hw_GBFlash.py`, class `FirmwareUpdater`).
//!
//! Only the *checks* are reachable from the command line (`zerocart-flash --fw-info`, `--fw-verify`). The writing
//! functions are here, tested against a simulated bootloader, but nothing calls them yet: replacing the firmware
//! of a reader is not done without the user's explicit go.
use crate::transport::{read_exact, Transport};

pub const INTRO: u32 = 0x4848_4A4A;
pub const OUTRO: u32 = 0x4A4A_4848;

pub mod command {
    pub const CONNECT: u16 = 0x21;
    pub const FINISH: u16 = 0x23;
    pub const WRITE_PAGE: u16 = 0x24;
}

/// The reader's own application command that restarts it into the bootloader.
pub const BOOTLOADER_RESET: u8 = 0xF1;
/// Baud rate of the bootloader.
pub const BOOTLOADER_BAUD: u32 = 2_000_000;

/// CRC16 of the bootloader protocol (the Modbus polynomial, computed a nibble at a time).
pub fn crc16(data: &[u8]) -> u16 {
    const TABLE: [u16; 16] = [
        0x0000, 0xCC01, 0xD801, 0x1400, 0xF001, 0x3C00, 0x2800, 0xE401, 0xA001, 0x6C00, 0x7800, 0xB401, 0x5000, 0x9C01,
        0x8801, 0x4400,
    ];
    let mut crc = 0xFFFFu16;
    for &c in data {
        crc = TABLE[((c ^ crc as u8) & 0x0F) as usize] ^ (crc >> 4);
        crc = TABLE[(((c >> 4) as u16 ^ crc) & 0x0F) as usize] ^ (crc >> 4);
    }
    crc
}

/// A request packet: intro, sender, sequence, command, payload length, payload, outro; padded to an even length.
pub fn pack_packet(sender: u8, seq: u16, command: u16, payload: &[u8]) -> Vec<u8> {
    let mut v = Vec::with_capacity(16 + payload.len());
    v.extend_from_slice(&INTRO.to_be_bytes());
    v.push(sender);
    v.extend_from_slice(&seq.to_be_bytes());
    v.extend_from_slice(&command.to_be_bytes());
    v.extend_from_slice(&(payload.len() as u16).to_be_bytes());
    v.extend_from_slice(payload);
    v.extend_from_slice(&OUTRO.to_be_bytes());
    if v.len() % 2 == 1 {
        v.push(0);
    }
    v
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Packet {
    pub sender: u8,
    pub seq: u16,
    pub command: u16,
    pub payload: Vec<u8>,
}

/// Reads one packet (11 byte header, payload, 4 byte outro) and checks the intro and the outro.
pub fn read_packet(t: &mut dyn Transport) -> Result<Packet, String> {
    let e = |x: crate::Error| x.to_string();
    let mut h = [0u8; 11];
    read_exact(t, &mut h).map_err(e)?;
    if u32::from_be_bytes([h[0], h[1], h[2], h[3]]) != INTRO {
        return Err(format!("bootloader error: bad packet start {:02X?}", &h[..4]));
    }
    let len = u16::from_be_bytes([h[9], h[10]]) as usize;
    let mut payload = vec![0u8; len];
    read_exact(t, &mut payload).map_err(e)?;
    let mut o = [0u8; 4];
    read_exact(t, &mut o).map_err(e)?;
    if u32::from_be_bytes(o) != OUTRO {
        return Err(format!("bootloader error: bad packet end {o:02X?}"));
    }
    Ok(Packet { sender: h[4], seq: u16::from_be_bytes([h[5], h[6]]), command: u16::from_be_bytes([h[7], h[8]]), payload })
}

/// What the bootloader reports when it is asked to connect.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BootloaderInfo {
    pub program_size: u16,
    pub page_size: u16,
}

/// Asks the running application to restart into the bootloader (`0xF1`, answered `0x01`). Returns false when the
/// firmware does not answer (it does not support it). The caller must then reopen the port at
/// [`BOOTLOADER_BAUD`]: the device shows up again after about a quarter of a second.
pub fn request_bootloader(t: &mut dyn Transport) -> Result<bool, String> {
    let e = |x: crate::Error| x.to_string();
    t.clear_input().map_err(e)?;
    t.write_all(&[BOOTLOADER_RESET]).map_err(e)?;
    let mut b = [0u8; 1];
    if t.read(&mut b).map_err(e)? == 0 || b[0] != 1 {
        return Ok(false);
    }
    let _ = t.write_all(&[1]); // the device may already be gone
    Ok(true)
}

/// Connects to the bootloader (command 0x21) and returns its program and page sizes.
pub fn connect_bootloader(t: &mut dyn Transport) -> Result<BootloaderInfo, String> {
    let e = |x: crate::Error| x.to_string();
    let query = pack_packet(0, 1, command::CONNECT, &[]);
    t.clear_input().map_err(e)?;
    t.write_all(&query).map_err(e)?;
    let p = read_packet(t)?;
    if p.seq != 1 || p.command != command::CONNECT || p.payload.len() < 9 || p.payload[1..3] != [0x00, 0x03] {
        return Err("this is not a GBFlash bootloader (unexpected answer to the connection request)".into());
    }
    let info = BootloaderInfo {
        program_size: u16::from_be_bytes([p.payload[3], p.payload[4]]),
        page_size: u16::from_be_bytes([p.payload[7], p.payload[8]]),
    };
    if info.page_size == 0 {
        return Err("the bootloader reports a page size of zero".into());
    }
    Ok(info)
}

/// Writes `firmware` page by page (command 0x24, each page with its CRC16), then sends the final check (0x23: the
/// CRC16 of the whole image and its complement). `progress(done, total)` counts pages.
///
/// NOT called from anywhere yet: this replaces the firmware of the reader.
pub fn write_firmware(
    t: &mut dyn Transport,
    info: BootloaderInfo,
    firmware: &[u8],
    progress: &mut dyn FnMut(usize, usize),
) -> Result<(), String> {
    let e = |x: crate::Error| x.to_string();
    if firmware.is_empty() {
        return Err("empty firmware".into());
    }
    let page = info.page_size as usize;
    let pages = firmware.len().div_ceil(page);
    let mut seq: u16 = 2;
    for (i, chunk) in firmware.chunks(page).enumerate() {
        let index = (i + 1) as u16;
        let mut payload = Vec::with_capacity(chunk.len() + 6);
        payload.extend_from_slice(&index.to_be_bytes());
        payload.extend_from_slice(&(chunk.len() as u16).to_be_bytes());
        payload.extend_from_slice(chunk);
        payload.extend_from_slice(&crc16(chunk).to_be_bytes());
        t.write_all(&pack_packet(0, seq, command::WRITE_PAGE, &payload)).map_err(e)?;
        let r = read_packet(t)?;
        if r.seq != seq || r.command != command::WRITE_PAGE || r.payload.len() < 3 {
            return Err(format!("unexpected answer to page {index}"));
        }
        if u16::from_be_bytes([r.payload[0], r.payload[1]]) != index {
            return Err(format!("the bootloader acknowledged another page than {index}"));
        }
        if r.payload[2] != 1 {
            return Err(format!("the bootloader refused page {index}"));
        }
        seq = seq.wrapping_add(1);
        progress(i + 1, pages);
    }
    let crc = crc16(firmware);
    let mut payload = Vec::with_capacity(4);
    payload.extend_from_slice(&crc.to_be_bytes());
    payload.extend_from_slice(&(!crc).to_be_bytes());
    t.write_all(&pack_packet(0, seq, command::FINISH, &payload)).map_err(e)?;
    let r = read_packet(t)?;
    if r.seq != seq || r.command != command::FINISH || r.payload.first() != Some(&1) {
        return Err("the bootloader did not accept the final check: the update failed".into());
    }
    Ok(())
}

// ----- the firmware file ---------------------------------------------------------------------------------------

/// A firmware image as shipped by FlashGBX (`fw.bin`, with `fw.ini` next to it in `res/fw_GBFlash.zip`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FirmwareFile {
    pub data: Vec<u8>,
    /// e.g. "L16"
    pub version: Option<String>,
    pub build_ts: Option<u32>,
}

/// Biggest image accepted: the application area of the reader's microcontroller is far smaller.
pub const MAX_FIRMWARE_SIZE: usize = 128 * 1024;

/// Reads the `[Firmware]` section of an `fw.ini`: `fw_ver = L16`, `fw_buildts = 1793184971`.
pub fn parse_ini(text: &str) -> (Option<String>, Option<u32>) {
    let (mut ver, mut ts) = (None, None);
    for line in text.lines() {
        if let Some((k, v)) = line.split_once('=') {
            match k.trim() {
                "fw_ver" => ver = Some(v.trim().to_string()),
                "fw_buildts" => ts = v.trim().parse().ok(),
                _ => {}
            }
        }
    }
    (ver, ts)
}

impl FirmwareFile {
    /// Sanity checks: not empty, a plausible size, not blank (all 0x00 or 0xFF), and, when the ini is given, a
    /// version and build time that parse.
    pub fn check(data: Vec<u8>, ini: Option<&str>) -> Result<Self, String> {
        if data.is_empty() {
            return Err("the file is empty".into());
        }
        if data.len() > MAX_FIRMWARE_SIZE {
            return Err(format!("the file is {} bytes: too big for a GBFlash firmware (limit {MAX_FIRMWARE_SIZE})", data.len()));
        }
        if data.len() < 1024 {
            return Err(format!("the file is only {} bytes: not a firmware", data.len()));
        }
        if data.iter().all(|&b| b == 0xFF) || data.iter().all(|&b| b == 0) {
            return Err("the file is blank (all 0x00 or all 0xFF)".into());
        }
        let (version, build_ts) = ini.map(parse_ini).unwrap_or((None, None));
        if ini.is_some() && (version.is_none() || build_ts.is_none()) {
            return Err("fw.ini has no usable fw_ver / fw_buildts".into());
        }
        Ok(Self { data, version, build_ts })
    }
}

/// `1793184971` -> `2026-10-28 10:56 UTC`
pub fn format_ts(ts: u32) -> String {
    let days = (ts / 86_400) as i64;
    let secs = ts % 86_400;
    // civil date from days since 1970-01-01 (Howard Hinnant's algorithm)
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{y:04}-{m:02}-{d:02} {:02}:{:02} UTC", secs / 3600, secs % 3600 / 60)
}

/// How a firmware file relates to what the reader runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Comparison {
    Same,
    Newer,
    Older,
    Unknown,
}

/// Compares the build time of the file with the one the reader reports.
pub fn compare(reader_ts: u32, file: &FirmwareFile) -> Comparison {
    match file.build_ts {
        None => Comparison::Unknown,
        Some(f) if f == reader_ts => Comparison::Same,
        Some(f) if f > reader_ts => Comparison::Newer,
        Some(_) => Comparison::Older,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn crc16_matches_the_modbus_check_value() {
        assert_eq!(crc16(b"123456789"), 0x4B37);
        assert_eq!(crc16(&[]), 0xFFFF);
    }

    #[test]
    fn packets_have_the_documented_layout_and_even_length() {
        let p = pack_packet(0, 1, command::CONNECT, &[]);
        assert_eq!(p, [0x48, 0x48, 0x4A, 0x4A, 0, 0, 1, 0, 0x21, 0, 0, 0x4A, 0x4A, 0x48, 0x48, 0]);
        assert_eq!(p.len() % 2, 0);
        let p = pack_packet(0, 2, command::WRITE_PAGE, &[9]);
        assert_eq!(p.len(), 16, "odd length is padded with a zero byte");
    }

    #[test]
    fn timestamps_format_as_utc_dates() {
        assert_eq!(format_ts(1_793_184_971), "2026-10-28 10:56 UTC");
        assert_eq!(format_ts(0), "1970-01-01 00:00 UTC");
        assert_eq!(format_ts(1_730_592_000), "2024-11-03 00:00 UTC");
    }

    #[test]
    fn the_ini_is_parsed_and_compared_with_the_reader() {
        let (v, ts) = parse_ini("[Firmware]\nfw_ver = L16\nfw_buildts = 1793184971\n");
        assert_eq!((v.as_deref(), ts), (Some("L16"), Some(1_793_184_971)));
        let f = FirmwareFile::check(vec![0x12; 4096], Some("fw_ver = L16\nfw_buildts = 1793184971")).unwrap();
        assert_eq!(compare(1_700_000_000, &f), Comparison::Newer);
        assert_eq!(compare(1_793_184_971, &f), Comparison::Same);
        assert_eq!(compare(1_800_000_000, &f), Comparison::Older);
        assert_eq!(compare(1, &FirmwareFile::check(vec![1; 4096], None).unwrap()), Comparison::Unknown);
    }

    #[test]
    fn implausible_files_are_refused() {
        assert!(FirmwareFile::check(vec![], None).is_err());
        assert!(FirmwareFile::check(vec![1; 100], None).is_err(), "too small");
        assert!(FirmwareFile::check(vec![1; MAX_FIRMWARE_SIZE + 1], None).is_err(), "too big");
        assert!(FirmwareFile::check(vec![0xFF; 4096], None).is_err(), "blank");
        assert!(FirmwareFile::check(vec![0; 4096], None).is_err(), "blank");
        assert!(FirmwareFile::check(vec![1; 4096], Some("nothing here")).is_err(), "ini without a version");
    }

    /// A bootloader that follows the protocol and keeps what it receives.
    struct Sim {
        out: Vec<u8>,
        pending: std::collections::VecDeque<u8>,
        pages: Vec<Vec<u8>>,
        finished: bool,
        corrupt_crc_of_page: Option<u16>,
        page_size: u16,
    }
    impl Sim {
        fn new(page_size: u16) -> Self {
            Self { out: vec![], pending: Default::default(), pages: vec![], finished: false, corrupt_crc_of_page: None, page_size }
        }
        fn respond(&mut self, seq: u16, command: u16, payload: &[u8]) {
            let mut v = Vec::new();
            v.extend_from_slice(&INTRO.to_be_bytes());
            v.push(1);
            v.extend_from_slice(&seq.to_be_bytes());
            v.extend_from_slice(&command.to_be_bytes());
            v.extend_from_slice(&(payload.len() as u16).to_be_bytes());
            v.extend_from_slice(payload);
            v.extend_from_slice(&OUTRO.to_be_bytes());
            self.pending.extend(v);
        }
    }
    impl Transport for Sim {
        fn write_all(&mut self, data: &[u8]) -> crate::Result<()> {
            self.out.extend_from_slice(data);
            let d = data;
            let seq = u16::from_be_bytes([d[5], d[6]]);
            let cmd = u16::from_be_bytes([d[7], d[8]]);
            let len = u16::from_be_bytes([d[9], d[10]]) as usize;
            let payload = d[11..11 + len].to_vec();
            match cmd {
                command::CONNECT => {
                    let mut p = vec![0, 0, 3, 0x40, 0x00, 0, 0];
                    p.extend_from_slice(&self.page_size.to_be_bytes());
                    self.respond(seq, cmd, &p);
                }
                command::WRITE_PAGE => {
                    let index = u16::from_be_bytes([payload[0], payload[1]]);
                    let n = u16::from_be_bytes([payload[2], payload[3]]) as usize;
                    let data = payload[4..4 + n].to_vec();
                    let crc = u16::from_be_bytes([payload[4 + n], payload[5 + n]]);
                    let ok = crc == crc16(&data) && self.corrupt_crc_of_page != Some(index);
                    if ok {
                        self.pages.push(data);
                    }
                    self.respond(seq, cmd, &[(index >> 8) as u8, index as u8, ok as u8]);
                }
                command::FINISH => {
                    let all: Vec<u8> = self.pages.concat();
                    let crc = u16::from_be_bytes([payload[0], payload[1]]);
                    let inv = u16::from_be_bytes([payload[2], payload[3]]);
                    self.finished = crc == crc16(&all) && inv == !crc;
                    self.respond(seq, cmd, &[self.finished as u8]);
                }
                _ => {}
            }
            Ok(())
        }
        fn read(&mut self, buf: &mut [u8]) -> crate::Result<usize> {
            let n = buf.len().min(self.pending.len());
            for b in buf.iter_mut().take(n) {
                *b = self.pending.pop_front().unwrap();
            }
            Ok(n)
        }
        fn set_timeout(&mut self, _: Duration) -> crate::Result<()> { Ok(()) }
        fn set_baud(&mut self, _: u32) -> crate::Result<()> { Ok(()) }
        fn clear_input(&mut self) -> crate::Result<()> { self.pending.clear(); Ok(()) }
    }

    fn image() -> Vec<u8> {
        (0..2500u32).map(|i| (i * 7 % 251) as u8).collect()
    }

    #[test]
    fn connect_reads_the_sizes_and_a_full_update_reaches_the_bootloader_intact() {
        let mut sim = Sim::new(1024);
        let info = connect_bootloader(&mut sim).unwrap();
        assert_eq!(info, BootloaderInfo { program_size: 0x4000, page_size: 1024 });
        let fw = image();
        let mut last = (0, 0);
        write_firmware(&mut sim, info, &fw, &mut |d, t| last = (d, t)).unwrap();
        assert_eq!(last, (3, 3), "2500 bytes are 3 pages of 1024");
        assert_eq!(sim.pages.concat(), fw, "the bootloader holds exactly the image, the last page being short");
        assert!(sim.finished);
    }

    #[test]
    fn a_page_with_a_bad_crc_stops_the_update() {
        let mut sim = Sim::new(1024);
        let info = connect_bootloader(&mut sim).unwrap();
        sim.corrupt_crc_of_page = Some(2);
        let r = write_firmware(&mut sim, info, &image(), &mut |_, _| {});
        assert!(r.unwrap_err().contains("refused page 2"));
        assert!(!sim.finished, "the final check is never sent after a refused page");
    }

    #[test]
    fn something_that_is_not_a_bootloader_is_refused() {
        struct Silent;
        impl Transport for Silent {
            fn write_all(&mut self, _: &[u8]) -> crate::Result<()> { Ok(()) }
            fn read(&mut self, _: &mut [u8]) -> crate::Result<usize> { Ok(0) }
            fn set_timeout(&mut self, _: Duration) -> crate::Result<()> { Ok(()) }
            fn set_baud(&mut self, _: u32) -> crate::Result<()> { Ok(()) }
            fn clear_input(&mut self) -> crate::Result<()> { Ok(()) }
        }
        assert!(connect_bootloader(&mut Silent).is_err());
        assert_eq!(request_bootloader(&mut Silent).unwrap(), false, "a firmware that stays silent does not support it");
    }
}
