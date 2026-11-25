//! Game Boy mappers: which registers to write to select a ROM bank (ported from FlashGBX `Mapper.py`).
//! Only the plain, common mappers are here; anything else is refused rather than guessed.

/// Size of one switchable ROM bank.
pub const ROM_BANK_SIZE: usize = 0x4000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DmgMapper {
    RomOnly,
    Mbc1,
    Mbc2,
    Mbc3,
    Mbc5,
}

impl DmgMapper {
    /// From the cartridge type byte of the header (0x147).
    pub fn from_cart_type(t: u8) -> Option<Self> {
        Some(match t {
            0x00 | 0x08 | 0x09 => Self::RomOnly,
            0x01..=0x03 => Self::Mbc1,
            0x05 | 0x06 => Self::Mbc2,
            0x0F..=0x13 => Self::Mbc3,
            0x19..=0x1E => Self::Mbc5,
            _ => return None,
        })
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::RomOnly => "ROM ONLY",
            Self::Mbc1 => "MBC1",
            Self::Mbc2 => "MBC2",
            Self::Mbc3 => "MBC3",
            Self::Mbc5 => "MBC5",
        }
    }

    /// Writes `(address, value)` that put ROM bank `index` at 0x4000, and the address the bank is read from.
    pub fn select_rom_bank(self, index: usize) -> (Vec<(u32, u8)>, u32) {
        match self {
            Self::RomOnly => (vec![], if index == 0 { 0 } else { 0x4000 }),
            Self::Mbc1 => (
                vec![(0x6000, 1), (0x2000, index as u8), (0x4000, (index >> 5) as u8)],
                if index & 0x1F != 0 { 0x4000 } else { 0 },
            ),
            Self::Mbc2 | Self::Mbc3 => (vec![(0x2100, index as u8)], if index == 0 { 0 } else { 0x4000 }),
            Self::Mbc5 => (vec![(0x3000, (index >> 8) as u8), (0x2100, index as u8)], if index == 0 { 0 } else { 0x4000 }),
        }
    }
}

/// Size of one switchable RAM bank.
pub const RAM_BANK_SIZE: usize = 0x2000;

impl DmgMapper {
    /// Writes that enable (or, for `false`, disable) the cartridge RAM. Disabling protects a battery-backed
    /// save from stray writes when the cartridge is powered down.
    pub fn enable_ram(self, enable: bool) -> Vec<(u32, u8)> {
        match (self, enable) {
            (Self::Mbc1, true) => vec![(0x6000, 1), (0x0000, 0x0A)],
            (Self::Mbc1, false) => vec![(0x0000, 0x00), (0x6000, 0x00)],
            (Self::RomOnly, _) => vec![],
            (_, e) => vec![(0x0000, if e { 0x0A } else { 0x00 })],
        }
    }

    /// Writes that put RAM bank `index` at 0xA000.
    pub fn select_ram_bank(self, index: usize) -> Vec<(u32, u8)> {
        match self {
            Self::RomOnly | Self::Mbc2 => vec![],
            _ => vec![(0x4000, index as u8)],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_cart_types() {
        assert_eq!(DmgMapper::from_cart_type(0x1B), Some(DmgMapper::Mbc5));
        assert_eq!(DmgMapper::from_cart_type(0x13), Some(DmgMapper::Mbc3));
        assert_eq!(DmgMapper::from_cart_type(0x03), Some(DmgMapper::Mbc1));
        assert_eq!(DmgMapper::from_cart_type(0x00), Some(DmgMapper::RomOnly));
        assert_eq!(DmgMapper::from_cart_type(0xFC), None); // Pocket Camera: not handled
    }

    #[test]
    fn ram_enable_and_bank_registers() {
        assert_eq!(DmgMapper::Mbc5.enable_ram(true), vec![(0, 0x0A)]);
        assert_eq!(DmgMapper::Mbc5.enable_ram(false), vec![(0, 0)]);
        assert_eq!(DmgMapper::Mbc1.enable_ram(true), vec![(0x6000, 1), (0, 0x0A)]);
        assert_eq!(DmgMapper::Mbc1.enable_ram(false), vec![(0, 0), (0x6000, 0)]);
        assert_eq!(DmgMapper::Mbc3.select_ram_bank(2), vec![(0x4000, 2)]);
        assert!(DmgMapper::Mbc2.select_ram_bank(1).is_empty());
        assert!(DmgMapper::RomOnly.enable_ram(true).is_empty());
    }

    #[test]
    fn mbc5_bank_300_uses_the_ninth_bit() {
        let (w, start) = DmgMapper::Mbc5.select_rom_bank(300);
        assert_eq!(w, vec![(0x3000, 1), (0x2100, 44)]);
        assert_eq!(start, 0x4000);
        assert_eq!(DmgMapper::Mbc5.select_rom_bank(0).1, 0);
    }

    #[test]
    fn mbc1_bank_33_sets_the_upper_bits() {
        let (w, start) = DmgMapper::Mbc1.select_rom_bank(0x21);
        assert_eq!(w, vec![(0x6000, 1), (0x2000, 0x21), (0x4000, 1)]);
        assert_eq!(start, 0x4000);
        assert_eq!(DmgMapper::Mbc1.select_rom_bank(0x20).1, 0); // banks 0x20/0x40 show at 0x0000 in mode 1
    }
}
