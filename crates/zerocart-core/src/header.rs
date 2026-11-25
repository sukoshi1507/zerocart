//! ROM header parsing for Game Boy / Color (0x100-0x14F) and Game Boy Advance (0x000-0x0BF).

const NINTENDO_LOGO: [u8; 48] = [
    0xCE, 0xED, 0x66, 0x66, 0xCC, 0x0D, 0x00, 0x0B, 0x03, 0x73, 0x00, 0x83, 0x00, 0x0C, 0x00, 0x0D, 0x00, 0x08, 0x11,
    0x1F, 0x88, 0x89, 0x00, 0x0E, 0xDC, 0xCC, 0x6E, 0xE6, 0xDD, 0xDD, 0xD9, 0x99, 0xBB, 0xBB, 0x67, 0x63, 0x6E, 0x0E,
    0xEC, 0xCC, 0xDD, 0xDC, 0x99, 0x9F, 0xBB, 0xB9, 0x33, 0x3E,
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DmgHeader {
    pub title: String,
    /// Four-letter Nintendo game code hidden at the end of a CGB title (e.g. "BXTE"), if it looks like one.
    pub game_code: Option<String>,
    pub cgb: CgbSupport,
    pub sgb: bool,
    pub cart_type: u8,
    pub mapper: &'static str,
    pub has_battery: bool,
    pub has_rtc: bool,
    pub rom_size: usize,
    pub ram_size: usize,
    pub logo_ok: bool,
    pub header_checksum_ok: bool,
    pub header_checksum: u8,
    pub global_checksum: u16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CgbSupport {
    No,
    Yes,
    Only,
}

fn clean(b: &[u8]) -> String {
    let end = b.iter().position(|&c| c == 0).unwrap_or(b.len());
    b[..end].iter().map(|&c| if (0x20..0x7F).contains(&c) { c as char } else { '?' }).collect::<String>().trim().to_string()
}

/// (mapper name, battery, rtc) for the cartridge type byte.
fn mapper_info(t: u8) -> (&'static str, bool, bool) {
    match t {
        0x00 => ("ROM ONLY", false, false),
        0x01 => ("MBC1", false, false),
        0x02 => ("MBC1+RAM", false, false),
        0x03 => ("MBC1+RAM+BATTERY", true, false),
        0x05 => ("MBC2", false, false),
        0x06 => ("MBC2+BATTERY", true, false),
        0x08 => ("ROM+RAM", false, false),
        0x09 => ("ROM+RAM+BATTERY", true, false),
        0x0F => ("MBC3+TIMER+BATTERY", true, true),
        0x10 => ("MBC3+TIMER+RAM+BATTERY", true, true),
        0x11 => ("MBC3", false, false),
        0x12 => ("MBC3+RAM", false, false),
        0x13 => ("MBC3+RAM+BATTERY", true, false),
        0x19 => ("MBC5", false, false),
        0x1A => ("MBC5+RAM", false, false),
        0x1B => ("MBC5+RAM+BATTERY", true, false),
        0x1C => ("MBC5+RUMBLE", false, false),
        0x1D => ("MBC5+RUMBLE+RAM", false, false),
        0x1E => ("MBC5+RUMBLE+RAM+BATTERY", true, false),
        0x22 => ("MBC7+SENSOR+RUMBLE+RAM+BATTERY", true, false),
        0xFC => ("POCKET CAMERA", true, false),
        _ => ("unknown", false, false),
    }
}

fn dmg_ram_size(code: u8, mapper: &str) -> usize {
    if mapper.starts_with("MBC2") {
        return 0x200; // 512 x 4 bit
    }
    match code {
        1 => 0x800,
        2 => 0x2000,
        3 => 0x8000,
        4 => 0x20000,
        5 => 0x10000,
        _ => 0,
    }
}

pub fn parse_dmg(h: &[u8]) -> Option<DmgHeader> {
    if h.len() < 0x150 {
        return None;
    }
    let cgb = match h[0x143] {
        0x80 => CgbSupport::Yes,
        0xC0 => CgbSupport::Only,
        _ => CgbSupport::No,
    };
    // The title is 16 bytes, 15 on CGB cartridges (byte 0x143 is the CGB flag). Official titles end with a four-letter
    // game code; as FlashGBX does, only split it off when it looks like one (A/B/H/K/V first, region letter last).
    let title_end = if cgb == CgbSupport::No { 0x144 } else { 0x143 };
    let raw = &h[0x134..title_end];
    let trimmed = &raw[..raw.iter().rposition(|&c| c != 0).map_or(0, |i| i + 1)];
    let (title, game_code) = if cgb != CgbSupport::No && trimmed.len() == 15 && b"ABHKV".contains(&trimmed[11]) && b"ABDEFIJKPSUXY".contains(&trimmed[14]) {
        (clean(&trimmed[..11]), Some(clean(&trimmed[11..])))
    } else {
        (clean(raw), None)
    };
    let cart_type = h[0x147];
    let (mapper, has_battery, has_rtc) = mapper_info(cart_type);
    let sum = h[0x134..=0x14C].iter().fold(0u8, |a, &b| a.wrapping_sub(b).wrapping_sub(1));
    Some(DmgHeader {
        title,
        game_code,
        cgb,
        sgb: h[0x146] == 0x03,
        cart_type,
        mapper,
        has_battery,
        has_rtc,
        rom_size: if h[0x148] <= 8 { 0x8000usize << h[0x148] } else { 0 },
        ram_size: dmg_ram_size(h[0x149], mapper),
        logo_ok: h[0x104..0x134] == NINTENDO_LOGO,
        header_checksum_ok: sum == h[0x14D],
        header_checksum: h[0x14D],
        global_checksum: u16::from_be_bytes([h[0x14E], h[0x14F]]),
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgbHeader {
    pub title: String,
    pub game_code: String,
    pub maker_code: String,
    pub version: u8,
    pub logo_ok: bool,
    pub header_checksum_ok: bool,
}

pub fn parse_agb(h: &[u8]) -> Option<AgbHeader> {
    if h.len() < 0xC0 {
        return None;
    }
    let sum = h[0xA0..=0xBC].iter().fold(0u8, |a, &b| a.wrapping_sub(b)).wrapping_sub(0x19);
    Some(AgbHeader {
        title: clean(&h[0xA0..0xAC]),
        game_code: clean(&h[0xAC..0xB0]),
        maker_code: clean(&h[0xB0..0xB2]),
        version: h[0xBC],
        logo_ok: h[0xB2] == 0x96,
        header_checksum_ok: sum == h[0xBD],
    })
}

/// Rough save-type detection from the ROM text (the GBA SDK library strings), as FlashGBX does.
pub fn detect_agb_save(rom: &[u8]) -> Option<&'static str> {
    const TAGS: [(&[u8], &str); 6] = [
        (b"FLASH1M_V", "FLASH 1M"),
        (b"FLASH512_V", "FLASH 512K"),
        (b"FLASH_V", "FLASH 512K"),
        (b"EEPROM_V", "EEPROM"),
        (b"SRAM_F_V", "FRAM"),
        (b"SRAM_V", "SRAM"),
    ];
    TAGS.iter().find(|(tag, _)| rom.windows(tag.len()).any(|w| w == *tag)).map(|(_, name)| *name)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dmg_rom() -> Vec<u8> {
        let mut h = vec![0u8; 0x150];
        h[0x104..0x134].copy_from_slice(&NINTENDO_LOGO);
        h[0x134..0x13E].copy_from_slice(b"POKEMON RE");
        h[0x147] = 0x13;
        h[0x148] = 4;
        h[0x149] = 3;
        let sum = h[0x134..=0x14C].iter().fold(0u8, |a, &b| a.wrapping_sub(b).wrapping_sub(1));
        h[0x14D] = sum;
        h
    }

    #[test]
    fn parses_dmg_header() {
        let d = parse_dmg(&dmg_rom()).unwrap();
        assert_eq!(d.title, "POKEMON RE");
        assert_eq!(d.mapper, "MBC3+RAM+BATTERY");
        assert!(d.has_battery && !d.has_rtc);
        assert_eq!(d.rom_size, 0x8000 << 4);
        assert_eq!(d.ram_size, 0x8000);
        assert!(d.logo_ok && d.header_checksum_ok);
    }

    #[test]
    fn cgb_titles_keep_15_chars_unless_a_game_code_is_there() {
        let mut h = dmg_rom();
        h[0x143] = 0xC0;
        h[0x134..0x143].copy_from_slice(b"REPUGNANTBOUNTY");
        let d = parse_dmg(&h).unwrap();
        assert_eq!((d.title.as_str(), d.game_code.clone()), ("REPUGNANTBOUNTY", None));
        h[0x134..0x143].copy_from_slice(b"POKEMON YELBXTE");
        let d = parse_dmg(&h).unwrap();
        assert_eq!((d.title.as_str(), d.game_code.as_deref()), ("POKEMON YEL", Some("BXTE")));
    }

    #[test]
    fn bad_checksum_is_flagged() {
        let mut h = dmg_rom();
        h[0x14D] ^= 1;
        assert!(!parse_dmg(&h).unwrap().header_checksum_ok);
    }

    #[test]
    fn parses_agb_header() {
        let mut h = vec![0u8; 0xC0];
        h[0xA0..0xA8].copy_from_slice(b"ZELDA ND");
        h[0xAC..0xB0].copy_from_slice(b"AZLE");
        h[0xB0..0xB2].copy_from_slice(b"01");
        h[0xB2] = 0x96;
        let sum = h[0xA0..=0xBC].iter().fold(0u8, |a, &b| a.wrapping_sub(b)).wrapping_sub(0x19);
        h[0xBD] = sum;
        let a = parse_agb(&h).unwrap();
        assert_eq!((a.title.as_str(), a.game_code.as_str(), a.maker_code.as_str()), ("ZELDA ND", "AZLE", "01"));
        assert!(a.logo_ok && a.header_checksum_ok);
    }

    #[test]
    fn detects_save_type() {
        assert_eq!(detect_agb_save(b"xxFLASH1M_V103xx"), Some("FLASH 1M"));
        assert_eq!(detect_agb_save(b"xxSRAM_V113"), Some("SRAM"));
        assert_eq!(detect_agb_save(b"nothing"), None);
    }
}
