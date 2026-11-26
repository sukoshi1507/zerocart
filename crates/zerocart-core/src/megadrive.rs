// SPDX-License-Identifier: GPL-3.0-or-later
//! Mega Drive / Genesis (and 32X) cartridges: the "SEGA" header, ROM size, checksum.
//!
//! Hardware independent: it only looks at bytes. The layout of the header is the public Sega one; the plausibility
//! rules (what a usable ROM size is, how the region letter is read) follow https://github.com/jfryman/flashkit-md-dotnet
//! (MIT, see THIRD_PARTY.md).

/// A Mega Drive cartridge owns 0x000000-0x3FFFFF; the Mega-CD uses 0x400000-0x7FFFFF. Flat 8 MiB flash carts exist,
/// nothing bigger is accepted.
pub const MAX_CART_BYTES: usize = 8 << 20;
/// Chip size assumed when the flash chip does not tell its own.
pub const DEFAULT_CHIP_BYTES: usize = 4 << 20;
/// Bytes read to parse the header (it ends at 0x200, where the program starts).
pub const HEADER_LEN: usize = 0x200;

/// Save RAM declared by the header ("RA" block at 0x1B0).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HeaderRam {
    /// The RAM is 8-bit on the ODD bytes of the 16-bit bus (the usual case, type 0xF8): the byte count is half the
    /// address range.
    pub odd_bytes: bool,
    pub start: u32,
    pub end: u32,
}

impl HeaderRam {
    /// Bytes of save data the cartridge holds.
    pub fn size(&self) -> usize {
        let span = self.end.wrapping_sub(self.start) as usize + 1;
        if self.odd_bytes { span.div_ceil(2) } else { span }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MdHeader {
    /// "SEGA MEGA DRIVE", "SEGA GENESIS", "SEGA 32X"... (16 bytes at 0x100).
    pub system: String,
    pub copyright: String,
    pub name_domestic: String,
    pub name_overseas: String,
    pub serial: String,
    /// Checksum declared at 0x18E.
    pub checksum: u16,
    pub rom_start: u32,
    pub rom_end: u32,
    pub ram: Option<HeaderRam>,
    pub region: &'static str,
    /// The first 16 bytes after 0x100 contain "SEGA" (some carts pad with a leading space).
    pub sega_ok: bool,
    pub is_32x: bool,
}

fn text(bytes: &[u8]) -> String {
    let s: String = bytes.iter().map(|&b| if (0x20..0x7f).contains(&b) { b as char } else { ' ' }).collect();
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn be32(b: &[u8]) -> u32 {
    u32::from_be_bytes([b[0], b[1], b[2], b[3]])
}

/// Region from the first letters of the field at 0x1F0: a first letter that differs from a meaningful second one
/// means several regions.
pub fn region_name(header: &[u8]) -> &'static str {
    let (a, b) = (header[0x1F0], header[0x1F1]);
    if a != b && b != 0x20 && b != 0 {
        return "World";
    }
    match a {
        b'F' | b'C' => "World",
        b'U' | b'W' | b'4' | 4 => "USA",
        b'J' | b'B' | b'1' | 1 => "Japan",
        b'E' | b'A' | b'8' | 8 => "Europe",
        _ => "Unknown",
    }
}

/// Parses the header from at least [`HEADER_LEN`] bytes read from address 0. Returns `None` only when the buffer is
/// too short: a blank or non-Sega cartridge still parses, with `sega_ok` false.
pub fn parse_header(b: &[u8]) -> Option<MdHeader> {
    if b.len() < HEADER_LEN {
        return None;
    }
    let system = text(&b[0x100..0x110]);
    let ram = (b[0x1B0] == b'R' && b[0x1B1] == b'A' && b[0x1B3] == 0x20)
        .then(|| HeaderRam { odd_bytes: b[0x1B2] & 0x10 != 0 || b[0x1B2] == 0xF8, start: be32(&b[0x1B4..]), end: be32(&b[0x1B8..]) })
        .filter(|r| r.end >= r.start);
    Some(MdHeader {
        sega_ok: system.contains("SEGA"),
        is_32x: system.contains("32X"),
        system,
        copyright: text(&b[0x110..0x120]),
        name_domestic: text(&b[0x120..0x150]),
        name_overseas: text(&b[0x150..0x180]),
        serial: text(&b[0x180..0x18E]),
        checksum: u16::from_be_bytes([b[0x18E], b[0x18F]]),
        rom_start: be32(&b[0x1A0..]),
        rom_end: be32(&b[0x1A4..]),
        ram,
        region: region_name(b),
    })
}

impl MdHeader {
    /// The best name to show: the overseas name, else the domestic one.
    pub fn title(&self) -> &str {
        if !self.name_overseas.is_empty() { &self.name_overseas } else { &self.name_domestic }
    }

    /// ROM size declared by the header, when it is plausible (start at 0, even size, 512 bytes to 8 MiB). A blank
    /// flash cart reads 0xFF everywhere and gives `None`: that is not an image size, do not guess one from it.
    pub fn rom_size(&self) -> Option<usize> {
        let size = self.rom_end as u64 + 1;
        (self.rom_start == 0 && size >= 0x200 && size <= MAX_CART_BYTES as u64 && size.is_multiple_of(2)).then_some(size as usize)
    }

    /// Whether the header looks like a real cartridge's (a Sega signature and a plausible size).
    pub fn is_valid(&self) -> bool {
        self.sega_ok && self.rom_size().is_some()
    }

    /// File extension for a dump of this cartridge.
    pub fn extension(&self) -> &'static str {
        if self.is_32x { "32x" } else { "bin" }
    }
}

/// The Mega Drive checksum: the sum of the big-endian 16-bit words from 0x200 to the end of the ROM, modulo 65536,
/// to compare with the word at 0x18E. (A ROM that is not a whole number of words ignores its last byte.)
pub fn checksum16(rom: &[u8]) -> u16 {
    rom.get(0x200..).unwrap_or(&[]).as_chunks::<2>().0.iter().fold(0u16, |s, w| s.wrapping_add(u16::from_be_bytes([w[0], w[1]])))
}

/// The save data of a stream read from the bus: one byte of data per word, on the ODD byte (the even bytes read as
/// open bus).
pub fn stream_to_data(stream: &[u8]) -> Vec<u8> {
    stream.chunks(2).filter_map(|w| w.get(1).copied()).collect()
}

/// A save file as the bus wants it. Two shapes are accepted: the word stream that `zerocart-md --save` and the
/// application write (`2 * size` bytes, data on the odd bytes) and the plain data (`size` bytes, what an emulator
/// keeps for a cartridge with an 8-bit RAM). `None` for any other length.
pub fn save_file_to_stream(file: &[u8], size: usize) -> Option<Vec<u8>> {
    if file.len() == 2 * size {
        Some(file.to_vec())
    } else if file.len() == size {
        Some(file.iter().flat_map(|&b| [0xFF, b]).collect())
    } else {
        None
    }
}

/// Streaming version of [`checksum16`] for a dump written block by block: feed the bytes in order.
#[derive(Debug, Default, Clone, Copy)]
pub struct Checksum16 {
    sum: u16,
    pos: usize,
    odd: Option<u8>,
}

impl Checksum16 {
    pub fn update(&mut self, data: &[u8]) {
        for &b in data {
            if self.pos >= 0x200 {
                match self.odd.take() {
                    None => self.odd = Some(b),
                    Some(hi) => self.sum = self.sum.wrapping_add(u16::from_be_bytes([hi, b])),
                }
            }
            self.pos += 1;
        }
    }

    pub fn finalize(&self) -> u16 {
        self.sum
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A header like the ones of real cartridges: Sega system field, names, checksum, ROM 0..0x7FFFF, 8 KiB of RAM.
    fn sample(rom_size: usize) -> Vec<u8> {
        let mut b = vec![0x20u8; rom_size.max(0x400)];
        b[0x100..0x110].copy_from_slice(b"SEGA MEGA DRIVE ");
        b[0x110..0x120].copy_from_slice(b"(C)TEST 2026.NOV");
        b[0x120..0x120 + 11].copy_from_slice(b"TEST GAME  ");
        b[0x150..0x150 + 9].copy_from_slice(b"TEST GAME");
        b[0x180..0x18E].copy_from_slice(b"GM 00000000-00");
        b[0x1A0..0x1A4].copy_from_slice(&0u32.to_be_bytes());
        b[0x1A4..0x1A8].copy_from_slice(&((rom_size - 1) as u32).to_be_bytes());
        b[0x1B0..0x1B4].copy_from_slice(&[b'R', b'A', 0xF8, 0x20]);
        b[0x1B4..0x1B8].copy_from_slice(&0x0020_0001u32.to_be_bytes());
        b[0x1B8..0x1BC].copy_from_slice(&0x0020_3FFFu32.to_be_bytes());
        b[0x1F0] = b'U';
        b[0x1F1] = b' ';
        b
    }

    #[test]
    fn a_normal_header_is_read() {
        let b = sample(0x80000);
        let h = parse_header(&b).unwrap();
        assert_eq!((h.system.as_str(), h.title(), h.serial.as_str(), h.region), ("SEGA MEGA DRIVE", "TEST GAME", "GM 00000000-00", "USA"));
        assert!(h.sega_ok && !h.is_32x && h.is_valid());
        assert_eq!(h.rom_size(), Some(0x80000));
        assert_eq!(h.extension(), "bin");
        let ram = h.ram.unwrap();
        assert!(ram.odd_bytes);
        assert_eq!(ram.size(), 0x2000, "0x200001..0x203FFF on the odd bytes is 8 KiB");
    }

    #[test]
    fn a_32x_cartridge_is_told_by_its_header() {
        let mut b = sample(0x40000);
        b[0x100..0x110].copy_from_slice(b"SEGA 32X        ");
        let h = parse_header(&b).unwrap();
        assert!(h.is_32x && h.sega_ok);
        assert_eq!(h.extension(), "32x");
    }

    #[test]
    fn save_files_are_accepted_as_a_word_stream_or_as_plain_data() {
        let data = [0x11u8, 0x22, 0x33];
        let stream = save_file_to_stream(&data, 3).unwrap();
        assert_eq!(stream, [0xFF, 0x11, 0xFF, 0x22, 0xFF, 0x33]);
        assert_eq!(stream_to_data(&stream), data);
        assert_eq!(save_file_to_stream(&stream, 3).unwrap(), stream, "a stream is kept as it is");
        assert!(save_file_to_stream(&[0; 5], 3).is_none() && save_file_to_stream(&[], 3).is_none());
        assert!(stream_to_data(&[1]).is_empty(), "a lone even byte carries no data");
    }

    #[test]
    fn a_blank_flash_cart_has_no_plausible_header() {
        let h = parse_header(&vec![0xFFu8; 0x200]).unwrap();
        assert!(!h.sega_ok && !h.is_valid());
        assert_eq!(h.rom_size(), None, "0xFFFFFFFF + 1 is not an image size");
        assert!(h.ram.is_none());
        assert!(parse_header(&[0u8; 0x1FF]).is_none(), "too short");
    }

    #[test]
    fn implausible_sizes_are_refused() {
        let mut b = sample(0x80000);
        b[0x1A0..0x1A4].copy_from_slice(&0x1000u32.to_be_bytes()); // does not start at 0
        assert_eq!(parse_header(&b).unwrap().rom_size(), None);
        let mut b = sample(0x80000);
        b[0x1A4..0x1A8].copy_from_slice(&(MAX_CART_BYTES as u32).to_be_bytes()); // 8 MiB + 1
        assert_eq!(parse_header(&b).unwrap().rom_size(), None);
        let mut b = sample(0x80000);
        b[0x1A4..0x1A8].copy_from_slice(&0x1FFu32.to_be_bytes()); // 512 bytes: the smallest accepted
        assert_eq!(parse_header(&b).unwrap().rom_size(), Some(0x200));
        b[0x1A4..0x1A8].copy_from_slice(&0x1FEu32.to_be_bytes());
        assert_eq!(parse_header(&b).unwrap().rom_size(), None);
        b[0x1A4..0x1A8].copy_from_slice(&0x200u32.to_be_bytes()); // odd size
        assert_eq!(parse_header(&b).unwrap().rom_size(), None);
    }

    #[test]
    fn regions() {
        let mut b = sample(0x1000);
        for (c, r) in [(b'J', "Japan"), (b'E', "Europe"), (b'U', "USA"), (b'F', "World"), (b'Z', "Unknown")] {
            b[0x1F0] = c;
            b[0x1F1] = b' ';
            assert_eq!(region_name(&b), r);
        }
        b[0x1F0] = b'J';
        b[0x1F1] = b'U'; // several letters: world
        assert_eq!(region_name(&b), "World");
    }

    #[test]
    fn the_checksum_sums_words_from_0x200_and_streams_the_same() {
        let mut rom = vec![0u8; 0x1000];
        for (i, b) in rom.iter_mut().enumerate() {
            *b = (i * 31 + 7) as u8;
        }
        // bytes before 0x200 (the header, including the checksum itself) do not count
        let mut by_hand = 0u16;
        for w in rom[0x200..].as_chunks::<2>().0 {
            by_hand = by_hand.wrapping_add(((w[0] as u16) << 8) | w[1] as u16);
        }
        assert_eq!(checksum16(&rom), by_hand);
        let mut s = Checksum16::default();
        for block in rom.chunks(333) {
            s.update(block); // odd block sizes: words straddle blocks
        }
        assert_eq!(s.finalize(), by_hand);
        assert_eq!(checksum16(&rom[..0x100]), 0);
    }
}
