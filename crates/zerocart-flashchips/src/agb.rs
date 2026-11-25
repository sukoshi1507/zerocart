//! GBA flash cartridges: whole-chip erase and programming through the reader's firmware. Supported so far: the
//! AMD command set with single (unbuffered) writes, which is what the generic profiles use.
use crate::detect::FlashBus;
use crate::profile::Profile;
use crate::program::{erase_plan_for, programmer, runs};
use zerocart_core::{CartMode, GbxCart, Transport};
use std::time::Duration;

/// Erases the whole chip with the profile's `chip_erase` list and waits until it reads back as erased.
/// `poll` is the delay between status reads, `max_polls` the limit, `cmd_delay` the pause after each command.
pub fn chip_erase(bus: &mut dyn FlashBus, p: &Profile, cmd_delay: Duration, poll: Duration, max_polls: u32) -> Result<(), String> {
    let plan = erase_plan_for(p, "chip_erase", 0)?;
    bus.write_cmds(&p.reset)?;
    for step in plan {
        if let Some((a, v)) = step.write {
            bus.write_cmds(&[(a, v)])?;
        }
        std::thread::sleep(cmd_delay);
        if let Some((a, value, mask)) = step.wait {
            let mut polls = 0;
            loop {
                bus.read(a, 2)?; // dummy read: some bootlegs need it
                let r = bus.read(a, 2)?;
                if r.len() < 2 {
                    return Err("short status read".into());
                }
                if u16::from_le_bytes([r[0], r[1]]) & mask == value {
                    break;
                }
                polls += 1;
                if polls >= max_polls {
                    return Err(format!("chip erase timed out (last status {:02X}{:02X})", r[1], r[0]));
                }
                std::thread::sleep(poll);
            }
        }
    }
    bus.write_cmds(&p.reset)
}

/// Checks that the chip answers `p`'s identifier command (the identifier read differs from the plain read), and,
/// when the profile lists flash IDs, that one of them matches. Returns the identifier bytes.
pub fn verify_profile(bus: &mut dyn FlashBus, p: &Profile) -> Result<Vec<u8>, String> {
    if p.read_identifier.is_empty() {
        return Err("profile has no identifier command".into());
    }
    let at = p.read_identifier_at;
    bus.write_cmds(&p.reset)?;
    let baseline = bus.read(at, 0x40)?;
    bus.write_cmds(&p.read_identifier)?;
    let id = bus.read(at, 0x40);
    bus.write_cmds(&p.reset)?;
    let id = id?;
    if id == baseline {
        return Err("the chip does not answer this profile's identifier command".into());
    }
    if !p.flash_ids.is_empty() && !p.flash_ids.iter().any(|e| id.starts_with(e)) {
        return Err(format!("the chip answered {:02X?}, which this profile does not list", &id[..8.min(id.len())]));
    }
    Ok(id[..8.min(id.len())].to_vec())
}

/// Picks the profile to use for a GBA chip: the one detection matched, or else the first profile (AMD, single
/// write) whose identifier command the chip answered and whose listed IDs (if any) agree. Profiles are tried in
/// file order, those that list IDs first. Returns an index into `profiles`.
pub fn choose_profile(
    bus: &mut dyn FlashBus,
    profiles: &[Profile],
    matches: &[usize],
    answered: &[crate::profile::Cmds],
) -> Result<usize, String> {
    if let [one] = matches {
        return Ok(*one);
    }
    let mut candidates: Vec<usize> = profiles
        .iter()
        .enumerate()
        .filter(|(_, p)| {
            p.kind == "AGB"
                && p.command_set.as_deref() == Some("AMD")
                && p.raw["commands"].get("single_write").is_some()
                && p.raw["commands"].get("chip_erase").is_some()
                && answered.contains(&p.read_identifier)
        })
        .map(|(i, _)| i)
        .collect();
    candidates.sort_by_key(|&i| profiles[i].flash_ids.is_empty()); // stable: file order inside each group
    for i in candidates {
        if verify_profile(bus, &profiles[i]).is_ok() {
            return Ok(i);
        }
    }
    Err("no GBA flash profile fits this chip".into())
}

pub struct Prepared {
    pub id: Vec<u8>,
}

/// Selects 3.3 V, verifies the chip against `p` and loads the programming parameters into the reader.
pub fn prepare<T: Transport>(dev: &mut GbxCart<T>, p: &Profile) -> Result<Prepared, String> {
    if p.kind != "AGB" || p.command_set.as_deref() != Some("AMD") || p.raw["commands"].get("single_write").is_none() {
        return Err("only GBA / AMD / single-write profiles are supported so far".into());
    }
    if dev.mode != Some(CartMode::Agb) {
        return Err("the reader is not in GBA mode".into());
    }
    dev.set_voltage(true).map_err(|e| e.to_string())?;
    let id = verify_profile(dev, p)?;
    dev.configure_flash_programmer(&programmer(p)?).map_err(|e| e.to_string())?;
    Ok(Prepared { id })
}

fn read_all<T: Transport>(dev: &mut GbxCart<T>, len: usize, progress: &mut dyn FnMut(usize)) -> Result<Vec<u8>, String> {
    let mut out = Vec::with_capacity(len);
    while out.len() < len {
        let n = 0x4000.min(len - out.len());
        out.extend(dev.read_rom(out.len() as u32, n).map_err(|e| e.to_string())?);
        progress(out.len());
    }
    Ok(out)
}

#[derive(Debug, PartialEq, Eq)]
pub struct AgbReport {
    pub attempts: usize,
    pub repaired: usize,
}

/// Erases the whole chip, programs `rom` at offset 0 and verifies. A flash chip can only clear bits when
/// programming, so differing bytes that need no more than that are repaired in place; anything else repeats the
/// erase (at most 2 attempts). `progress(done, total, message)` over the ROM bytes.
pub fn flash_rom<T: Transport>(
    dev: &mut GbxCart<T>,
    p: &Profile,
    rom: &[u8],
    progress: &mut dyn FnMut(usize, usize, &str),
) -> Result<AgbReport, String> {
    prepare(dev, p)?;
    let total = rom.len();
    let mut repaired = 0;
    for attempt in 1..=2 {
        progress(0, total, &format!("erasing the chip (attempt {attempt})"));
        chip_erase(dev, p, Duration::from_millis(100), Duration::from_millis(500), 600)?;
        let mut pos = 0;
        while pos < total {
            let n = 0x10000.min(total - pos);
            dev.flash_program(pos as u32, &rom[pos..pos + n], 0x400).map_err(|e| e.to_string())?;
            pos += n;
            progress(pos, total, "programming");
        }
        dev.write_cmds(&p.reset)?;
        for _ in 0..3 {
            progress(total, total, "verifying");
            let have = read_all(dev, total, &mut |_| {})?;
            let bad: Vec<usize> = have.iter().zip(rom).enumerate().filter(|(_, (h, w))| h != w).map(|(i, _)| i).collect();
            if bad.is_empty() {
                return Ok(AgbReport { attempts: attempt, repaired });
            }
            progress(total, total, &format!("{} byte(s) differ (first at 0x{:X})", bad.len(), bad[0]));
            if bad.iter().any(|&i| rom[i] & !have[i] != 0) {
                break; // needs an erase
            }
            for (at, len) in runs(&bad) {
                dev.flash_program(at as u32, &rom[at..at + len], len).map_err(|e| e.to_string())?;
                repaired += len;
            }
            dev.write_cmds(&p.reset)?;
        }
    }
    Err("the cartridge still differs from the ROM after 2 attempts".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::profile::parse;

    fn profile() -> Profile {
        parse("fc_agb.txt", &std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../configs/fc_AGB_Generic_AAA_A9.txt")).unwrap()).unwrap()
    }

    struct Chip {
        writes: Vec<(u32, u16)>,
        busy: u32,
        in_id: bool,
    }
    impl FlashBus for Chip {
        fn write_cmds(&mut self, c: &[(u32, u16)]) -> Result<(), String> {
            for &x in c {
                self.writes.push(x);
                if x == (0, 0xF0) {
                    self.in_id = false;
                }
                if x == (0xAAA, 0x90) {
                    self.in_id = true;
                }
            }
            Ok(())
        }
        fn read(&mut self, _a: u32, len: usize) -> Result<Vec<u8>, String> {
            if self.in_id {
                let mut v = vec![0u8; len];
                v[..4].copy_from_slice(&[0xC1, 0x00, 0xCB, 0x22]);
                return Ok(v);
            }
            if self.busy > 0 {
                self.busy -= 1;
                return Ok(vec![0; len]);
            }
            Ok(vec![0xFF; len])
        }
        fn set_we_pin(&mut self, _: u8) -> Result<(), String> {
            Ok(())
        }
    }

    #[test]
    fn chip_erase_sends_the_whole_sequence_and_waits_for_all_ones() {
        let mut chip = Chip { writes: vec![], busy: 3, in_id: false };
        chip_erase(&mut chip, &profile(), Duration::ZERO, Duration::ZERO, 50).unwrap();
        assert_eq!(chip.writes.first(), Some(&(0, 0xF0)));
        assert!(chip.writes.contains(&(0xAAA, 0x10)), "the chip-erase command itself");
        assert_eq!(chip.writes.iter().filter(|w| **w == (0xAAA, 0xA9)).count(), 2, "A9/56 unlock written twice");
        assert_eq!(chip.busy, 0);
    }

    #[test]
    fn a_chip_that_never_finishes_erasing_times_out() {
        let mut chip = Chip { writes: vec![], busy: u32::MAX, in_id: false };
        assert!(chip_erase(&mut chip, &profile(), Duration::ZERO, Duration::ZERO, 4).unwrap_err().contains("timed out"));
    }

    #[test]
    fn identification_needs_the_chip_to_change_its_answer() {
        let mut chip = Chip { writes: vec![], busy: 0, in_id: false };
        assert_eq!(verify_profile(&mut chip, &profile()).unwrap(), vec![0xC1, 0, 0xCB, 0x22, 0, 0, 0, 0]);
        struct Rom;
        impl FlashBus for Rom {
            fn write_cmds(&mut self, _: &[(u32, u16)]) -> Result<(), String> { Ok(()) }
            fn read(&mut self, _: u32, len: usize) -> Result<Vec<u8>, String> { Ok(vec![7; len]) }
            fn set_we_pin(&mut self, _: u8) -> Result<(), String> { Ok(()) }
        }
        assert!(verify_profile(&mut Rom, &profile()).is_err(), "a mask ROM never answers");
    }

    fn agb_profiles() -> Vec<Profile> {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../configs");
        crate::profile::load_dir(&dir).0
    }

    #[test]
    fn the_generic_profile_is_chosen_when_only_its_command_makes_the_chip_answer() {
        let profiles = agb_profiles();
        let mut chip = Chip { writes: vec![], busy: 0, in_id: false };
        let answered = vec![profile().read_identifier];
        let i = choose_profile(&mut chip, &profiles, &[], &answered).unwrap();
        assert_eq!(profiles[i].name, "Generic Flash Cartridge (AAA/A9)");
    }

    #[test]
    fn a_unique_detection_match_is_used_directly_and_an_unanswered_chip_gets_nothing() {
        let profiles = agb_profiles();
        let mut chip = Chip { writes: vec![], busy: 0, in_id: false };
        assert_eq!(choose_profile(&mut chip, &profiles, &[7], &[]).unwrap(), 7);
        assert!(choose_profile(&mut chip, &profiles, &[], &[]).is_err(), "no answered command: nothing is tried");
        assert!(chip.writes.is_empty(), "and nothing was written to the chip");
    }

    #[test]
    fn profile_flash_ids_are_enforced_when_present() {
        let mut p = profile();
        p.flash_ids = vec![vec![0xC1, 0x00, 0x7D, 0x22]];
        let mut chip = Chip { writes: vec![], busy: 0, in_id: false };
        assert!(verify_profile(&mut chip, &p).unwrap_err().contains("does not list"));
    }
}
