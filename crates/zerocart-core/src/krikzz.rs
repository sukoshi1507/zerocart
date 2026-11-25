// SPDX-License-Identifier: GPL-3.0-or-later
//! Krikzz's Mega Drive cartridge programmer (the "FlashKit MD": FTDI USB serial, byte-oriented command stream).
//!
//! Two parts:
//! - [`FlashKit`]: the wire protocol and the READ side (header, ROM dump, flash chip identification, save RAM
//!   read) and the WRITE side (CFI geometry, sector erase, programming, verification). It is NOT wired into the
//!   application yet.
//! - [`KrikzzMd`]: the PLACEHOLDER of the application's reader (`CartReader`): every operation still answers
//!   [`Error::Planned`] ("Mega Drive : bientôt") and nothing opens a port, so no behaviour depends on it.
//!
//! Written from the protocol and cartridge notes of https://github.com/jfryman/flashkit-md-dotnet (MIT, see
//! THIRD_PARTY.md and docs/ROADMAP.md).
use crate::device::{CartMode, DumpSums};
use crate::mapper::DmgMapper;
use crate::megadrive::{self, Checksum16, MdHeader};
use crate::reader::{CartReader, ReaderInfo, ReaderKind};
use crate::transport::{read_exact, Transport};
use crate::{Error, Result};
use std::time::Duration;

/// FTDI's USB vendor id: the programmer is an FTDI USB-serial device (`/dev/ttyUSB*`).
pub const USB_VID_FTDI: u16 = 0x0403;
/// Product id of the user's board (read 2026-11-25: FTDI "FT245R USB FIFO", 0403:6001, `/dev/ttyUSB0`). It is the
/// generic FT232R/FT245R id, shared by other FTDI adapters: a port found by it is only a candidate, [`FlashKit::connect`]
/// checks the programmer's id before anything else is sent.
pub const USB_PID: u16 = 0x6001;

/// `/dev/ttyUSB*` with the USB ids of the programmer (candidates: see [`USB_PID`]).
pub fn find_ports() -> Vec<String> {
    crate::transport::find_ports_in(std::path::Path::new("/sys/class/tty"), &[(USB_VID_FTDI, USB_PID)])
}

mod wire {
    pub const CMD_ADDR: u8 = 0;
    pub const CMD_LEN: u8 = 1;
    pub const CMD_RD: u8 = 2;
    pub const CMD_WR: u8 = 3;
    /// Wait for the flash chip to be ready (its RY/BY# line).
    pub const CMD_RY: u8 = 4;
    pub const CMD_DELAY: u8 = 5;
    /// Auto-increment the address (block transfer).
    pub const PAR_INC: u8 = 128;
    /// A single word.
    pub const PAR_SINGLE: u8 = 64;
    /// Read the programmer's id instead of the bus.
    pub const PAR_DEV_ID: u8 = 32;
    /// 8-bit write.
    pub const PAR_MODE8: u8 = 16;
}
use wire::*;

/// The bank register that maps the save RAM (0xFFFF) or the ROM (0x0000) at 0x200000.
pub const BANK_REGISTER: u32 = 0xA13000;
/// Where the save RAM appears when the bank register says so.
pub const SAVE_WINDOW: u32 = 0x200000;
/// Bytes per block command (the protocol counts words in 16 bits: 32768 words).
pub const BLOCK: usize = 65536;
/// Bytes per read when dumping a ROM (progress granularity).
pub const DUMP_BLOCK: usize = 32768;
const CFI_QRY: [u8; 6] = [0x00, 0x51, 0x00, 0x52, 0x00, 0x59];

fn invalid(msg: &'static str) -> Error {
    Error::Io(std::io::Error::new(std::io::ErrorKind::InvalidData, msg))
}

/// What the read-only identification of the flash chip found (CFI query and AMD/Spansion autoselect).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FlashIdInfo {
    /// The chip answered the CFI query with "QRY".
    pub cfi_ok: bool,
    /// Chip size from the CFI table (2^n bytes), 0 when unknown.
    pub size_bytes: usize,
    pub manufacturer: u16,
    /// One word, or three for the AMD extended-id scheme (first word ending in 0x7E); empty without autoselect.
    pub device_id: Vec<u16>,
    /// A pair of 8-bit chips where only the low-byte one answered: the ids are byte values (repro carts).
    pub byte_wide: bool,
}

/// What a ROM dump returns: the CRC-32 and the Mega Drive checksum computed over it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MdDump {
    pub crc32: u32,
    /// To compare with the header's checksum word.
    pub checksum16: u16,
}

/// The flash chip's layout from its CFI table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FlashGeometry {
    /// CFI primary command set: 2 = AMD/Spansion, the only one the erase and program sequences here are for.
    pub command_set: u16,
    pub size_bytes: usize,
    /// Write buffer of the chip in bytes (0 when it has none). Not used: programming is word by word.
    pub buffer_bytes: usize,
    /// `(offset, size)` of every erase sector, in address order, covering the whole chip.
    pub sectors: Vec<(u32, u32)>,
}

/// What [`FlashKit::flash_rom`] did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct FlashReport {
    pub rewritten: usize,
    pub unchanged: usize,
    /// Sectors that needed a second attempt.
    pub retried: usize,
}

/// Bytes of one programming block (the reference client programs 4 KiB at a time).
pub const PROGRAM_BLOCK: usize = 4096;
/// A sector erase answers only when the chip is ready again (up to a few seconds on a large sector).
pub const READY_TIMEOUT: Duration = Duration::from_secs(60);

pub struct FlashKit<T: Transport> {
    t: T,
}

impl<T: Transport> FlashKit<T> {
    /// Opens the programmer: asks for its id, which is valid when both bytes are equal and not zero. Then the bus delay
    /// is set to 0 and the timeout raised from 200 ms (probe) to 2 s (transfers).
    pub fn connect(mut t: T) -> Result<Self> {
        t.set_timeout(Duration::from_millis(200))?;
        t.clear_input()?;
        t.write_all(&[CMD_RD | PAR_SINGLE | PAR_DEV_ID])?;
        let mut id = [0u8; 2];
        read_exact(&mut t, &mut id).map_err(|e| match e {
            Error::ShortRead { got: 0, .. } => Error::NoAnswer,
            e => e,
        })?;
        if id[0] != id[1] || id[0] == 0 {
            return Err(invalid("not a FlashKit MD programmer (bad id)"));
        }
        let mut k = Self { t };
        k.set_delay(0)?;
        k.t.set_timeout(Duration::from_secs(2))?;
        Ok(k)
    }

    pub fn into_inner(self) -> T {
        self.t
    }

    /// Sets the bus delay (0 for ROM transfers and flash, 1 for save RAM and header reads, as the reference client).
    pub fn set_delay(&mut self, value: u8) -> Result<()> {
        self.t.write_all(&[CMD_DELAY, value])
    }

    fn addr_cmds(byte_addr: u32) -> [u8; 6] {
        let a = byte_addr / 2; // the wire counts words
        [CMD_ADDR, (a >> 16) as u8, CMD_ADDR, (a >> 8) as u8, CMD_ADDR, a as u8]
    }

    pub fn set_addr(&mut self, byte_addr: u32) -> Result<()> {
        self.t.write_all(&Self::addr_cmds(byte_addr))
    }

    pub fn read_word(&mut self, byte_addr: u32) -> Result<u16> {
        let mut cmd = Self::addr_cmds(byte_addr).to_vec();
        cmd.push(CMD_RD | PAR_SINGLE);
        self.t.write_all(&cmd)?;
        let mut w = [0u8; 2];
        read_exact(&mut self.t, &mut w)?;
        Ok(u16::from_be_bytes(w))
    }

    /// A word write on the cartridge bus. This is how bank registers and flash commands are sent: callers that are
    /// not read-only helpers must not use it on a cartridge they do not mean to change.
    pub fn write_word(&mut self, byte_addr: u32, value: u16) -> Result<()> {
        let mut cmd = Self::addr_cmds(byte_addr).to_vec();
        cmd.push(CMD_WR | PAR_SINGLE);
        cmd.extend_from_slice(&value.to_be_bytes());
        self.t.write_all(&cmd)
    }

    /// An 8-bit write (flash commands: the high byte of the word is 0).
    pub fn write_byte(&mut self, byte_addr: u32, value: u8) -> Result<()> {
        let mut cmd = Self::addr_cmds(byte_addr).to_vec();
        cmd.push(CMD_WR | PAR_SINGLE | PAR_MODE8);
        cmd.push(value);
        self.t.write_all(&cmd)
    }

    /// Reads `len` bytes (even) from `byte_addr` with the address auto-incrementing: big-endian words.
    pub fn read(&mut self, byte_addr: u32, len: usize) -> Result<Vec<u8>> {
        if len % 2 != 0 || byte_addr % 2 != 0 {
            return Err(invalid("the Mega Drive bus is read in 16-bit words: even address and length"));
        }
        self.set_addr(byte_addr)?;
        let mut out = vec![0u8; len];
        let mut done = 0;
        while done < len {
            let n = (len - done).min(BLOCK);
            let words = (n / 2) as u16;
            self.t.write_all(&[CMD_LEN, (words >> 8) as u8, CMD_LEN, words as u8, CMD_RD | PAR_INC])?;
            read_exact(&mut self.t, &mut out[done..done + n])?;
            done += n;
        }
        Ok(out)
    }

    /// The first 512 bytes of the cartridge (header), with the ROM mapped in.
    pub fn read_header_bytes(&mut self) -> Result<Vec<u8>> {
        self.set_delay(1)?;
        self.write_word(BANK_REGISTER, 0x0000)?;
        self.read(0, megadrive::HEADER_LEN)
    }

    pub fn read_header(&mut self) -> Result<Option<MdHeader>> {
        Ok(megadrive::parse_header(&self.read_header_bytes()?))
    }

    /// Dumps `size` bytes of ROM into `out`, 32 KiB at a time. `progress(done)` returns false to cancel. The size is the
    /// caller's choice: the header's (see `MdHeader::rom_size`) is far more reliable than probing mirrors on a flash
    /// cartridge.
    pub fn dump_rom(
        &mut self,
        size: usize,
        out: &mut dyn std::io::Write,
        progress: &mut dyn FnMut(usize) -> bool,
    ) -> Result<MdDump> {
        if size == 0 || size % 2 != 0 || size > megadrive::MAX_CART_BYTES {
            return Err(invalid("invalid ROM size"));
        }
        self.set_delay(1)?;
        self.write_word(BANK_REGISTER, 0x0000)?;
        let (mut crc, mut sum) = (crc32fast::Hasher::new(), Checksum16::default());
        let mut pos = 0;
        while pos < size {
            let n = DUMP_BLOCK.min(size - pos);
            let block = self.read(pos as u32, n)?;
            crc.update(&block);
            sum.update(&block);
            out.write_all(&block)?;
            pos += n;
            if !progress(pos) {
                return Err(Error::Cancelled);
            }
        }
        Ok(MdDump { crc32: crc.finalize(), checksum16: sum.finalize() })
    }

    /// Reads the save RAM as the bus shows it: `save_bytes` of data are on the ODD bytes of a word stream of
    /// `2 * save_bytes` bytes (even bytes read as open bus). The ROM is mapped back at the end. Only the bank register
    /// is written.
    pub fn read_save_stream(&mut self, save_bytes: usize) -> Result<Vec<u8>> {
        if save_bytes == 0 {
            return Err(invalid("no save RAM"));
        }
        self.set_delay(1)?;
        self.write_word(BANK_REGISTER, 0xFFFF)?;
        let stream = self.read(SAVE_WINDOW, save_bytes * 2);
        let _ = self.write_word(BANK_REGISTER, 0x0000);
        stream
    }

    /// Read-only identification of the flash chip: the CFI query (word 0x10 must read "QRY", the size is 2^n at word
    /// 0x27) and the AMD/Spansion autoselect (manufacturer at word 0, device id at words 1, 0x0E, 0x0F). It sends
    /// only query, autoselect and reset (0xF0) commands, never erase or program, and always leaves the chip in
    /// read-array mode. `None`: nothing answered (empty connector, mask ROM).
    pub fn probe_flash(&mut self) -> Result<Option<FlashIdInfo>> {
        let probe = (|| -> Result<(Vec<u8>, Vec<u8>, Vec<u8>, bool)> {
            let array = self.read(0, 0x20)?;
            self.write_byte(0x55 * 2, 0x98)?;
            let cfi = self.read(0x20, 6)?;
            let cfi_ok = cfi == CFI_QRY;
            let size = if cfi_ok { self.read(0x4E, 2)? } else { vec![0, 0] };
            self.write_word(0, 0xF0)?;
            self.write_word(0x555 * 2, 0xAA)?;
            self.write_word(0x2AA * 2, 0x55)?;
            self.write_word(0x555 * 2, 0x90)?;
            let ids = self.read(0, 0x20)?;
            self.write_word(0, 0xF0)?;
            Ok((array, ids, size, cfi_ok))
        })();
        let (array, ids, size, cfi_ok) = match probe {
            Ok(v) => v,
            Err(e) => {
                let _ = self.write_word(0, 0xF0); // best effort: back to read-array mode
                return Err(e);
            }
        };
        let word = |b: &[u8], a: usize| u16::from_be_bytes([b[a], b[a + 1]]);
        let id_addrs = [0x00usize, 0x02, 0x1C, 0x1E];
        let id_ok = id_addrs.iter().any(|&a| word(&ids, a) != word(&array, a));
        if !cfi_ok && !id_ok {
            return Ok(None);
        }
        let size_bytes = if cfi_ok && (10..=30).contains(&size[1]) { 1usize << size[1] } else { 0 };
        if !id_ok {
            return Ok(Some(FlashIdInfo { cfi_ok, size_bytes, manufacturer: 0, device_id: vec![], byte_wide: false }));
        }
        // Repro carts often pair two 8-bit chips: the commands go out with a zero high byte, so only the low-byte chip
        // enters autoselect and the high bytes still read array data.
        let byte_wide = id_addrs.iter().all(|&a| ids[a] == array[a]);
        let id = |a: usize| if byte_wide { ids[a + 1] as u16 } else { word(&ids, a) };
        let dev1 = id(0x02);
        // Words 2 and 3 mean something only for the extended-id scheme; otherwise they hold unrelated autoselect data.
        let device_id = if dev1 & 0xFF == 0x7E { vec![dev1, id(0x1C), id(0x1E)] } else { vec![dev1] };
        Ok(Some(FlashIdInfo { cfi_ok, size_bytes, manufacturer: id(0x00), device_id, byte_wide }))
    }

    /// Reads the chip's CFI table: command set, size, write buffer and the erase sectors. Only the CFI query and
    /// the reset command are sent. `None`: the chip does not answer the query.
    pub fn read_geometry(&mut self) -> Result<Option<FlashGeometry>> {
        let table = (|| -> Result<Vec<u8>> {
            self.write_byte(0x55 * 2, 0x98)?;
            let t = self.read(0x20, 0x5A)?; // words 0x10..=0x3A
            self.write_word(0, 0xF0)?;
            Ok(t)
        })();
        let table = match table {
            Ok(t) => t,
            Err(e) => {
                let _ = self.write_word(0, 0xF0);
                return Err(e);
            }
        };
        let w = |i: usize| table[(i - 0x10) * 2 + 1] as usize; // CFI data is in the low byte of each word
        if table[..6] != CFI_QRY {
            return Ok(None);
        }
        let command_set = (w(0x13) | w(0x14) << 8) as u16;
        let size_exp = w(0x27);
        if !(10..=30).contains(&size_exp) {
            return Ok(None);
        }
        let size_bytes = 1usize << size_exp;
        let buffer_exp = w(0x2A);
        let buffer_bytes = if (1..=12).contains(&buffer_exp) { 1usize << buffer_exp } else { 0 };
        let regions = w(0x2C);
        let mut sectors = Vec::new();
        let mut offset = 0u64;
        for r in 0..regions {
            let count = (w(0x2D + 4 * r) | w(0x2E + 4 * r) << 8) + 1;
            let size = (w(0x2F + 4 * r) | w(0x30 + 4 * r) << 8) * 256;
            if size == 0 {
                return Ok(None);
            }
            for _ in 0..count {
                sectors.push((offset as u32, size as u32));
                offset += size as u64;
            }
        }
        // a table that does not add up to the chip is not trusted: nothing would be erased from it
        if regions == 0 || offset != size_bytes as u64 {
            return Ok(None);
        }
        Ok(Some(FlashGeometry { command_set, size_bytes, buffer_bytes, sectors }))
    }

    /// Back to read-array mode, also out of unlock-bypass mode (the reference client's `flashResetByPass`).
    pub fn flash_reset(&mut self) -> Result<()> {
        self.write_word(0, 0xF0)?;
        self.write_byte(0, 0x90)?;
        self.write_byte(0, 0x00)
    }

    /// Waits for the chip (RY/BY#): the programmer answers once it is ready.
    fn wait_ready(&mut self) -> Result<()> {
        self.t.set_timeout(READY_TIMEOUT)?;
        let r = (|| {
            self.t.write_all(&[CMD_RY, CMD_RD | PAR_SINGLE])?;
            let mut w = [0u8; 2];
            read_exact(&mut self.t, &mut w).map_err(|e| match e {
                Error::ShortRead { got: 0, .. } => Error::NoAnswer,
                e => e,
            })
        })();
        self.t.set_timeout(Duration::from_secs(2))?;
        r
    }

    /// DESTRUCTIVE. Erases the sector that starts at `offset` and waits for the end of the erase.
    pub fn erase_sector(&mut self, offset: u32) -> Result<()> {
        self.flash_reset()?;
        for (a, v) in [(0x555, 0xAA), (0x2AA, 0x55), (0x555, 0x80), (0x555, 0xAA), (0x2AA, 0x55)] {
            self.write_word(a * 2, v)?;
        }
        self.write_byte(offset, 0x30)?;
        self.wait_ready()
    }

    /// DESTRUCTIVE. Programs `data` (even length) at the even byte address `offset` word by word, in unlock-bypass
    /// mode, waiting for the chip after every word. The target must be erased: programming only clears bits.
    pub fn program(&mut self, offset: u32, data: &[u8]) -> Result<()> {
        if offset % 2 != 0 || data.len() % 2 != 0 || data.is_empty() {
            return Err(invalid("programming needs an even address and a non-empty even length"));
        }
        self.write_byte(0x555 * 2, 0xAA)?;
        self.write_byte(0x2AA * 2, 0x55)?;
        self.write_byte(0x555 * 2, 0x20)?; // unlock bypass: from now on a program command is just A0
        let result = (|| {
            self.set_addr(offset)?;
            for block in data.chunks(PROGRAM_BLOCK) {
                let mut cmd = Vec::with_capacity(block.len() * 3);
                for w in block.chunks(2) {
                    cmd.extend_from_slice(&[CMD_WR | PAR_SINGLE | PAR_MODE8, 0xA0, CMD_WR | PAR_SINGLE | PAR_INC, w[0], w[1], CMD_RY]);
                }
                self.t.write_all(&cmd)?;
            }
            Ok(())
        })();
        let reset = self.flash_reset();
        result.and(reset)
    }

    /// DESTRUCTIVE. Writes `image` at offset 0 of the flash cartridge: the sectors that hold it are compared with the
    /// image (padded with 0xFF to the end of its last sector) and only the ones that differ are erased and
    /// programmed, each read back (one retry). `geometry` must come from [`read_geometry`](Self::read_geometry) of
    /// THIS cartridge. `cancel` is checked between sectors, never inside one. `progress(message, done, total)`
    /// counts sectors. Whole-image verification and the backup of what is overwritten are the caller's job.
    pub fn flash_rom(
        &mut self,
        geometry: &FlashGeometry,
        image: &[u8],
        progress: &mut dyn FnMut(&str, usize, usize),
        cancel: &dyn Fn() -> bool,
    ) -> Result<FlashReport> {
        if geometry.command_set != 2 {
            return Err(invalid("only the AMD/Spansion command set is supported"));
        }
        if image.is_empty() || image.len() % 2 != 0 || image.len() > geometry.size_bytes {
            return Err(invalid("the image is empty, odd, or larger than the flash chip: nothing is erased"));
        }
        let used: Vec<(u32, u32)> = geometry.sectors.iter().copied().filter(|s| (s.0 as usize) < image.len()).collect();
        let target = |off: u32, size: u32| -> Vec<u8> {
            let (o, e) = (off as usize, (off as usize + size as usize).min(image.len()));
            let mut t = image[o..e].to_vec();
            t.resize(size as usize, 0xFF);
            t
        };
        self.set_delay(0)?;
        self.write_word(BANK_REGISTER, 0x0000)?;
        let mut report = FlashReport::default();
        let result = (|| -> Result<()> {
            for (i, &(off, size)) in used.iter().enumerate() {
                if cancel() {
                    return Err(Error::Cancelled);
                }
                let want = target(off, size);
                if self.read(off, size as usize)? == want {
                    report.unchanged += 1;
                    progress("unchanged", i + 1, used.len());
                    continue;
                }
                let mut attempt = 0;
                loop {
                    attempt += 1;
                    progress(if attempt == 1 { "erasing and programming" } else { "again" }, i, used.len());
                    self.erase_sector(off)?;
                    if self.read(off, size as usize)?.iter().any(|&b| b != 0xFF) {
                        if attempt < 2 {
                            continue;
                        }
                        return Err(invalid("a sector did not erase"));
                    }
                    self.program(off, &want)?;
                    if self.read(off, size as usize)? == want {
                        break;
                    }
                    if attempt >= 2 {
                        return Err(invalid("a sector still differs after two attempts"));
                    }
                }
                if attempt > 1 {
                    report.retried += 1;
                }
                report.rewritten += 1;
                progress("done", i + 1, used.len());
            }
            Ok(())
        })();
        if result.is_err() {
            let _ = self.flash_reset(); // best effort: leave the chip in read-array mode
        }
        result.map(|_| report)
    }
}

/// The placeholder of the application's reader: see the module documentation.
pub struct KrikzzMd;

fn soon<T>() -> Result<T> {
    Err(Error::Planned("Mega Drive"))
}

impl CartReader for KrikzzMd {
    fn kind(&self) -> ReaderKind {
        ReaderKind::KrikzzMd
    }
    fn info(&self, _mode: CartMode) -> ReaderInfo {
        ReaderInfo { name: "Krikzz Mega Drive".into(), firmware: "bientôt".into(), voltage: "" }
    }
    fn read_header(&mut self, _mode: CartMode) -> Result<Vec<u8>> {
        soon()
    }
    fn read_agb_rom(&mut self, _address: u32, _len: usize) -> Result<Vec<u8>> {
        soon()
    }
    fn dump_dmg_rom(
        &mut self,
        _mapper: DmgMapper,
        _size: usize,
        _out: &mut dyn std::io::Write,
        _progress: &mut dyn FnMut(usize) -> bool,
    ) -> Result<DumpSums> {
        soon()
    }
    fn read_dmg_ram(&mut self, _mapper: DmgMapper, _size: usize, _progress: &mut dyn FnMut(usize) -> bool) -> Result<Vec<u8>> {
        soon()
    }
    fn write_dmg_ram(&mut self, _mapper: DmgMapper, _data: &[u8], _progress: &mut dyn FnMut(usize) -> bool) -> Result<()> {
        soon()
    }
    fn read_agb_sram(&mut self, _address: u32, _len: usize) -> Result<Vec<u8>> {
        soon()
    }
    fn write_agb_sram(&mut self, _data: &[u8], _progress: &mut dyn FnMut(usize) -> bool) -> Result<()> {
        soon()
    }
    fn can_power_cycle(&self) -> bool {
        false
    }
    fn power_cycle(&mut self, _off: Duration) -> Result<bool> {
        soon()
    }
    fn close(&mut self) -> Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;

    #[derive(PartialEq, Clone, Copy, Debug)]
    enum Chip {
        Array,
        Cfi,
        Autoselect,
        /// After AA/55/80/AA/55: the next 30 erases a sector.
        EraseArmed,
        /// Unlock bypass: A0 starts a program, 90 then 00 leaves.
        Bypass,
        BypassProgram,
        BypassExit,
    }

    const SECTOR: usize = 0x10000;

    /// The programmer and a cartridge, simulated from the protocol: parses the command stream the way the firmware does
    /// (commands can be split over several writes), a ROM, a save RAM on odd bytes behind the bank register, and a
    /// flash chip that answers the CFI query and autoselect.
    struct Sim {
        id: [u8; 2],
        rom: Vec<u8>,
        sram: Vec<u8>,
        bank: u16,
        addr: u32,
        len: u32,
        delay: u8,
        rx: Vec<u8>,
        out: VecDeque<u8>,
        chip: Chip,
        /// The chip answers CFI and autoselect (a flash cartridge) or not (mask ROM).
        flash: bool,
        unlock: Vec<(u32, u16)>,
        writes: Vec<(u32, u16)>,
        log: Vec<u8>,
        /// Start offsets of the sectors erased, in order.
        erased: Vec<usize>,
    }

    impl Sim {
        fn new(rom: Vec<u8>) -> Self {
            Self {
                id: [0x42, 0x42], rom, sram: vec![0; 0x4000], bank: 0, addr: 0, len: 0, delay: 0, rx: vec![],
                out: VecDeque::new(), chip: Chip::Array, flash: false, unlock: vec![], writes: vec![], log: vec![], erased: vec![],
            }
        }

        fn rom_word(&self, a: u32) -> u16 {
            let i = a as usize * 2;
            match self.rom.get(i..i + 2) {
                Some(b) => u16::from_be_bytes([b[0], b[1]]),
                None => 0xFFFF,
            }
        }

        fn read_word(&self, a: u32) -> u16 {
            if a >= 0x100000 && self.bank == 0xFFFF {
                let i = (a - 0x100000) as usize;
                return 0xFF00 | *self.sram.get(i).unwrap_or(&0xFF) as u16;
            }
            match (self.chip, a) {
                (Chip::Cfi, 0x10) => 0x0051,
                (Chip::Cfi, 0x11) => 0x0052,
                (Chip::Cfi, 0x12) => 0x0059,
                (Chip::Cfi, 0x13) => 0x0002, // AMD command set
                (Chip::Cfi, 0x27) => self.rom.len().trailing_zeros() as u16,
                (Chip::Cfi, 0x2C) => 1, // one region of uniform 64 KiB sectors
                (Chip::Cfi, 0x2D) => (self.rom.len() / SECTOR - 1) as u16,
                (Chip::Cfi, 0x30) => 0x0001,
                (Chip::Autoselect, 0x00) => 0x0001,
                (Chip::Autoselect, 0x01) => 0x227E,
                (Chip::Autoselect, 0x0E) => 0x2210,
                (Chip::Autoselect, 0x0F) => 0x2200,
                _ => self.rom_word(a),
            }
        }

        fn write_word(&mut self, a: u32, v: u16) {
            self.writes.push((a, v));
            if a == BANK_REGISTER / 2 {
                self.bank = v;
                return;
            }
            if a >= 0x100000 && self.bank == 0xFFFF {
                let i = (a - 0x100000) as usize;
                if i < self.sram.len() {
                    self.sram[i] = v as u8;
                }
                return;
            }
            if !self.flash {
                return;
            }
            match (self.chip, a, v) {
                (Chip::BypassProgram, _, _) => {
                    // programming only clears bits (AND), like a real chip
                    let i = a as usize * 2;
                    if i + 1 < self.rom.len() {
                        self.rom[i] &= (v >> 8) as u8;
                        self.rom[i + 1] &= v as u8;
                    }
                    self.chip = Chip::Bypass;
                }
                (Chip::Bypass, _, 0xA0) => self.chip = Chip::BypassProgram,
                (Chip::Bypass, _, 0x90) => self.chip = Chip::BypassExit,
                (Chip::BypassExit, _, 0x00) => self.chip = Chip::Array,
                (Chip::Bypass | Chip::BypassExit, _, 0xF0) => {} // ignored in bypass mode
                (Chip::EraseArmed, _, 0x30) => {
                    let start = (a as usize * 2) / SECTOR * SECTOR;
                    self.rom[start..start + SECTOR].fill(0xFF);
                    self.chip = Chip::Array;
                    self.erased.push(start);
                }
                (_, 0x55, 0x98) => self.chip = Chip::Cfi,
                (_, _, 0xF0) => {
                    self.chip = Chip::Array;
                    self.unlock.clear();
                }
                (_, 0x555, 0xAA) | (_, 0x2AA, 0x55) => self.unlock.push((a, v)),
                (_, 0x555, 0x90) if self.unlock == [(0x555, 0xAA), (0x2AA, 0x55)] => {
                    self.chip = Chip::Autoselect;
                    self.unlock.clear();
                }
                (_, 0x555, 0x80) if self.unlock == [(0x555, 0xAA), (0x2AA, 0x55)] => {
                    self.unlock = vec![(0x555, 0x80)];
                }
                (_, 0x555, 0x20) if self.unlock == [(0x555, 0xAA), (0x2AA, 0x55)] => {
                    self.chip = Chip::Bypass;
                    self.unlock.clear();
                }
                _ => {}
            }
            // the second unlock pair of an erase arms it
            if self.unlock == [(0x555, 0x80), (0x555, 0xAA), (0x2AA, 0x55)] {
                self.chip = Chip::EraseArmed;
                self.unlock.clear();
            }
        }

        fn run(&mut self) {
            loop {
                let Some(&c) = self.rx.first() else { return };
                let (base, flags) = (c & 0x0F, c & 0xF0);
                let need = match (base, flags) {
                    (CMD_ADDR | CMD_LEN | CMD_DELAY, 0) => 2,
                    (CMD_RY, 0) => 1,
                    (CMD_RD, _) => 1,
                    (CMD_WR, f) if f & PAR_SINGLE != 0 => 1 + if f & PAR_MODE8 != 0 { 1 } else { 2 },
                    (CMD_WR, _) => 1 + self.len as usize * 2,
                    _ => panic!("unknown command byte {c:#04x}"),
                };
                if self.rx.len() < need {
                    return;
                }
                let cmd: Vec<u8> = self.rx.drain(..need).collect();
                self.log.push(c);
                match (base, flags) {
                    (CMD_ADDR, 0) => self.addr = ((self.addr << 8) | cmd[1] as u32) & 0xFF_FFFF,
                    (CMD_LEN, 0) => self.len = ((self.len << 8) | cmd[1] as u32) & 0xFFFF,
                    (CMD_DELAY, 0) => self.delay = cmd[1],
                    (CMD_RY, 0) => {}
                    (CMD_RD, f) if f & PAR_DEV_ID != 0 => self.out.extend(self.id),
                    (CMD_RD, f) if f & PAR_SINGLE != 0 => {
                        self.out.extend(self.read_word(self.addr).to_be_bytes());
                        if f & PAR_INC != 0 {
                            self.addr += 1;
                        }
                    }
                    (CMD_RD, _) => {
                        for _ in 0..self.len {
                            self.out.extend(self.read_word(self.addr).to_be_bytes());
                            self.addr += 1;
                        }
                    }
                    (CMD_WR, f) if f & PAR_SINGLE != 0 => {
                        let v = if f & PAR_MODE8 != 0 { cmd[1] as u16 } else { u16::from_be_bytes([cmd[1], cmd[2]]) };
                        self.write_word(self.addr, v);
                        if f & PAR_INC != 0 {
                            self.addr += 1;
                        }
                    }
                    (CMD_WR, _) => {
                        for w in cmd[1..].chunks(2) {
                            self.write_word(self.addr, u16::from_be_bytes([w[0], w[1]]));
                            self.addr += 1;
                        }
                    }
                    _ => unreachable!(),
                }
            }
        }
    }

    impl Transport for Sim {
        fn write_all(&mut self, data: &[u8]) -> Result<()> {
            self.rx.extend_from_slice(data);
            self.run();
            Ok(())
        }
        fn read(&mut self, buf: &mut [u8]) -> Result<usize> {
            let n = buf.len().min(self.out.len());
            for b in buf.iter_mut().take(n) {
                *b = self.out.pop_front().unwrap();
            }
            Ok(n)
        }
        fn set_timeout(&mut self, _: Duration) -> Result<()> { Ok(()) }
        fn set_baud(&mut self, _: u32) -> Result<()> { Ok(()) }
        fn clear_input(&mut self) -> Result<()> { self.out.clear(); Ok(()) }
    }

    /// A 512 KiB ROM with a valid header and the checksum the header declares.
    fn game() -> Vec<u8> {
        let mut rom = vec![0u8; 0x80000];
        for (i, b) in rom.iter_mut().enumerate().skip(0x200) {
            *b = (i * 13 + (i >> 8)) as u8;
        }
        rom[0x100..0x110].copy_from_slice(b"SEGA MEGA DRIVE ");
        rom[0x150..0x159].copy_from_slice(b"TEST GAME");
        rom[0x1A4..0x1A8].copy_from_slice(&(0x7FFFFu32).to_be_bytes());
        rom[0x1B0..0x1B4].copy_from_slice(&[b'R', b'A', 0xF8, 0x20]);
        rom[0x1B4..0x1B8].copy_from_slice(&0x0020_0001u32.to_be_bytes());
        rom[0x1B8..0x1BC].copy_from_slice(&0x0020_1FFFu32.to_be_bytes()); // 4 KiB of save data
        rom[0x1F0] = b'E';
        let sum = megadrive::checksum16(&rom);
        rom[0x18E..0x190].copy_from_slice(&sum.to_be_bytes());
        rom
    }

    fn kit(rom: Vec<u8>) -> FlashKit<Sim> {
        FlashKit::connect(Sim::new(rom)).unwrap()
    }

    #[test]
    fn ports_are_found_by_the_ftdi_ids_and_the_gbxcart_ch340_is_not_one() {
        let root = std::env::temp_dir().join(format!("zerocart-krikzz-ports-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        for (tty, vid, pid) in [("ttyUSB0", "0403", "6001"), ("ttyUSB1", "1a86", "7523"), ("ttyACM0", "0483", "0721")] {
            let dev = root.join(tty).join("device");
            std::fs::create_dir_all(&dev).unwrap();
            std::fs::write(dev.join("idVendor"), format!("{vid}\n")).unwrap();
            std::fs::write(dev.join("idProduct"), format!("{pid}\n")).unwrap();
        }
        assert_eq!(crate::transport::find_ports_in(&root, &[(USB_VID_FTDI, USB_PID)]), ["/dev/ttyUSB0"]);
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn the_handshake_accepts_an_id_with_two_equal_nonzero_bytes() {
        let mut ok = Sim::new(vec![]);
        ok.id = [0x17, 0x17];
        assert!(FlashKit::connect(ok).is_ok());
        for bad in [[0, 0], [0x17, 0x18]] {
            let mut s = Sim::new(vec![]);
            s.id = bad;
            assert!(FlashKit::connect(s).is_err(), "{bad:?}");
        }
        struct Mute;
        impl Transport for Mute {
            fn write_all(&mut self, _: &[u8]) -> Result<()> { Ok(()) }
            fn read(&mut self, _: &mut [u8]) -> Result<usize> { Ok(0) }
            fn set_timeout(&mut self, _: Duration) -> Result<()> { Ok(()) }
            fn set_baud(&mut self, _: u32) -> Result<()> { Ok(()) }
            fn clear_input(&mut self) -> Result<()> { Ok(()) }
        }
        assert!(matches!(FlashKit::connect(Mute), Err(Error::NoAnswer)));
    }

    #[test]
    fn commands_have_the_wire_format_of_the_reference_client() {
        // From Device.cs: readWord = 7 bytes (3 x [ADDR, byte] then RD|SINGLE), writeWord = 9, addresses in WORDS.
        let w = FlashKit::<Sim>::addr_cmds(0x200000);
        assert_eq!(w, [0, 0x10, 0, 0, 0, 0]);
        let mut k = kit(game());
        k.read_word(0x100).unwrap();
        k.write_word(0x555 * 2, 0xAA).unwrap();
        k.write_byte(0x2AA * 2, 0x55).unwrap();
        let s = k.into_inner();
        // ... connect: DEV_ID read, DELAY, then the three commands above
        assert_eq!(s.log, vec![CMD_RD | PAR_SINGLE | PAR_DEV_ID, CMD_DELAY]
            .into_iter()
            .chain([CMD_ADDR, CMD_ADDR, CMD_ADDR, CMD_RD | PAR_SINGLE])
            .chain([CMD_ADDR, CMD_ADDR, CMD_ADDR, CMD_WR | PAR_SINGLE])
            .chain([CMD_ADDR, CMD_ADDR, CMD_ADDR, CMD_WR | PAR_SINGLE | PAR_MODE8])
            .collect::<Vec<u8>>());
        assert_eq!(s.writes, vec![(0x555, 0xAA), (0x2AA, 0x55)]);
        assert_eq!(s.delay, 0);
    }

    #[test]
    fn blocks_are_read_big_endian_in_chunks_of_64_kib() {
        let rom = game();
        let mut k = kit(rom.clone());
        assert_eq!(k.read_word(0x100).unwrap(), u16::from_be_bytes([rom[0x100], rom[0x101]]));
        assert_eq!(k.read(0, 0x30000).unwrap(), rom[..0x30000], "three chunks of 64 KiB, the 32768 words of one chunk fit the length");
        assert_eq!(k.read(0x7E000, 0x2000).unwrap(), rom[0x7E000..]);
        assert!(k.read(1, 4).is_err() && k.read(0, 3).is_err(), "even address and length");
    }

    #[test]
    fn the_header_is_read_with_the_rom_mapped_in() {
        let rom = game();
        let mut k = kit(rom.clone());
        let h = k.read_header().unwrap().unwrap();
        assert!(h.is_valid());
        assert_eq!((h.title(), h.rom_size(), h.region), ("TEST GAME", Some(0x80000), "Europe"));
        assert_eq!(h.ram.unwrap().size(), 0x1000);
        let s = k.into_inner();
        assert_eq!(s.bank, 0, "the bank register maps the ROM");
        assert_eq!(s.delay, 1);
    }

    #[test]
    fn a_dump_is_the_rom_and_its_checksum_matches_the_header() {
        let rom = game();
        let mut k = kit(rom.clone());
        let h = k.read_header().unwrap().unwrap();
        let mut out = Vec::new();
        let mut last = 0;
        let d = k.dump_rom(h.rom_size().unwrap(), &mut out, &mut |n| {
            last = n;
            true
        })
        .unwrap();
        assert_eq!(out, rom);
        assert_eq!(last, rom.len());
        assert_eq!(d.crc32, crc32fast::hash(&rom));
        assert_eq!(d.checksum16, h.checksum, "the Mega Drive checksum of a good dump equals the header's");
    }

    #[test]
    fn a_cancelled_dump_stops_and_bad_sizes_are_refused_before_any_read() {
        let mut k = kit(game());
        assert!(matches!(k.dump_rom(0x80000, &mut Vec::new(), &mut |n| n < 0x10000), Err(Error::Cancelled)));
        let before = k.into_inner().log.len();
        let mut k = kit(game());
        for bad in [0usize, 3, megadrive::MAX_CART_BYTES + 2] {
            assert!(k.dump_rom(bad, &mut Vec::new(), &mut |_| true).is_err());
        }
        assert!(k.into_inner().log.len() <= before, "nothing was sent for the refused sizes");
    }

    #[test]
    fn the_save_is_read_from_the_odd_bytes_behind_the_bank_register() {
        let mut s = Sim::new(game());
        for (i, b) in s.sram.iter_mut().enumerate().take(0x1000) {
            *b = (i * 5 + 1) as u8;
        }
        let expect = s.sram[..0x1000].to_vec();
        let mut k = FlashKit::connect(s).unwrap();
        let stream = k.read_save_stream(0x1000).unwrap();
        assert_eq!(stream.len(), 0x2000);
        let data: Vec<u8> = stream.chunks(2).map(|w| w[1]).collect();
        assert_eq!(data, expect);
        assert!(stream.chunks(2).all(|w| w[0] == 0xFF), "even bytes read as open bus");
        assert_eq!(k.into_inner().bank, 0, "the ROM is mapped back");
        assert!(kit(game()).read_save_stream(0).is_err());
    }

    #[test]
    fn a_flash_cartridge_is_identified_without_erasing_or_programming() {
        let mut s = Sim::new(game());
        s.flash = true;
        let mut k = FlashKit::connect(s).unwrap();
        let id = k.probe_flash().unwrap().unwrap();
        assert_eq!(
            id,
            FlashIdInfo { cfi_ok: true, size_bytes: 0x80000, manufacturer: 1, device_id: vec![0x227E, 0x2210, 0x2200], byte_wide: false }
        );
        let s = k.into_inner();
        assert_eq!(s.chip, Chip::Array, "the chip is left in read-array mode");
        // only the CFI query, the autoselect unlock and the resets were written
        let allowed = [(0x55, 0x98), (0, 0xF0), (0x555, 0xAA), (0x2AA, 0x55), (0x555, 0x90)];
        assert!(s.writes.iter().all(|w| allowed.contains(w)), "{:?}", s.writes);
        assert!(!s.writes.iter().any(|&(_, v)| matches!(v, 0x30 | 0x80 | 0xA0 | 0x20)), "no erase or program command");
    }

    fn flash_kit() -> FlashKit<Sim> {
        let mut s = Sim::new(game());
        s.flash = true;
        FlashKit::connect(s).unwrap()
    }

    #[test]
    fn the_geometry_comes_from_the_cfi_table_and_a_mask_rom_has_none() {
        let mut k = flash_kit();
        let g = k.read_geometry().unwrap().unwrap();
        assert_eq!((g.command_set, g.size_bytes, g.buffer_bytes), (2, 0x80000, 0));
        assert_eq!(g.sectors, (0..8).map(|i| (i * 0x10000, 0x10000)).collect::<Vec<_>>());
        assert_eq!(k.into_inner().chip, Chip::Array, "left in read-array mode");
        assert_eq!(kit(game()).read_geometry().unwrap(), None);
    }

    #[test]
    fn erasing_a_sector_touches_only_that_sector_and_waits_for_the_chip() {
        let mut k = flash_kit();
        k.erase_sector(0x20000).unwrap();
        let s = k.into_inner();
        assert_eq!(s.erased, vec![0x20000]);
        assert!(s.rom[0x20000..0x30000].iter().all(|&b| b == 0xFF));
        assert_eq!(s.rom[..0x20000], game()[..0x20000]);
        assert_eq!(s.rom[0x30000..], game()[0x30000..]);
        assert!(s.log.contains(&CMD_RY), "the end of the erase is waited for");
    }

    #[test]
    fn programming_only_clears_bits_so_the_sector_must_be_erased_first() {
        let data: Vec<u8> = (0..0x2000usize).map(|i| (i * 7 + 3) as u8).collect();
        let mut k = flash_kit();
        k.erase_sector(0x10000).unwrap();
        k.program(0x10000, &data).unwrap();
        let s = k.into_inner();
        assert_eq!(s.rom[0x10000..0x12000], data[..]);
        assert!(s.rom[0x12000..0x20000].iter().all(|&b| b == 0xFF));
        assert_eq!(s.chip, Chip::Array, "bypass mode is left");
        // without an erase, the old content ANDs with the new
        let mut k = flash_kit();
        k.program(0, &[0xF0, 0x0F, 0xFF, 0xFF]).unwrap();
        let s = k.into_inner();
        assert_eq!(s.rom[..4], [game()[0] & 0xF0, game()[1] & 0x0F, game()[2], game()[3]]);
        assert!(flash_kit().program(1, &[0, 0]).is_err() && flash_kit().program(0, &[0]).is_err() && flash_kit().program(0, &[]).is_err());
    }

    #[test]
    fn flashing_rewrites_only_the_sectors_that_differ_and_pads_the_last_one() {
        let mut k = flash_kit();
        let g = k.read_geometry().unwrap().unwrap();
        let mut image = game()[..0x28000].to_vec();
        image[0x1000] ^= 0xFF; // sector 0
        image[0x21000] ^= 0x55; // sector 2
        let mut steps = Vec::new();
        let r = k.flash_rom(&g, &image, &mut |m, d, t| steps.push((m.to_string(), d, t)), &|| false).unwrap();
        assert_eq!(r, FlashReport { rewritten: 2, unchanged: 1, retried: 0 });
        let s = k.into_inner();
        assert_eq!(s.erased, vec![0, 0x20000], "sector 1 was identical and is not erased");
        assert_eq!(s.rom[..0x28000], image[..]);
        assert!(s.rom[0x28000..0x30000].iter().all(|&b| b == 0xFF), "the end of the last sector is padded with FF");
        assert_eq!(s.rom[0x30000..], game()[0x30000..], "sectors beyond the image are not touched");
        assert_eq!(s.chip, Chip::Array);
        assert_eq!(steps.last().unwrap(), &("done".to_string(), 3, 3));
    }

    #[test]
    fn nothing_is_erased_when_the_image_does_not_fit_or_the_chip_is_not_amd() {
        let mut k = flash_kit();
        let g = k.read_geometry().unwrap().unwrap();
        for bad in [vec![], vec![0u8; 3], vec![0u8; 0x80002]] {
            assert!(k.flash_rom(&g, &bad, &mut |_, _, _| {}, &|| false).is_err());
        }
        let other = FlashGeometry { command_set: 1, ..g.clone() };
        assert!(k.flash_rom(&other, &game(), &mut |_, _, _| {}, &|| false).is_err());
        let s = k.into_inner();
        assert!(s.erased.is_empty() && s.rom == game());
    }

    #[test]
    fn a_cancelled_flash_stops_between_sectors_and_leaves_the_chip_readable() {
        let mut k = flash_kit();
        let g = k.read_geometry().unwrap().unwrap();
        let mut image = game();
        for b in image.iter_mut().step_by(0x10000) {
            *b ^= 0xFF; // every sector differs
        }
        let count = std::cell::Cell::new(0);
        let e = k.flash_rom(&g, &image, &mut |m, _, _| if m == "done" { count.set(count.get() + 1) }, &|| count.get() >= 2).unwrap_err();
        assert!(matches!(e, Error::Cancelled));
        let s = k.into_inner();
        assert_eq!(s.erased, vec![0, 0x10000], "two sectors were done, then the cancel was heard");
        assert_eq!(s.rom[..0x20000], image[..0x20000]);
        assert_eq!(s.rom[0x20000..], game()[0x20000..]);
        assert_eq!(s.chip, Chip::Array);
    }

    #[test]
    fn a_mask_rom_cartridge_answers_nothing() {
        let mut k = kit(game()); // flash = false: the chip ignores the commands
        assert_eq!(k.probe_flash().unwrap(), None);
        assert_eq!(k.into_inner().chip, Chip::Array);
    }

    #[test]
    fn every_operation_says_soon_and_nothing_is_touched() {
        let mut k = KrikzzMd;
        let planned = |e: Error| matches!(e, Error::Planned("Mega Drive")) && e.to_string() == "Mega Drive : bientôt";
        assert!(planned(k.read_header(CartMode::Dmg).unwrap_err()));
        assert!(planned(k.read_header(CartMode::Agb).unwrap_err()));
        assert!(planned(k.read_agb_rom(0, 2).unwrap_err()));
        assert!(planned(k.dump_dmg_rom(DmgMapper::Mbc5, 0x8000, &mut Vec::new(), &mut |_| true).unwrap_err()));
        assert!(planned(k.read_dmg_ram(DmgMapper::Mbc5, 0x2000, &mut |_| true).unwrap_err()));
        assert!(planned(k.write_dmg_ram(DmgMapper::Mbc5, &[0], &mut |_| true).unwrap_err()));
        assert!(planned(k.read_agb_sram(0, 2).unwrap_err()));
        assert!(planned(k.write_agb_sram(&[0], &mut |_| true).unwrap_err()));
        assert!(planned(k.power_cycle(Duration::from_millis(1)).unwrap_err()));
        assert!(!k.can_power_cycle());
        assert!(k.close().is_ok());
        assert_eq!(k.kind(), ReaderKind::KrikzzMd);
        assert!(k.kind().is_placeholder());
        assert_eq!(k.info(CartMode::Dmg).firmware, "bientôt");
    }
}
