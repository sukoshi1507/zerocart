//! CFI (Common Flash Interface) query data, as read after the "CFI query" command.

#[derive(Debug, Clone, PartialEq)]
pub struct Cfi {
    pub device_size: u64,
    pub vdd_min: f32,
    pub vdd_max: f32,
    pub buffer_size: Option<u32>,
    /// (sector size, number of sectors, region size)
    pub regions: Vec<(u32, u32, u64)>,
    /// Data pins that are wired swapped, e.g. [(0,1)].
    pub swapped: Vec<(u8, u8)>,
    /// Boot sector flag of the PRI table (0 = absent, 2 = "as shown", 3 = reversed).
    pub tb_boot_raw: u8,
}

fn swap(n: u8, (p, q): (u8, u8)) -> u8 {
    if ((n >> p) ^ (n >> q)) & 1 == 1 { n ^ (1 << p) ^ (1 << q) } else { n }
}

/// Parses 0x400 bytes of query data read on a 16-bit bus (one useful byte per two). `None` if there is no CFI.
pub fn parse(raw: &[u8]) -> Option<Cfi> {
    if raw.len() < 0x400 {
        return None;
    }
    let swapped: Vec<(u8, u8)> = match [raw[0x20], raw[0x22], raw[0x24]] {
        [b'Q', b'R', b'Y'] => vec![],
        [b'R', b'Q', b'Z'] => vec![(0, 1)],
        [0x92, 0x91, 0x9A] => vec![(0, 1), (6, 7)],
        _ => return None,
    };
    let mut b = raw.to_vec();
    for &pair in &swapped {
        for x in b.iter_mut() {
            *x = swap(*x, pair);
        }
    }
    if b[0x36] == 0xFF && b[0x48] == 0xFF {
        return None; // no voltage information: not trustworthy
    }
    let volts = |x: u8| (x >> 4) as f32 + (x & 0x0F) as f32 / 10.0;
    let bs = (b[0x56] as u32) << 8 | b[0x54] as u32;
    let mut regions = Vec::new();
    for i in 0..(b[0x58] as usize).min(4) {
        let blocks = ((b[0x5C + i * 8] as u32) << 8 | b[0x5A + i * 8] as u32) + 1;
        let size = ((b[0x60 + i * 8] as u32) << 8 | b[0x5E + i * 8] as u32) * 256;
        regions.push((size, blocks, blocks as u64 * size as u64));
    }
    let pri = {
        let a = ((b[0x2A] as usize) | (b[0x2C] as usize) << 8) * 2;
        if a + 0x3C >= 0x400 { 0x80 } else { a }
    };
    let tb_boot_raw = if [b[pri], b[pri + 2], b[pri + 4]] == *b"PRI" && !matches!(b[pri + 0x1E], 0 | 0xFF) {
        b[pri + 0x1E]
    } else {
        0
    };
    Some(Cfi {
        tb_boot_raw,
        device_size: 1u64.checked_shl(b[0x4E] as u32)?,
        vdd_min: volts(b[0x36]),
        vdd_max: volts(b[0x38]),
        buffer_size: (bs > 1).then(|| 1u32 << bs.min(20)),
        regions,
        swapped,
    })
}

impl Cfi {
    /// Sectors as `(offset, size)` in address order. The CFI lists the regions as the chip reports them; when the
    /// boot-sector flag says "reversed" (0x03), FlashGBX reverses the list, and so does this.
    pub fn sectors(&self) -> Vec<(u64, u32)> {
        let mut regions = self.regions.clone();
        if self.tb_boot_raw == 0x03 {
            regions.reverse();
        }
        let mut out = Vec::new();
        let mut pos = 0u64;
        for (size, count, _) in regions {
            for _ in 0..count {
                out.push((pos, size));
                pos += size as u64;
            }
        }
        out
    }

    /// The sectors grouped as `(size, count)` runs in address order, e.g. `[(0x2000, 8), (0x10000, 127)]`.
    pub fn sector_runs(&self) -> Vec<(u32, u32)> {
        let mut runs: Vec<(u32, u32)> = Vec::new();
        for (_, size) in self.sectors() {
            match runs.last_mut() {
                Some(r) if r.0 == size => r.1 += 1,
                _ => runs.push((size, 1)),
            }
        }
        runs
    }

    pub fn describe(&self) -> String {
        let mut s = format!(
            "boot flag {:#04x}; {} MiB, {:.1}-{:.1} V{}",
            self.tb_boot_raw,
            self.device_size as f64 / 1048576.0,
            self.vdd_min,
            self.vdd_max,
            self.buffer_size.map(|b| format!(", buffered write {b} bytes")).unwrap_or_default()
        );
        for (i, (size, n, _)) in self.regions.iter().enumerate() {
            s += &format!("\n  region {}: {} x 0x{:X} bytes", i + 1, n, size);
        }
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Vec<u8> {
        let mut b = vec![0u8; 0x400];
        b[0x20] = b'Q';
        b[0x22] = b'R';
        b[0x24] = b'Y';
        b[0x36] = 0x27; // 2.7 V
        b[0x38] = 0x36; // 3.6 V
        b[0x4E] = 22; // 4 MiB
        b[0x54] = 5; // 32-byte buffer
        b[0x58] = 1;
        b[0x5A] = 63; // 64 blocks
        b[0x5E] = 0; // 0x10000 bytes: 0x100 * 256
        b[0x60] = 1;
        b
    }

    #[test]
    fn parses_a_query_block() {
        let c = parse(&sample()).unwrap();
        assert_eq!(c.device_size, 4 << 20);
        assert_eq!((c.vdd_min, c.vdd_max), (2.7, 3.6));
        assert_eq!(c.buffer_size, Some(32));
        assert_eq!(c.regions, vec![(0x10000, 64, 0x400000)]);
    }

    #[test]
    fn reversed_boot_flag_puts_the_small_sectors_last() {
        let mut c = parse(&sample()).unwrap();
        c.regions = vec![(0x2000, 8, 0x10000), (0x10000, 63, 0x3F0000)];
        c.tb_boot_raw = 0x03;
        let s = c.sectors();
        assert_eq!(s.len(), 71);
        assert_eq!(s[0], (0, 0x10000));
        assert_eq!(s[62], (62 * 0x10000, 0x10000));
        assert_eq!(s[63], (63 * 0x10000, 0x2000));
        assert_eq!(s[70].0 + s[70].1 as u64, 0x400000);
        c.tb_boot_raw = 0;
        assert_eq!(c.sectors()[0], (0, 0x2000), "as listed when not reversed");
    }

    #[test]
    fn sector_runs_follow_the_address_order() {
        let mut c = parse(&sample()).unwrap();
        c.regions = vec![(0x2000, 8, 0x10000), (0x10000, 63, 0x3F0000)];
        c.tb_boot_raw = 0x02; // as listed: small sectors first
        assert_eq!(c.sector_runs(), vec![(0x2000, 8), (0x10000, 63)]);
        c.tb_boot_raw = 0x03; // reversed: small sectors last
        assert_eq!(c.sector_runs(), vec![(0x10000, 63), (0x2000, 8)]);
    }

    #[test]
    fn rejects_non_cfi_data() {
        assert!(parse(&[0xFF; 0x400]).is_none());
        assert!(parse(&[0; 16]).is_none());
    }

    #[test]
    fn handles_swapped_data_lines() {
        let mut b = sample();
        for x in b.iter_mut() {
            *x = swap(*x, (0, 1));
        }
        let c = parse(&b).unwrap();
        assert_eq!(c.swapped, vec![(0, 1)]);
        assert_eq!(c.device_size, 4 << 20);
    }
}
