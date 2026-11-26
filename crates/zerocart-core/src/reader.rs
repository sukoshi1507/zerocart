// SPDX-License-Identifier: GPL-3.0-or-later
//! What the application needs from a cartridge reader, whatever the hardware: header, ROM, saves.
//!
//! Two readers implement it: [`GbxCart`] (GBxCart RW / GBFlash, one cartridge connector whose voltage and mode
//! are set by software) and [`Chis`] (Chisflash, original firmware: the same single connector, no software choice
//! of voltage). Flashing is not part of the trait: it needs the chip database
//! and differs a lot between the two, so the application handles it per reader.
use crate::chis::Chis;
use crate::device::{CartMode, DumpSums, GbxCart};
use crate::mapper::{DmgMapper, RAM_BANK_SIZE, ROM_BANK_SIZE};
use crate::transport::Transport;
use crate::{Error, Result};
use std::time::Duration;

/// Header bytes read for identification (the GBA database wants the first 0x180).
pub const HEADER_LEN: usize = 0x180;

/// `non_exhaustive`: other readers will come (ZeroCart flashes cartridges of any system), adding one must not break
/// the code that matches on this.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ReaderKind {
    GbxCart,
    Chisflash,
    /// Krikzz's Mega Drive cartridge programmer. PLACEHOLDER: planned, not implemented ([`crate::krikzz`]).
    KrikzzMd,
}

impl ReaderKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::GbxCart => "GBxCart RW / GBFlash",
            Self::Chisflash => "Chisflash",
            Self::KrikzzMd => "MD Flasher",
        }
    }

    /// The kinds of cartridge this reader can take.
    pub fn cart_kinds(self) -> &'static [CartKind] {
        match self {
            Self::GbxCart | Self::Chisflash => &[CartKind::Gb, CartKind::Gba],
            Self::KrikzzMd => &[CartKind::MegaDrive],
        }
    }

    /// Planned but not implemented: every operation answers "bientôt" and the application does not offer it yet.
    pub fn is_placeholder(self) -> bool {
        matches!(self, Self::KrikzzMd)
    }
}

/// The system of a cartridge. `Gb` covers GB and GBC (same reader path; the header's CGB flag tells them apart).
/// The Mega Drive is a PLACEHOLDER (no reader operation is implemented for it). ZeroCart is meant to flash cartridges
/// of any system (Master System, Super Famicom... later): `non_exhaustive`, add a variant when its reader exists.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum CartKind {
    Gb,
    Gba,
    MegaDrive,
}

impl CartKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Gb => "GB/GBC",
            Self::Gba => "GBA",
            Self::MegaDrive => "Mega Drive",
        }
    }

    pub fn from_mode(mode: CartMode) -> Self {
        match mode {
            CartMode::Dmg => Self::Gb,
            CartMode::Agb => Self::Gba,
        }
    }

    /// The mode of the GB/GBA reader operations; `None` for a system they do not handle.
    pub fn mode(self) -> Option<CartMode> {
        match self {
            Self::Gb => Some(CartMode::Dmg),
            Self::Gba => Some(CartMode::Agb),
            Self::MegaDrive => None,
        }
    }

    pub fn is_placeholder(self) -> bool {
        matches!(self, Self::MegaDrive)
    }
}

/// What the top bar shows about the reader.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReaderInfo {
    pub name: String,
    pub firmware: String,
    /// "3.3V" or "5V" when the reader sets it, empty when it does not (the Chisflash). Both readers have ONE
    /// connector shared by all cartridges: the mode only tells the reader which protocol (and voltage) to use.
    pub voltage: &'static str,
}

pub trait CartReader {
    fn kind(&self) -> ReaderKind;
    fn info(&self, mode: CartMode) -> ReaderInfo;
    /// Selects the system (voltage and pins for the GBxCart, which bus protocol for the Chisflash) and returns the
    /// first [`HEADER_LEN`] bytes of the cartridge in the connector. An empty or wrong-system cartridge gives bytes
    /// that fail the header checks: the caller says so.
    fn read_header(&mut self, mode: CartMode) -> Result<Vec<u8>>;
    /// GBA ROM bytes (byte address, even address and length).
    fn read_agb_rom(&mut self, address: u32, len: usize) -> Result<Vec<u8>>;
    /// Dumps a Game Boy ROM bank by bank into `out`. `progress(done)` returns false to cancel.
    fn dump_dmg_rom(
        &mut self,
        mapper: DmgMapper,
        size: usize,
        out: &mut dyn std::io::Write,
        progress: &mut dyn FnMut(usize) -> bool,
    ) -> Result<DumpSums>;
    fn read_dmg_ram(&mut self, mapper: DmgMapper, size: usize, progress: &mut dyn FnMut(usize) -> bool) -> Result<Vec<u8>>;
    /// Writes the cartridge RAM. The RAM is disabled again at the end, also after an error.
    fn write_dmg_ram(&mut self, mapper: DmgMapper, data: &[u8], progress: &mut dyn FnMut(usize) -> bool) -> Result<()>;
    fn read_agb_sram(&mut self, address: u32, len: usize) -> Result<Vec<u8>>;
    /// Writes the GBA SRAM from offset 0.
    fn write_agb_sram(&mut self, data: &[u8], progress: &mut dyn FnMut(usize) -> bool) -> Result<()>;
    /// Whether [`power_cycle`](Self::power_cycle) can cut the cartridge power. Without it, two reads of a save in one
    /// session prove nothing about a dead battery: the application must present such a save as UNVERIFIED.
    fn can_power_cycle(&self) -> bool;
    /// Cuts the cartridge power for `off`, to see whether a battery keeps the save. Returns false when the reader
    /// cannot cut it (nothing was done).
    fn power_cycle(&mut self, off: Duration) -> Result<bool>;
    fn close(&mut self) -> Result<()>;
}

impl<T: Transport> CartReader for GbxCart<T> {
    fn kind(&self) -> ReaderKind {
        ReaderKind::GbxCart
    }
    fn info(&self, mode: CartMode) -> ReaderInfo {
        ReaderInfo {
            name: self.fw.pcb_name.clone(),
            firmware: format!("{} {}", self.fw.pcb_label(), self.fw.version_string()).trim().to_string(),
            voltage: if mode == CartMode::Dmg { "5V" } else { "3.3V" },
        }
    }
    fn read_header(&mut self, mode: CartMode) -> Result<Vec<u8>> {
        self.set_mode(mode)?;
        self.prepare_header()?;
        GbxCart::read_rom(self, 0, HEADER_LEN)
    }
    fn read_agb_rom(&mut self, address: u32, len: usize) -> Result<Vec<u8>> {
        GbxCart::read_rom(self, address, len)
    }
    fn dump_dmg_rom(
        &mut self,
        mapper: DmgMapper,
        size: usize,
        out: &mut dyn std::io::Write,
        progress: &mut dyn FnMut(usize) -> bool,
    ) -> Result<DumpSums> {
        GbxCart::dump_dmg_rom(self, mapper, size, out, progress)
    }
    fn read_dmg_ram(&mut self, mapper: DmgMapper, size: usize, progress: &mut dyn FnMut(usize) -> bool) -> Result<Vec<u8>> {
        GbxCart::read_dmg_ram(self, mapper, size, progress)
    }
    fn write_dmg_ram(&mut self, mapper: DmgMapper, data: &[u8], progress: &mut dyn FnMut(usize) -> bool) -> Result<()> {
        GbxCart::write_dmg_ram(self, mapper, data, progress)
    }
    fn read_agb_sram(&mut self, address: u32, len: usize) -> Result<Vec<u8>> {
        GbxCart::read_agb_sram(self, address, len)
    }
    fn write_agb_sram(&mut self, data: &[u8], progress: &mut dyn FnMut(usize) -> bool) -> Result<()> {
        GbxCart::write_agb_sram(self, data, progress)
    }
    fn can_power_cycle(&self) -> bool {
        GbxCart::can_power_cycle(self)
    }
    fn power_cycle(&mut self, off: Duration) -> Result<bool> {
        GbxCart::power_cycle(self, off)
    }
    fn close(&mut self) -> Result<()> {
        GbxCart::close(self)
    }
}

/// Bytes per write command when the Chisflash writes a save: small enough for a progress bar that moves and for
/// a cancel that is heard quickly.
const SAVE_WRITE_BLOCK: usize = 1024;

impl<T: Transport> CartReader for Chis<T> {
    fn kind(&self) -> ReaderKind {
        ReaderKind::Chisflash
    }
    fn info(&self, _mode: CartMode) -> ReaderInfo {
        ReaderInfo { name: "Chisflash".into(), firmware: String::new(), voltage: "" }
    }
    fn read_header(&mut self, mode: CartMode) -> Result<Vec<u8>> {
        match mode {
            CartMode::Agb => self.read_rom(0, HEADER_LEN),
            CartMode::Dmg => self.gbc_read(0, HEADER_LEN),
        }
    }
    fn read_agb_rom(&mut self, address: u32, len: usize) -> Result<Vec<u8>> {
        self.read_rom(address, len)
    }
    fn dump_dmg_rom(
        &mut self,
        mapper: DmgMapper,
        size: usize,
        out: &mut dyn std::io::Write,
        progress: &mut dyn FnMut(usize) -> bool,
    ) -> Result<DumpSums> {
        let mut crc = crc32fast::Hasher::new();
        let (mut sum, mut pos) = (0u16, 0usize);
        for bank in 0..size.div_ceil(ROM_BANK_SIZE) {
            let (writes, start) = mapper.select_rom_bank(bank);
            for (addr, value) in writes {
                self.gbc_write(addr, &[value])?;
            }
            let n = ROM_BANK_SIZE.min(size - pos);
            let data = self.gbc_read(start, n)?;
            for (i, &b) in data.iter().enumerate() {
                let p = pos + i;
                if p != 0x14E && p != 0x14F {
                    sum = sum.wrapping_add(b as u16);
                }
            }
            crc.update(&data);
            out.write_all(&data)?;
            pos += n;
            if !progress(pos) {
                return Err(Error::Cancelled);
            }
        }
        Ok(DumpSums { crc32: crc.finalize(), checksum16: sum })
    }
    fn read_dmg_ram(&mut self, mapper: DmgMapper, size: usize, progress: &mut dyn FnMut(usize) -> bool) -> Result<Vec<u8>> {
        for (a, v) in mapper.enable_ram(true) {
            self.gbc_write(a, &[v])?;
        }
        let result = (|| {
            let mut out = Vec::with_capacity(size);
            for bank in 0..size.div_ceil(RAM_BANK_SIZE) {
                for (a, v) in mapper.select_ram_bank(bank) {
                    self.gbc_write(a, &[v])?;
                }
                let n = RAM_BANK_SIZE.min(size - out.len());
                out.extend(self.gbc_read(0xA000, n)?);
                if !progress(out.len()) {
                    return Err(Error::Cancelled);
                }
            }
            Ok(out)
        })();
        for (a, v) in mapper.enable_ram(false) {
            let _ = self.gbc_write(a, &[v]);
        }
        result
    }
    fn write_dmg_ram(&mut self, mapper: DmgMapper, data: &[u8], progress: &mut dyn FnMut(usize) -> bool) -> Result<()> {
        for (a, v) in mapper.enable_ram(true) {
            self.gbc_write(a, &[v])?;
        }
        let result = (|| {
            let mut done = 0;
            for (bank, part) in data.chunks(RAM_BANK_SIZE).enumerate() {
                for (a, v) in mapper.select_ram_bank(bank) {
                    self.gbc_write(a, &[v])?;
                }
                for (i, piece) in part.chunks(SAVE_WRITE_BLOCK).enumerate() {
                    self.gbc_write(0xA000 + (i * SAVE_WRITE_BLOCK) as u32, piece)?;
                    done += piece.len();
                    if !progress(done) {
                        return Err(Error::Cancelled);
                    }
                }
            }
            Ok(())
        })();
        for (a, v) in mapper.enable_ram(false) {
            let _ = self.gbc_write(a, &[v]);
        }
        result
    }
    fn read_agb_sram(&mut self, address: u32, len: usize) -> Result<Vec<u8>> {
        self.read_ram(address, len)
    }
    fn write_agb_sram(&mut self, data: &[u8], progress: &mut dyn FnMut(usize) -> bool) -> Result<()> {
        let mut done = 0;
        for (i, piece) in data.chunks(SAVE_WRITE_BLOCK).enumerate() {
            self.write_ram((i * SAVE_WRITE_BLOCK) as u32, piece)?;
            done += piece.len();
            if !progress(done) {
                return Err(Error::Cancelled);
            }
        }
        Ok(())
    }
    fn can_power_cycle(&self) -> bool {
        false
    }
    fn power_cycle(&mut self, _off: Duration) -> Result<bool> {
        Ok(false)
    }
    fn close(&mut self) -> Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chis::{cmd, ACK};
    use std::collections::VecDeque;

    /// Just enough of the Chisflash firmware to run the reader operations: a GB bus (ROM 0x0000-0x7FFF with an MBC5
    /// bank register, RAM 0xA000-0xBFFF with a RAM bank register) and a GBA ROM / SRAM.
    struct Sim {
        rom: Vec<u8>,
        gb_ram: Vec<u8>,
        sram: Vec<u8>,
        rom_bank: usize,
        ram_bank: usize,
        ram_on: bool,
        pending: Vec<u8>,
        out: VecDeque<u8>,
    }

    impl Sim {
        fn new(rom: Vec<u8>) -> Self {
            Self { rom, gb_ram: vec![0; 0x8000], sram: vec![0; 0x8000], rom_bank: 1, ram_bank: 0, ram_on: false, pending: vec![], out: VecDeque::new() }
        }
        fn gb_write(&mut self, a: usize, v: u8) {
            match a {
                0x0000..=0x1FFF => self.ram_on = v & 0x0F == 0x0A,
                0x2000..=0x2FFF => self.rom_bank = (self.rom_bank & 0x100) | v as usize,
                0x3000..=0x3FFF => self.rom_bank = (self.rom_bank & 0xFF) | ((v as usize & 1) << 8),
                0x4000..=0x5FFF => self.ram_bank = v as usize & 0x0F,
                0xA000..=0xBFFF if self.ram_on => self.gb_ram[self.ram_bank * 0x2000 + a - 0xA000] = v,
                _ => {}
            }
        }
        fn gb_read(&self, a: usize) -> u8 {
            match a {
                0x0000..=0x3FFF => self.rom[a],
                0x4000..=0x7FFF => self.rom[self.rom_bank * 0x4000 + a - 0x4000],
                0xA000..=0xBFFF if self.ram_on => self.gb_ram[self.ram_bank * 0x2000 + a - 0xA000],
                _ => 0xFF,
            }
        }
        fn handle(&mut self, f: Vec<u8>) {
            let body = &f[3..f.len() - 2];
            let addr = u32::from_le_bytes(body[..4].try_into().unwrap()) as usize;
            let n = || u16::from_le_bytes([body[4], body[5]]) as usize;
            match f[2] {
                cmd::GBC_WRITE => {
                    for (i, &v) in body[4..].iter().enumerate() {
                        self.gb_write((addr + i) & 0xFFFF, v);
                    }
                    self.out.push_back(ACK);
                }
                cmd::GBC_READ => {
                    self.out.extend([0, 0]);
                    let bytes: Vec<u8> = (0..n()).map(|i| self.gb_read((addr + i) & 0xFFFF)).collect();
                    self.out.extend(bytes);
                }
                cmd::ROM_READ => {
                    self.out.extend([0, 0]);
                    self.out.extend(self.rom[addr..addr + n()].to_vec());
                }
                cmd::RAM_READ => {
                    self.out.extend([0, 0]);
                    self.out.extend(self.sram[addr..addr + n()].to_vec());
                }
                cmd::RAM_WRITE => {
                    self.sram[addr..addr + body.len() - 4].copy_from_slice(&body[4..]);
                    self.out.push_back(ACK);
                }
                c => panic!("unexpected command {c:#04x}"),
            }
        }
    }

    impl Transport for Sim {
        fn write_all(&mut self, data: &[u8]) -> Result<()> {
            self.pending.extend_from_slice(data);
            if self.pending.len() > 2 && u16::from_le_bytes([self.pending[0], self.pending[1]]) as usize == self.pending.len() {
                let f = std::mem::take(&mut self.pending);
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
    }

    /// 64 KiB Game Boy ROM (4 banks), each byte tells where it is; a valid-looking header is not needed here.
    fn gb_rom() -> Vec<u8> {
        (0..0x10000usize).map(|i| (i % 251) as u8 ^ (i / 0x4000) as u8).collect()
    }

    fn reader(rom: Vec<u8>) -> Chis<Sim> {
        Chis::new(Sim::new(rom)).unwrap()
    }

    #[test]
    fn kinds_of_readers_and_cartridges() {
        for k in [ReaderKind::GbxCart, ReaderKind::Chisflash] {
            assert_eq!(k.cart_kinds(), &[CartKind::Gb, CartKind::Gba]);
            assert!(!k.is_placeholder());
        }
        assert_eq!(ReaderKind::KrikzzMd.cart_kinds(), &[CartKind::MegaDrive]);
        assert!(ReaderKind::KrikzzMd.is_placeholder() && CartKind::MegaDrive.is_placeholder());
        assert!(!CartKind::Gb.is_placeholder() && !CartKind::Gba.is_placeholder());
        for m in [CartMode::Dmg, CartMode::Agb] {
            assert_eq!(CartKind::from_mode(m).mode(), Some(m));
        }
        assert_eq!(CartKind::MegaDrive.mode(), None);
        assert_eq!((CartKind::Gb.label(), CartKind::MegaDrive.label()), ("GB/GBC", "Mega Drive"));
        assert_eq!(ReaderKind::Chisflash.label(), "Chisflash");
    }

    #[test]
    fn the_chisflash_describes_itself_without_a_voltage() {
        let r = reader(vec![0; 16]);
        let i = CartReader::info(&r, CartMode::Dmg);
        assert_eq!((i.name.as_str(), i.voltage), ("Chisflash", ""));
        assert_eq!(CartReader::kind(&r), ReaderKind::Chisflash);
    }

    #[test]
    fn the_mode_only_changes_which_protocol_reads_the_header() {
        let rom = gb_rom();
        let mut r = reader(rom.clone());
        assert_eq!(CartReader::read_header(&mut r, CartMode::Dmg).unwrap(), rom[..HEADER_LEN]);
        assert_eq!(CartReader::read_header(&mut r, CartMode::Agb).unwrap(), rom[..HEADER_LEN]);
    }

    #[test]
    fn the_dump_walks_the_banks_and_sums_like_the_gbxcart_does() {
        let rom = gb_rom();
        let mut r = reader(rom.clone());
        let mut out = Vec::new();
        let mut last = 0;
        let sums = CartReader::dump_dmg_rom(&mut r, DmgMapper::Mbc5, rom.len(), &mut out, &mut |n| {
            last = n;
            true
        })
        .unwrap();
        assert_eq!(out, rom);
        assert_eq!(last, rom.len());
        let expect: u16 = rom.iter().enumerate().filter(|(i, _)| *i != 0x14E && *i != 0x14F).fold(0u16, |s, (_, &b)| s.wrapping_add(b as u16));
        assert_eq!((sums.crc32, sums.checksum16), (crc32fast::hash(&rom), expect));
    }

    #[test]
    fn a_cancelled_dump_stops_with_cancelled() {
        let rom = gb_rom();
        let mut r = reader(rom.clone());
        let e = CartReader::dump_dmg_rom(&mut r, DmgMapper::Mbc5, rom.len(), &mut Vec::new(), &mut |n| n < 0x8000).unwrap_err();
        assert!(matches!(e, Error::Cancelled));
    }

    #[test]
    fn gb_ram_round_trip_through_the_banks_and_disabled_after() {
        let mut r = reader(gb_rom());
        let save: Vec<u8> = (0..0x6000usize).map(|i| (i * 13 + 5) as u8).collect(); // 3 banks, the last one partial
        CartReader::write_dmg_ram(&mut r, DmgMapper::Mbc5, &save, &mut |_| true).unwrap();
        let sim = r.into_inner();
        assert!(!sim.ram_on, "RAM must be disabled again");
        assert_eq!(&sim.gb_ram[..save.len()], &save[..]);
        let mut r = Chis::new(sim).unwrap();
        let back = CartReader::read_dmg_ram(&mut r, DmgMapper::Mbc5, save.len(), &mut |_| true).unwrap();
        assert_eq!(back, save);
        assert!(!r.into_inner().ram_on);
    }

    #[test]
    fn gba_sram_round_trip_and_no_power_cycle() {
        let mut r = reader(vec![0; 64]);
        let save: Vec<u8> = (0..0x2800usize).map(|i| (i * 7) as u8).collect();
        let mut steps = 0;
        CartReader::write_agb_sram(&mut r, &save, &mut |_| {
            steps += 1;
            true
        })
        .unwrap();
        assert_eq!(steps, save.len().div_ceil(SAVE_WRITE_BLOCK));
        assert_eq!(CartReader::read_agb_sram(&mut r, 0, save.len()).unwrap(), save);
        assert!(!CartReader::can_power_cycle(&r));
        assert!(!CartReader::power_cycle(&mut r, Duration::from_millis(1)).unwrap());
        assert!(matches!(CartReader::write_agb_sram(&mut r, &save, &mut |_| false), Err(Error::Cancelled)));
    }
}
