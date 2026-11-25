//! Flash chip identification, following FlashGBX's `DetectFlash` for the profiles with a plain
//! "reset / read identifier" sequence. It writes only reset, unlock and identifier-read commands.
use crate::cfi::{self, Cfi};
use crate::profile::{Cmds, Profile};
use zerocart_core::GbxCart;
use std::collections::BTreeMap;

/// What detection needs from the reader. All writes go through the flash-cartridge bus.
pub trait FlashBus {
    fn write_cmds(&mut self, cmds: &[(u32, u16)]) -> Result<(), String>;
    fn read(&mut self, address: u32, len: usize) -> Result<Vec<u8>, String>;
    /// Game Boy only: 1 = WR pin, 2 = AUDIO pin as write-enable.
    fn set_we_pin(&mut self, pin: u8) -> Result<(), String>;
}

impl<T: zerocart_core::Transport> FlashBus for zerocart_core::GbxCart<T> {
    fn write_cmds(&mut self, cmds: &[(u32, u16)]) -> Result<(), String> {
        if cmds.is_empty() {
            return Ok(());
        }
        self.flash_write(cmds, true).map_err(|e| e.to_string())
    }
    fn read(&mut self, address: u32, len: usize) -> Result<Vec<u8>, String> {
        self.read_rom(address, len).map_err(|e| e.to_string())
    }
    fn set_we_pin(&mut self, pin: u8) -> Result<(), String> {
        GbxCart::set_we_pin(self, pin).map_err(|e| e.to_string())
    }
}

/// The Chisflash reader (original firmware): flash commands go out as plain single-byte writes on the Game Boy
/// cartridge bus and reads as bus reads, so the firmware's own programming algorithm is not used. Game Boy only:
/// the write-enable pin cannot be chosen (the cartridge's WR line is used).
impl<T: zerocart_core::Transport> FlashBus for zerocart_core::chis::Chis<T> {
    fn write_cmds(&mut self, cmds: &[(u32, u16)]) -> Result<(), String> {
        for &(a, v) in cmds {
            self.gbc_write(a, &[v as u8]).map_err(|e| e.to_string())?;
        }
        Ok(())
    }
    fn read(&mut self, address: u32, len: usize) -> Result<Vec<u8>, String> {
        self.gbc_read(address, len).map_err(|e| e.to_string())
    }
    fn set_we_pin(&mut self, _: u8) -> Result<(), String> {
        Ok(())
    }
}

#[derive(Debug, Default)]
pub struct Detection {
    /// Indexes into the profile slice, in file order.
    pub matches: Vec<usize>,
    /// Raw identifier reads that differed from the baseline, as text lines.
    pub id_lines: Vec<String>,
    pub cfi: Option<Cfi>,
    /// The write-enable pin that worked (Game Boy): "WR" or "AUDIO".
    pub we_pin: Option<&'static str>,
    /// Identifier command lists that made the chip answer differently from a plain read: the profiles that use
    /// one of them are the candidates for a chip that no profile matched.
    pub answered: Vec<Cmds>,
}

fn id_bytes(buf: &[u8], len: usize, offsets: Option<&Vec<usize>>) -> Vec<u8> {
    let idx: Vec<usize> = match offsets {
        Some(o) => o.iter().copied().take(len).collect(),
        None => (0..len).collect(),
    };
    if idx.len() != len || idx.iter().any(|&o| o >= buf.len()) {
        return vec![];
    }
    idx.iter().map(|&o| buf[o]).collect()
}

type Key = (Cmds, u32);

pub fn detect(bus: &mut dyn FlashBus, profiles: &[Profile], kind: &str) -> Result<Detection, String> {
    let agb = kind == "AGB";
    let mut resets: Vec<Cmds> = Vec::new();
    // groups of profiles sharing one identifier command: key -> (profile indexes, bytes to read)
    let mut groups: BTreeMap<Key, (Vec<usize>, usize)> = BTreeMap::new();
    for (i, p) in profiles.iter().enumerate() {
        if p.kind != kind || p.command_set.is_none() || p.manual_select || p.special {
            continue;
        }
        for list in [&p.reset, &p.unlock] {
            if !list.is_empty() && !resets.contains(list) {
                resets.push(list.clone());
            }
        }
        if p.read_identifier.is_empty() || (!agb && p.read_identifier[0].0 > 0x7000) {
            continue;
        }
        let id_len = p.flash_ids.iter().map(Vec::len).max().unwrap_or(0);
        let offs = p.flash_id_offsets.as_ref().and_then(|o| o.iter().max()).map_or(0, |m| m + 1);
        let e = groups.entry((p.read_identifier.clone(), p.read_identifier_at)).or_insert((vec![], 0x800));
        e.0.push(i);
        e.1 = e.1.max(id_len).max(offs);
    }
    for list in &resets {
        bus.write_cmds(list)?;
    }

    let mut out = Detection::default();
    // CFI query candidates (pin, reset, query, read address) taken from every profile that shares an identifier
    // command, as FlashGBX does for chips that no profile matches.
    let mut candidates: Vec<(u8, Cmds, Cmds, u32)> = Vec::new();
    let pins: &[u8] = if agb { &[0] } else { &[1, 2] };
    for &pin in pins {
        if !agb {
            bus.set_we_pin(pin)?;
        }
        let we_name = match pin {
            1 => "WR",
            2 => "AUDIO",
            _ => "",
        };
        for ((ident, at), (members, mut len)) in &groups {
            if agb {
                len += len % 2;
            }
            let reset = members.first().map(|&i| profiles[i].reset.clone()).unwrap_or_default();
            let res = (|| {
                bus.write_cmds(&reset)?;
                let baseline = bus.read(*at, len)?;
                bus.write_cmds(ident)?;
                let cmp = bus.read(*at, len)?;
                Ok::<_, String>((baseline, cmp))
            })();
            bus.write_cmds(&reset)?;
            let (baseline, cmp) = res?;
            for &i in members {
                let p = &profiles[i];
                if !agb && p.write_pin.as_deref().is_some_and(|w| w != we_name) {
                    continue;
                }
                let query = p.read_cfi.clone().unwrap_or_else(|| vec![(ident[0].0, 0x98)]);
                let cand = (pin, p.reset.clone(), query, *at);
                if !candidates.contains(&cand) {
                    candidates.push(cand);
                }
                for expected in &p.flash_ids {
                    let off = p.flash_id_offsets.as_ref();
                    if !expected.is_empty()
                        && id_bytes(&cmp, expected.len(), off) == *expected
                        && id_bytes(&baseline, expected.len(), off) != *expected
                    {
                        if !out.matches.contains(&i) {
                            out.matches.push(i);
                        }
                        if out.we_pin.is_none() && !agb {
                            out.we_pin = Some(if pin == 1 { "WR" } else { "AUDIO" });
                        }
                    }
                }
            }
            if baseline != cmp {
                if !out.answered.contains(ident) {
                    out.answered.push(ident.clone());
                }
                let shown: Vec<String> = cmp.iter().take(8).map(|b| format!("{b:02X}")).collect();
                out.id_lines.push(format!("[{} {:X}/{:X}] {}", we_name, ident[0].0, ident[0].1, shown.join(" ")));
            }
        }
        if !out.matches.is_empty() {
            break; // found with this pin: no need to try the other one
        }
    }
    bus.write_cmds(&[(0, 0xFF), (0, 0xF0)])?;
    out.matches.sort_unstable();

    // CFI: the matched profile's query command, else every candidate, else the plain default.
    let mut attempts: Vec<(u8, Cmds, Cmds, u32)> = Vec::new();
    if let Some(p) = out.matches.first().map(|&i| &profiles[i]) {
        let query = p.read_cfi.clone().unwrap_or_else(|| match p.command_set.as_deref() {
            // FlashGBX's per-command-set default (AMD: 0xAA, Intel: 0)
            Some("AMD") => vec![(0xAA, 0x98)],
            Some("INTEL") => vec![(0, 0x98)],
            _ => vec![(p.read_identifier.first().map_or(0, |c| c.0), 0x98)],
        });
        let pin = out.we_pin.map_or(1, |w| if w == "WR" { 1 } else { 2 });
        attempts.push((if agb { 0 } else { pin }, p.reset.clone(), query, p.read_identifier_at));
    } else {
        attempts.extend(candidates);
        for &pin in pins {
            attempts.push((pin, vec![(0, 0xF0)], vec![(0, 0x98)], 0));
        }
    }
    for (pin, reset, query, at) in attempts {
        if !agb {
            bus.set_we_pin(pin)?;
        }
        bus.write_cmds(&reset)?;
        bus.write_cmds(&query)?;
        let buf = bus.read(at, 0x400);
        bus.write_cmds(&reset)?;
        if let Some(c) = buf.ok().and_then(|b| cfi::parse(&b)) {
            out.cfi = Some(c);
            break;
        }
    }
    if !agb {
        bus.set_we_pin(1)?; // back to the normal pin
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::profile::parse;

    /// A chip that answers an AMD "autoselect" with its ID, and a CFI query block, until reset.
    struct Chip {
        id: Vec<u8>,
        mode: u8, // 0 array, 1 id, 2 cfi
        seq: Vec<(u32, u16)>,
        writes: Vec<(u32, u16)>,
    }
    impl FlashBus for Chip {
        fn write_cmds(&mut self, cmds: &[(u32, u16)]) -> Result<(), String> {
            for &c in cmds {
                self.writes.push(c);
                if c == (0, 0xF0) || c == (0, 0xFF) {
                    self.mode = 0;
                    self.seq.clear();
                    continue;
                }
                self.seq.push(c);
                if self.seq.ends_with(&[(0xAAA, 0xAA), (0x555, 0x55), (0xAAA, 0x90)]) {
                    self.mode = 1;
                }
                if c == (0xAAA, 0x98) || c == (0, 0x98) {
                    self.mode = 2;
                }
            }
            Ok(())
        }
        fn read(&mut self, _a: u32, len: usize) -> Result<Vec<u8>, String> {
            let mut v = vec![0xFFu8; len];
            match self.mode {
                1 => v[..self.id.len()].copy_from_slice(&self.id),
                2 if len >= 0x400 => {
                    v[0x20] = b'Q';
                    v[0x22] = b'R';
                    v[0x24] = b'Y';
                    v[0x36] = 0x27;
                    v[0x38] = 0x36;
                    v[0x4E] = 21; // 2 MiB
                    v[0x54] = 0;
                    v[0x58] = 1;
                    v[0x5A] = 31;
                    v[0x5E] = 0;
                    v[0x60] = 1;
                }
                _ => {}
            }
            Ok(v)
        }
        fn set_we_pin(&mut self, _: u8) -> Result<(), String> {
            Ok(())
        }
    }

    fn profiles() -> Vec<Profile> {
        let a = r#"{"type":"DMG","names":["Chip A"],"command_set":"AMD","flash_ids":[[0x01,0xAD]],"write_pin":"WR",
          "commands":{"reset":[[0,0xF0]],"read_identifier":[[0xAAA,0xAA],[0x555,0x55],[0xAAA,0x90]],"read_cfi":[[0xAAA,0x98]]}}"#;
        let b = r#"{"type":"DMG","names":["Chip B"],"command_set":"AMD","flash_ids":[[0xC2,0x22]],"write_pin":"WR",
          "commands":{"reset":[[0,0xF0]],"read_identifier":[[0xAAA,0xAA],[0x555,0x55],[0xAAA,0x90]]}}"#;
        vec![parse("fc_a.txt", a).unwrap(), parse("fc_b.txt", b).unwrap()]
    }

    #[test]
    fn finds_the_chip_by_its_id_and_reads_cfi() {
        let mut chip = Chip { id: vec![0x01, 0xAD], mode: 0, seq: vec![], writes: vec![] };
        let d = detect(&mut chip, &profiles(), "DMG").unwrap();
        assert_eq!(d.matches, vec![0], "only Chip A matches");
        assert_eq!(d.we_pin, Some("WR"));
        assert_eq!(d.cfi.unwrap().device_size, 2 << 20);
        assert_eq!(chip.mode, 0, "the chip is left in array (read) mode");
    }

    #[test]
    fn a_plain_rom_matches_nothing() {
        // ROM never changes on a write: the ID read equals the baseline, so nothing is detected.
        struct Rom;
        impl FlashBus for Rom {
            fn write_cmds(&mut self, _: &[(u32, u16)]) -> Result<(), String> { Ok(()) }
            fn read(&mut self, _: u32, len: usize) -> Result<Vec<u8>, String> { Ok(vec![0x01; len]) }
            fn set_we_pin(&mut self, _: u8) -> Result<(), String> { Ok(()) }
        }
        let d = detect(&mut Rom, &profiles(), "DMG").unwrap();
        assert!(d.matches.is_empty() && d.cfi.is_none());
    }

    #[test]
    fn only_reset_unlock_and_identify_commands_are_written() {
        let mut chip = Chip { id: vec![0x01, 0xAD], mode: 0, seq: vec![], writes: vec![] };
        detect(&mut chip, &profiles(), "DMG").unwrap();
        let allowed = [(0, 0xF0), (0, 0xFF), (0xAAA, 0xAA), (0x555, 0x55), (0xAAA, 0x90), (0xAAA, 0x98), (0, 0x98)];
        assert!(chip.writes.iter().all(|w| allowed.contains(w)), "{:?}", chip.writes);
    }
}
