// SPDX-License-Identifier: GPL-3.0-or-later
//! Protocol of the Chisflash "beggar socket" reader (STM32F103C8, USB CDC serial, VID:PID 0483:0721).
//!
//! Written from the protocol description and firmware of https://github.com/julpage/beggar_socket (WTFPL).
//! Nothing here touches the hardware by itself: it builds frames and talks through a [`Transport`].
//!
//! Frame sent to the reader (little endian): `size(2) cmd(1) params... crc(2)`, where `size` counts the whole
//! frame including itself. The firmware ignores the CRC but waits for exactly `size` bytes, and its command
//! buffer holds 5500 bytes. Answers are either one byte (`0xaa` = success) or `crc(2) data(n)`.
//! The firmware cannot be resynchronised by bytes: after an error the host raises DTR (or RTS) on the serial
//! port to empty the command buffer. That needs the real port, so it is not part of [`Transport`].
use crate::transport::{read_exact, Transport};
use crate::{Error, Result};
use std::time::Duration;

pub const USB_VID: u16 = 0x0483;
pub const USB_PID: u16 = 0x0721;
/// The firmware does not care about the baud rate of the virtual serial port (the reference client uses 9600).
pub const BAUD: u32 = 9600;
pub const ACK: u8 = 0xaa;
/// Largest payload of one read / write command accepted by the firmware.
pub const MAX_PAYLOAD: usize = 5000;
/// Payload size used when a transfer is split (even, so that word accesses stay aligned).
pub const CHUNK: usize = 4096;
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(2);
/// A 64 KB sector erase answers only when the erase is over.
pub const ERASE_TIMEOUT: Duration = Duration::from_secs(30);

pub mod cmd {
    pub const ROM_ID: u8 = 0xf0;
    pub const ROM_ERASE_CHIP: u8 = 0xf1;
    pub const ROM_ERASE_SECTOR: u8 = 0xf3;
    pub const ROM_PROGRAM: u8 = 0xf4;
    pub const ROM_WRITE: u8 = 0xf5;
    pub const ROM_READ: u8 = 0xf6;
    pub const RAM_WRITE: u8 = 0xf7;
    pub const RAM_READ: u8 = 0xf8;
    pub const RAM_TO_FLASH: u8 = 0xf9;
    pub const FRAM_WRITE: u8 = 0xe7;
    pub const FRAM_READ: u8 = 0xe8;
    pub const GBC_WRITE: u8 = 0xfa;
    pub const GBC_READ: u8 = 0xfb;
    pub const GBC_PROGRAM: u8 = 0xfc;
    pub const GBC_FRAM_WRITE: u8 = 0xea;
    pub const GBC_FRAM_READ: u8 = 0xeb;
}

/// CRC-16/MODBUS, what the reference client computes (the firmware ignores it).
pub fn crc16(buf: &[u8]) -> u16 {
    let mut crc = 0xffffu16;
    for &b in buf {
        crc ^= b as u16;
        for _ in 0..8 {
            let low = crc & 1;
            crc >>= 1;
            if low != 0 {
                crc ^= 0xa001;
            }
        }
    }
    crc
}

/// Builds a complete frame. Panics only if the frame cannot fit the 16-bit size field (callers chunk before).
pub fn frame(cmd: u8, params: &[u8]) -> Vec<u8> {
    let size = 2 + 1 + params.len() + 2;
    let mut f = Vec::with_capacity(size);
    f.extend_from_slice(&u16::try_from(size).expect("frame too large").to_le_bytes());
    f.push(cmd);
    f.extend_from_slice(params);
    let crc = crc16(&f);
    f.extend_from_slice(&crc.to_le_bytes());
    f
}

fn invalid(msg: &'static str) -> Error {
    Error::Io(std::io::Error::new(std::io::ErrorKind::InvalidInput, msg))
}

/// Parameters `address(4)` followed by `extra` and `data`.
fn with_address(address: u32, extra: &[u8], data: &[u8]) -> Vec<u8> {
    let mut p = Vec::with_capacity(4 + extra.len() + data.len());
    p.extend_from_slice(&address.to_le_bytes());
    p.extend_from_slice(extra);
    p.extend_from_slice(data);
    p
}

/// `/dev/ttyACM*` of the readers with the Chisflash USB ids. 0483 is STMicroelectronics' generic vendor id, so the
/// product id is part of the filter. The RC31 firmware (0483:5740) is another protocol and is not matched here.
pub fn find_ports() -> Vec<String> {
    crate::transport::find_ports_in(std::path::Path::new("/sys/class/tty"), &[(USB_VID, USB_PID)])
}

pub struct Chis<T: Transport> {
    t: T,
}

impl<T: Transport> Chis<T> {
    pub fn new(mut t: T) -> Result<Self> {
        t.set_baud(BAUD)?;
        t.set_timeout(DEFAULT_TIMEOUT)?;
        t.reset_device()?;
        t.clear_input()?;
        Ok(Self { t })
    }

    /// Empties the firmware command buffer (DTR/RTS pulse) and our input. Call it after any error that may have
    /// left a half frame in the reader: it waits for exactly the announced size and would swallow the next command.
    pub fn resync(&mut self) -> Result<()> {
        self.t.reset_device()?;
        self.t.clear_input()
    }

    pub fn into_inner(self) -> T {
        self.t
    }

    fn send(&mut self, cmd: u8, params: &[u8]) -> Result<()> {
        self.t.write_all(&frame(cmd, params))
    }

    fn short_to_no_answer(e: Error) -> Error {
        match e {
            Error::ShortRead { got: 0, .. } => Error::NoAnswer,
            e => e,
        }
    }

    fn expect_ack(&mut self) -> Result<()> {
        let mut b = [0u8; 1];
        read_exact(&mut self.t, &mut b).map_err(Self::short_to_no_answer)?;
        if b[0] == ACK {
            Ok(())
        } else {
            Err(Error::Nack(b[0]))
        }
    }

    /// Answer made of `crc(2)` and `n` bytes of data.
    fn read_data(&mut self, n: usize) -> Result<Vec<u8>> {
        let mut buf = vec![0u8; 2 + n];
        read_exact(&mut self.t, &mut buf).map_err(Self::short_to_no_answer)?;
        buf.drain(..2);
        Ok(buf)
    }

    /// Autoselect IDs of the flash chip on a GBA cartridge: words 0, 1, 14 and 15 (manufacturer, device, ...).
    /// This is a write sequence (0xaa / 0x55 / 0x90 then a software reset), so use it on flash carts only.
    pub fn rom_id(&mut self) -> Result<[u16; 4]> {
        self.send(cmd::ROM_ID, &[])?;
        let d = self.read_data(8)?;
        Ok([0, 2, 4, 6].map(|i| u16::from_le_bytes([d[i], d[i + 1]])))
    }

    /// Reads the GBA ROM. `address` and `len` are in bytes and must be even.
    pub fn read_rom(&mut self, address: u32, len: usize) -> Result<Vec<u8>> {
        if !address.is_multiple_of(2) || !len.is_multiple_of(2) {
            return Err(invalid("GBA ROM reads need an even address and length"));
        }
        let mut out = Vec::with_capacity(len);
        while out.len() < len {
            let n = (len - out.len()).min(CHUNK);
            let a = address + out.len() as u32;
            self.send(cmd::ROM_READ, &with_address(a, &(n as u16).to_le_bytes(), &[]))?;
            out.extend(self.read_data(n)?);
        }
        Ok(out)
    }

    /// Reads the GBA save RAM (SRAM). The firmware keeps the low 16 bits of the address; bank switching is the
    /// host's job.
    pub fn read_ram(&mut self, address: u32, len: usize) -> Result<Vec<u8>> {
        self.read_plain(cmd::RAM_READ, address, len)
    }

    /// Reads the Game Boy / Game Boy Color cartridge bus (ROM at 0x0000-0x7FFF, RAM at 0xA000-0xBFFF).
    pub fn gbc_read(&mut self, address: u32, len: usize) -> Result<Vec<u8>> {
        self.read_plain(cmd::GBC_READ, address, len)
    }

    fn read_plain(&mut self, command: u8, address: u32, len: usize) -> Result<Vec<u8>> {
        let mut out = Vec::with_capacity(len);
        while out.len() < len {
            let n = (len - out.len()).min(CHUNK);
            let a = address + out.len() as u32;
            self.send(command, &with_address(a, &(n as u16).to_le_bytes(), &[]))?;
            out.extend(self.read_data(n)?);
        }
        Ok(out)
    }

    pub fn write_ram(&mut self, address: u32, data: &[u8]) -> Result<()> {
        self.write_plain(cmd::RAM_WRITE, address, data)
    }

    pub fn gbc_write(&mut self, address: u32, data: &[u8]) -> Result<()> {
        self.write_plain(cmd::GBC_WRITE, address, data)
    }

    fn write_plain(&mut self, command: u8, address: u32, data: &[u8]) -> Result<()> {
        for (i, c) in data.chunks(CHUNK).enumerate() {
            let a = address + (i * CHUNK) as u32;
            self.send(command, &with_address(a, &[], c))?;
            self.expect_ack()?;
        }
        Ok(())
    }

    /// FRAM (FM20) read: the chip is slow, `delay` is the number of wait cycles per access.
    pub fn read_fram(&mut self, address: u32, len: usize, delay: u8) -> Result<Vec<u8>> {
        self.read_fram_cmd(cmd::FRAM_READ, address, len, delay)
    }

    pub fn gbc_read_fram(&mut self, address: u32, len: usize, delay: u8) -> Result<Vec<u8>> {
        self.read_fram_cmd(cmd::GBC_FRAM_READ, address, len, delay)
    }

    fn read_fram_cmd(&mut self, command: u8, address: u32, len: usize, delay: u8) -> Result<Vec<u8>> {
        let mut out = Vec::with_capacity(len);
        while out.len() < len {
            let n = (len - out.len()).min(CHUNK);
            let a = address + out.len() as u32;
            let mut extra = (n as u16).to_le_bytes().to_vec();
            extra.push(delay);
            self.send(command, &with_address(a, &extra, &[]))?;
            out.extend(self.read_data(n)?);
        }
        Ok(out)
    }

    pub fn write_fram(&mut self, address: u32, data: &[u8], delay: u8) -> Result<()> {
        self.write_fram_cmd(cmd::FRAM_WRITE, address, data, delay)
    }

    pub fn gbc_write_fram(&mut self, address: u32, data: &[u8], delay: u8) -> Result<()> {
        self.write_fram_cmd(cmd::GBC_FRAM_WRITE, address, data, delay)
    }

    fn write_fram_cmd(&mut self, command: u8, address: u32, data: &[u8], delay: u8) -> Result<()> {
        for (i, c) in data.chunks(CHUNK).enumerate() {
            let a = address + (i * CHUNK) as u32;
            self.send(command, &with_address(a, &[delay], c))?;
            self.expect_ack()?;
        }
        Ok(())
    }

    /// DESTRUCTIVE. Starts a full chip erase; the reader answers at once and does not wait for the end
    /// (the host has to poll or wait, the flash chip is busy meanwhile).
    pub fn erase_chip(&mut self) -> Result<()> {
        self.send(cmd::ROM_ERASE_CHIP, &[])?;
        self.expect_ack()
    }

    /// DESTRUCTIVE. Erases the 64 KB sector holding the byte `address` and waits for the end of the erase.
    pub fn erase_sector(&mut self, address: u32) -> Result<()> {
        self.t.set_timeout(ERASE_TIMEOUT)?;
        let r = self.send(cmd::ROM_ERASE_SECTOR, &address.to_le_bytes()).and_then(|_| self.expect_ack());
        self.t.set_timeout(DEFAULT_TIMEOUT)?;
        r
    }

    /// DESTRUCTIVE. Programs the GBA flash with the chip's own buffered or single-word programming.
    /// `buffer_size` is the write buffer of the chip in bytes, 0 for chips that only program one word at a time.
    /// `address` is in bytes and even, `data` has an even length.
    pub fn program_rom(&mut self, address: u32, buffer_size: u16, data: &[u8]) -> Result<()> {
        self.program(cmd::ROM_PROGRAM, address, buffer_size, data)
    }

    /// DESTRUCTIVE. Programs a Game Boy flash cartridge with the firmware's own loop (command 0xfc).
    ///
    /// What the firmware does (read in `gbcRomProgram`): the unlock addresses are FIXED at 0xAAA and 0x555 (`aa`
    /// at 0xAAA, `55` at 0x555), so chips that need 0x555 / 0x2AA are not supported by this command.
    /// - `buffer_size` = 0: per byte, `a0` at 0xAAA, the byte, then polls until the byte reads back equal.
    /// - `buffer_size` > 0: `25` at the start address, `len - 1` (one BYTE: at most 256), the data, `29` at the
    ///   start address, then polls the last byte. The firmware does not align anything itself.
    ///
    /// The firmware has NO timeout: if a byte can never read back equal (target not erased, wrong chip), it loops
    /// until the host pulses DTR/RTS ([`resync`](Self::resync)), then answers nothing. The chip is left in program
    /// mode and needs a reset command (`f0`) sent with [`gbc_write`](Self::gbc_write).
    ///
    /// `address` is a bus address (bank switching is the host's job) and the firmware keeps only its low 16 bits,
    /// so `data` must stay inside one 16 KiB window (0x0000-0x3FFF or 0x4000-0x7FFF). With a write buffer the
    /// address must be a multiple of `buffer_size` so that no buffer crosses a chip page.
    pub fn gbc_program(&mut self, address: u32, buffer_size: u16, data: &[u8]) -> Result<()> {
        if buffer_size > 256 {
            return Err(invalid("the GB firmware stores the buffer length in one byte: 256 at most"));
        }
        if data.is_empty() {
            return Err(invalid("nothing to program"));
        }
        if (address & 0x3fff) as usize + data.len() > 0x4000 {
            return Err(invalid("a GB programming request must stay inside one 16 KiB window"));
        }
        if buffer_size > 0 && !address.is_multiple_of(buffer_size as u32) {
            return Err(invalid("the address must be a multiple of the write buffer size"));
        }
        let step = if buffer_size > 0 { (CHUNK / buffer_size as usize) * buffer_size as usize } else { CHUNK };
        for (i, c) in data.chunks(step).enumerate() {
            let a = address + (i * step) as u32;
            self.send(cmd::GBC_PROGRAM, &with_address(a, &buffer_size.to_le_bytes(), c))?;
            self.expect_ack()?;
        }
        Ok(())
    }

    fn program(&mut self, command: u8, address: u32, buffer_size: u16, data: &[u8]) -> Result<()> {
        if !address.is_multiple_of(2) || !data.len().is_multiple_of(2) {
            return Err(invalid("programming needs an even address and length"));
        }
        // A chunk is a multiple of the chip's write buffer so that every buffer is written whole.
        let step = if buffer_size >= 2 { (CHUNK / buffer_size as usize).max(1) * buffer_size as usize } else { CHUNK };
        let step = step.min(MAX_PAYLOAD & !1);
        for (i, c) in data.chunks(step).enumerate() {
            let a = address + (i * step) as u32;
            self.send(command, &with_address(a, &buffer_size.to_le_bytes(), c))?;
            self.expect_ack()?;
        }
        Ok(())
    }

    /// DESTRUCTIVE. Raw write of 16-bit words to the ROM bus, with no chip algorithm (command sequences).
    /// `word_address` is a word address; `data` has an even length of 2 to 5000 bytes.
    pub fn write_rom_words(&mut self, word_address: u32, data: &[u8]) -> Result<()> {
        if data.len() < 2 || !data.len().is_multiple_of(2) || data.len() > MAX_PAYLOAD {
            return Err(invalid("raw ROM writes take 2 to 5000 bytes, even"));
        }
        self.send(cmd::ROM_WRITE, &with_address(word_address, &[], data))?;
        self.expect_ack()
    }

    /// DESTRUCTIVE. Copies bytes into the save flash of a cartridge that stores saves in flash.
    pub fn write_save_flash(&mut self, address: u32, data: &[u8]) -> Result<()> {
        self.write_plain(cmd::RAM_TO_FLASH, address, data)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;

    /// A reader simulated from the firmware: parses frames the way `uart_cmdHandler` does and acts on a cartridge.
    struct Sim {
        rom: Vec<u8>,
        ram: Vec<u8>,
        pending: Vec<u8>,
        out: VecDeque<u8>,
        seen: Vec<u8>,
        /// Answers one byte other than 0xaa to write commands.
        nack: Option<u8>,
        resets: usize,
    }

    impl Sim {
        fn new(rom: Vec<u8>) -> Self {
            Self { rom, ram: vec![0; 0x10000], pending: vec![], out: VecDeque::new(), seen: vec![], nack: None, resets: 0 }
        }
        fn handle(&mut self, f: Vec<u8>) {
            assert_eq!(u16::from_le_bytes([f[0], f[1]]) as usize, f.len(), "size field");
            assert_eq!(crc16(&f[..f.len() - 2]).to_le_bytes(), [f[f.len() - 2], f[f.len() - 1]], "crc");
            let c = f[2];
            self.seen.push(c);
            let body = &f[3..f.len() - 2];
            let addr = || u32::from_le_bytes(body[..4].try_into().unwrap()) as usize;
            let ack = |s: &mut Self| s.out.push_back(s.nack.unwrap_or(ACK));
            match c {
                cmd::ROM_ID => {
                    self.out.extend([0, 0]);
                    self.out.extend([0x01, 0x00, 0x7e, 0x22, 0x22, 0x22, 0x01, 0x22]);
                }
                cmd::ROM_READ => {
                    let n = u16::from_le_bytes([body[4], body[5]]) as usize;
                    assert!(n.is_multiple_of(2) && addr() % 2 == 0);
                    self.out.extend([0, 0]);
                    self.out.extend(self.rom[addr()..addr() + n].to_vec());
                }
                cmd::GBC_READ | cmd::RAM_READ => {
                    let n = u16::from_le_bytes([body[4], body[5]]) as usize;
                    let src = if c == cmd::GBC_READ { &self.rom } else { &self.ram };
                    self.out.extend([0, 0]);
                    self.out.extend(src[addr()..addr() + n].to_vec());
                }
                cmd::FRAM_READ | cmd::GBC_FRAM_READ => {
                    let n = u16::from_le_bytes([body[4], body[5]]) as usize;
                    assert_eq!(body.len(), 7, "address, count, delay");
                    self.out.extend([0, 0]);
                    self.out.extend(self.ram[addr()..addr() + n].to_vec());
                }
                cmd::RAM_WRITE => {
                    let a = addr();
                    self.ram[a..a + body.len() - 4].copy_from_slice(&body[4..]);
                    ack(self);
                }
                cmd::FRAM_WRITE | cmd::GBC_FRAM_WRITE => {
                    let a = addr();
                    self.ram[a..a + body.len() - 5].copy_from_slice(&body[5..]);
                    ack(self);
                }
                cmd::ROM_PROGRAM | cmd::GBC_PROGRAM => {
                    let a = addr();
                    let data = &body[6..];
                    assert!(a + data.len() <= self.rom.len());
                    self.rom[a..a + data.len()].copy_from_slice(data);
                    ack(self);
                }
                cmd::ROM_ERASE_SECTOR => {
                    let a = addr() & !0xffff;
                    self.rom[a..a + 0x10000].fill(0xff);
                    ack(self);
                }
                cmd::ROM_ERASE_CHIP => {
                    self.rom.fill(0xff);
                    ack(self);
                }
                cmd::ROM_WRITE | cmd::GBC_WRITE | cmd::RAM_TO_FLASH => ack(self),
                other => panic!("unexpected command {other:#04x}"),
            }
        }
    }

    impl Transport for Sim {
        fn write_all(&mut self, data: &[u8]) -> Result<()> {
            self.pending.extend_from_slice(data);
            // like the firmware: execute when the buffer holds exactly `size` bytes
            while self.pending.len() > 2 && u16::from_le_bytes([self.pending[0], self.pending[1]]) as usize == self.pending.len() {
                let f = std::mem::take(&mut self.pending);
                assert!(f.len() <= 5500, "frame larger than the firmware command buffer");
                self.handle(f);
            }
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
        fn reset_device(&mut self) -> Result<()> {
            self.pending.clear();
            self.resets += 1;
            Ok(())
        }
    }

    fn pattern(n: usize) -> Vec<u8> {
        (0..n).map(|i| (i * 7 + i / 251) as u8).collect()
    }

    #[test]
    fn ports_are_found_by_vendor_and_product_id() {
        let root = std::env::temp_dir().join(format!("zerocart-chis-ports-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        for (tty, vid, pid) in [("ttyACM0", "0483", "0721"), ("ttyACM1", "0483", "5740"), ("ttyUSB0", "1a86", "7523")] {
            let dev = root.join(tty).join("device");
            std::fs::create_dir_all(&dev).unwrap();
            std::fs::write(dev.join("idVendor"), format!("{vid}\n")).unwrap();
            std::fs::write(dev.join("idProduct"), format!("{pid}\n")).unwrap();
        }
        assert_eq!(crate::transport::find_ports_in(&root, &[(USB_VID, USB_PID)]), ["/dev/ttyACM0"]);
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn crc_and_frames_match_the_firmware_examples() {
        // From the firmware comments: "05 00 f0 61 85" (get ID) and "05 00 f1 a0 45" (erase chip).
        assert_eq!(frame(cmd::ROM_ID, &[]), [0x05, 0x00, 0xf0, 0x61, 0x85]);
        assert_eq!(frame(cmd::ROM_ERASE_CHIP, &[]), [0x05, 0x00, 0xf1, 0xa0, 0x45]);
        // size field counts the whole frame: 2 + 1 + 4 + 2 + 2 = 11 for a read
        let f = frame(cmd::ROM_READ, &with_address(0x1000, &16u16.to_le_bytes(), &[]));
        assert_eq!(&f[..3], &[11, 0, 0xf6]);
        assert_eq!(&f[3..7], &0x1000u32.to_le_bytes());
    }

    #[test]
    fn rom_id_is_four_little_endian_words() {
        let mut c = Chis::new(Sim::new(vec![0; 16])).unwrap();
        assert_eq!(c.rom_id().unwrap(), [0x0001, 0x227e, 0x2222, 0x2201]);
    }

    #[test]
    fn rom_read_splits_in_chunks_and_keeps_order() {
        let rom = pattern(3 * CHUNK + 600);
        let mut c = Chis::new(Sim::new(rom.clone())).unwrap();
        assert_eq!(c.read_rom(0, rom.len()).unwrap(), rom);
        assert_eq!(c.read_rom(2, 100).unwrap(), rom[2..102]);
        assert_eq!(c.into_inner().seen.iter().filter(|&&x| x == cmd::ROM_READ).count(), 4 + 1);
    }

    #[test]
    fn rom_read_refuses_odd_addresses_and_lengths() {
        let mut c = Chis::new(Sim::new(vec![0; 64])).unwrap();
        assert!(c.read_rom(1, 4).is_err());
        assert!(c.read_rom(0, 3).is_err());
        assert!(c.into_inner().seen.is_empty(), "nothing is sent for a refused request");
    }

    #[test]
    fn ram_and_gbc_round_trips() {
        let data = pattern(CHUNK + 33);
        let mut c = Chis::new(Sim::new(pattern(0x8000))).unwrap();
        c.write_ram(0x10, &data[..CHUNK]).unwrap();
        assert_eq!(c.read_ram(0x10, CHUNK).unwrap(), data[..CHUNK]);
        assert_eq!(c.gbc_read(0x100, 0x50).unwrap(), pattern(0x8000)[0x100..0x150]);
        c.write_fram(0, &data[..10], 3).unwrap();
        assert_eq!(c.read_fram(0, 10, 3).unwrap(), data[..10]);
        c.gbc_write_fram(0x20, &data[..5], 1).unwrap();
        assert_eq!(c.gbc_read_fram(0x20, 5, 1).unwrap(), data[..5]);
    }

    #[test]
    fn program_and_erase_change_the_simulated_cartridge() {
        let mut c = Chis::new(Sim::new(vec![0; 0x20000])).unwrap();
        let data = pattern(CHUNK * 2 + 64);
        c.program_rom(0x10000, 64, &data).unwrap();
        assert_eq!(c.read_rom(0x10000, data.len()).unwrap(), data);
        c.erase_sector(0x10000).unwrap();
        assert!(c.read_rom(0x10000, 256).unwrap().iter().all(|&b| b == 0xff));
        c.erase_chip().unwrap();
        assert!(c.read_rom(0, 256).unwrap().iter().all(|&b| b == 0xff));
    }

    #[test]
    fn gbc_programming_respects_the_firmware_limits() {
        let mut c = Chis::new(Sim::new(vec![0; 0x10000])).unwrap();
        let data = pattern(0x1000 + 5);
        // odd lengths are fine on a Game Boy; single byte mode and buffer mode both reach the cartridge
        c.gbc_program(0x4000, 0, &data).unwrap();
        assert_eq!(c.gbc_read(0x4000, data.len()).unwrap(), data);
        c.gbc_program(0x4040, 64, &data[..640]).unwrap();
        assert_eq!(c.gbc_read(0x4040, 640).unwrap(), data[..640]);
        // refused before anything is sent
        let before = c.t.seen.len();
        assert!(c.gbc_program(0x7ff0, 0, &[0; 32]).is_err(), "crosses 0x8000");
        assert!(c.gbc_program(0x3ff0, 0, &[0; 32]).is_err(), "crosses 0x4000");
        assert!(c.gbc_program(0x4001, 64, &[0; 64]).is_err(), "unaligned buffer");
        assert!(c.gbc_program(0x4000, 512, &[0; 64]).is_err(), "length stored in one byte");
        assert!(c.gbc_program(0x4000, 0, &[]).is_err());
        assert_eq!(c.t.seen.len(), before);
    }

    #[test]
    fn programming_chunks_are_multiples_of_the_chip_buffer() {
        let mut c = Chis::new(Sim::new(vec![0; 0x20000])).unwrap();
        let data = pattern(CHUNK * 2);
        c.program_rom(0, 512, &data).unwrap();
        assert_eq!(c.read_rom(0, data.len()).unwrap(), data);
        // 4096 is a multiple of 512: two commands. With a 96-byte buffer a chunk is 42 * 96 = 4032 bytes.
        let mut c = Chis::new(Sim::new(vec![0; 0x20000])).unwrap();
        c.program_rom(0, 96, &data).unwrap();
        assert_eq!(c.into_inner().seen.iter().filter(|&&x| x == cmd::ROM_PROGRAM).count(), 3);
    }

    #[test]
    fn errors_are_reported() {
        let mut sim = Sim::new(vec![0; 64]);
        sim.nack = Some(0x55);
        let mut c = Chis::new(sim).unwrap();
        assert!(matches!(c.write_ram(0, &[1]), Err(Error::Nack(0x55))));
        assert!(matches!(c.erase_chip(), Err(Error::Nack(0x55))));
        // a reader that never answers
        struct Dead;
        impl Transport for Dead {
            fn write_all(&mut self, _: &[u8]) -> Result<()> { Ok(()) }
            fn read(&mut self, _: &mut [u8]) -> Result<usize> { Ok(0) }
            fn set_timeout(&mut self, _: Duration) -> Result<()> { Ok(()) }
            fn set_baud(&mut self, _: u32) -> Result<()> { Ok(()) }
            fn clear_input(&mut self) -> Result<()> { Ok(()) }
        }
        let mut c = Chis::new(Dead).unwrap();
        assert!(matches!(c.rom_id(), Err(Error::NoAnswer)));
        assert!(matches!(c.erase_chip(), Err(Error::NoAnswer)));
    }

    #[test]
    fn a_half_frame_swallows_the_next_command_until_the_reader_is_reset() {
        let c = Chis::new(Sim::new(pattern(64))).unwrap();
        assert_eq!(c.into_inner().resets, 1, "opening resets the reader");
        let mut c = Chis::new(Sim::new(pattern(64))).unwrap();
        // the reader got only the start of a read frame (size says 11, 3 bytes arrived)
        c.t.write_all(&frame(cmd::ROM_READ, &with_address(0, &8u16.to_le_bytes(), &[]))[..3]).unwrap();
        // without a reset the next frame is appended to the half one and nothing is executed
        c.t.write_all(&frame(cmd::ROM_READ, &with_address(0, &8u16.to_le_bytes(), &[]))).unwrap();
        assert!(c.t.seen.is_empty());
        c.resync().unwrap();
        assert_eq!(c.read_rom(0, 8).unwrap(), pattern(64)[..8]);
        assert_eq!(c.into_inner().resets, 2);
    }

    #[test]
    fn raw_writes_check_their_size() {
        let mut c = Chis::new(Sim::new(vec![0; 64])).unwrap();
        assert!(c.write_rom_words(0, &[1]).is_err());
        assert!(c.write_rom_words(0, &vec![0; MAX_PAYLOAD + 2]).is_err());
        c.write_rom_words(0x555, &[0xaa, 0x00]).unwrap();
    }
}
