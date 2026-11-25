//! Erasing and programming. Only the case verified so far is supported: Game Boy, AMD command set, single
//! (unbuffered) writes through the reader's firmware, WR as write-enable, MBC5 bank switching.
use crate::detect::{detect, FlashBus};
use crate::profile::Profile;
use zerocart_core::mapper::{DmgMapper, ROM_BANK_SIZE};
use zerocart_core::{FlashProgrammer, GbxCart, Transport};
use serde_json::Value;
use std::time::Duration;

/// An address in a profile command: a fixed one, or "SA" (the sector being erased).
fn addr(v: &Value, sa: u32) -> Result<Option<u32>, String> {
    match v {
        Value::Null => Ok(None),
        Value::String(s) if s == "SA" => Ok(Some(sa)),
        Value::String(s) => Err(format!("unsupported address token '{s}'")),
        n => n.as_u64().map(|a| Some(a as u32)).ok_or_else(|| format!("bad address {n}")),
    }
}

/// One step of a sector erase: the command to write, and what to wait for afterwards.
#[derive(Debug, PartialEq, Eq)]
pub struct EraseStep {
    pub write: Option<(u32, u16)>,
    /// `(address, expected value, mask)`: poll until `read & mask == value`
    pub wait: Option<(u32, u16, u16)>,
}

pub fn erase_plan(p: &Profile, sa: u32) -> Result<Vec<EraseStep>, String> {
    erase_plan_for(p, "sector_erase", sa)
}

/// The steps of an erase command list (`sector_erase` or `chip_erase`) with its `_wait_for` list.
pub fn erase_plan_for(p: &Profile, key: &str, sa: u32) -> Result<Vec<EraseStep>, String> {
    let cmds = p.raw["commands"][key].as_array().ok_or(format!("profile has no {key}"))?;
    let waits = p.raw["commands"][format!("{key}_wait_for")].as_array().ok_or(format!("profile has no {key}_wait_for"))?;
    if cmds.len() != waits.len() {
        return Err(format!("{key} and {key}_wait_for differ in length"));
    }
    let mut out = Vec::new();
    for (c, w) in cmds.iter().zip(waits) {
        let write = match addr(&c[0], sa)? {
            Some(a) => Some((a, c[1].as_u64().ok_or("sector_erase value is not a number")? as u16)),
            None => None,
        };
        let wait = match addr(&w[0], sa)? {
            Some(a) => Some((a, w[1].as_u64().ok_or("wait value")? as u16, w[2].as_u64().ok_or("wait mask")? as u16)),
            None => None,
        };
        out.push(EraseStep { write, wait });
    }
    Ok(out)
}

/// Erases the sector at `sa` (the bus address it is reached at) and waits until the chip reports it done.
pub fn erase_sector(bus: &mut dyn FlashBus, p: &Profile, sa: u32, poll: Duration, max_polls: u32) -> Result<(), String> {
    let plan = erase_plan(p, sa)?;
    bus.write_cmds(&p.reset)?;
    for step in plan {
        if let Some((a, v)) = step.write {
            bus.write_cmds(&[(a, v)])?;
        }
        if let Some((a, value, mask)) = step.wait {
            std::thread::sleep(poll);
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
                    return Err(format!("erase timed out (last status {:02X}{:02X})", r[1], r[0]));
                }
                std::thread::sleep(poll);
            }
        }
    }
    bus.write_cmds(&p.reset)
}

/// The part of a sector inside one 16 KiB ROM bank.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Segment {
    pub bank: usize,
    /// Offset inside the bank
    pub start: usize,
    pub len: usize,
    /// Offset inside the sector data
    pub data_at: usize,
}

#[derive(Debug)]
pub struct Plan {
    pub segments: Vec<Segment>,
    /// Bus address the sector is erased at (bank 0 is reached at the profile's `start_addr`, others at 0x4000).
    pub sector_address: u32,
}

/// Bank numbers whose switch value equals the first value of a flash command (0xAA here) need a separator write.
pub fn needs_separator(bank: usize) -> bool {
    bank & 0xFF == 0xAA
}

/// Checks that a sector rewrite is something this code supports, and where it goes.
pub fn plan_rewrite(p: &Profile, offset: u64, len: usize) -> Result<Plan, String> {
    if p.kind != "DMG" || p.command_set.as_deref() != Some("AMD") || p.write_pin.as_deref() != Some("WR") {
        return Err("only Game Boy / AMD / WR-pin profiles are supported so far".into());
    }
    if p.raw["commands"].get("single_write").is_none() {
        return Err("profile has no single_write".into());
    }
    if offset % 0x2000 != 0 || len == 0 || len % 0x2000 != 0 {
        return Err("offset and length must be multiples of 8 KiB".into());
    }
    let (start, end) = (offset as usize, offset as usize + len);
    let mut segments = Vec::new();
    for bank in start / ROM_BANK_SIZE..=(end - 1) / ROM_BANK_SIZE {
        let from = start.max(bank * ROM_BANK_SIZE);
        let to = end.min((bank + 1) * ROM_BANK_SIZE);
        segments.push(Segment { bank, start: from - bank * ROM_BANK_SIZE, len: to - from, data_at: from - start });
    }
    let first = &segments[0];
    let base = if first.bank == 0 { p.raw["start_addr"].as_u64().unwrap_or(0x4000) as u32 } else { 0x4000 };
    Ok(Plan { sector_address: base + first.start as u32, segments })
}

/// Programs `data` at the bus `address` one byte at a time with the profile's `single_write` sequence (the unlock
/// commands, then the byte), polling DQ7 after each byte. Bytes that are 0xFF are skipped (an erased chip already
/// holds them). For readers that only offer plain bus writes. Slow (one command round trip per cycle), exact.
pub fn program_single(bus: &mut dyn FlashBus, p: &Profile, address: u32, data: &[u8], max_polls: u32) -> Result<(), String> {
    let cmds = p.raw["commands"]["single_write"].as_array().ok_or("profile has no single_write")?;
    // the leading commands with a fixed address are the unlock cycles; the last one ("PA", "PD") is the byte
    let unlock: Vec<(u32, u16)> = cmds
        .iter()
        .take_while(|c| c[0].is_u64())
        .map(|c| (c[0].as_u64().unwrap_or(0) as u32, c[1].as_u64().unwrap_or(0) as u16))
        .collect();
    if unlock.len() != 3 || cmds.len() != 4 {
        return Err("only the 4-cycle AMD single write (3 unlock commands, then the byte) is supported".into());
    }
    for (i, &byte) in data.iter().enumerate() {
        if byte == 0xFF {
            continue;
        }
        let at = address + i as u32;
        let mut seq = unlock.clone();
        seq.push((at, byte as u16));
        bus.write_cmds(&seq)?;
        let mut polls = 0;
        loop {
            let r = bus.read(at, 1)?;
            let b = *r.first().ok_or("short read while polling")?;
            if b & 0x80 == byte & 0x80 {
                break; // DQ7 shows the data bit: the byte is programmed
            }
            polls += 1;
            if polls >= max_polls {
                return Err(format!("programming 0x{at:X} timed out (read {b:02X}, wrote {byte:02X})"));
            }
        }
    }
    Ok(())
}

/// The firmware parameters for a profile (single write, AMD, WR).
pub fn programmer(p: &Profile) -> Result<FlashProgrammer, String> {
    let cmds = p.raw["commands"]["single_write"].as_array().ok_or("no single_write")?;
    let parsed: Vec<(u32, u16)> = cmds
        .iter()
        .take(6)
        .map(|c| (c[0].as_u64().unwrap_or(0) as u32, c[1].as_u64().unwrap_or(0) as u16)) // PA / PD tokens -> 0
        .collect();
    Ok(FlashProgrammer {
        command_set: 1,
        method: 1,
        we_pin: match p.write_pin.as_deref() {
            Some("WR") => 1,
            Some("AUDIO") | Some("VIN") => 2,
            _ => 0,
        },
        cmds: parsed,
        pullup_wr: p.raw.get("enable_pullup_wr").and_then(|v| v.as_bool()).unwrap_or(false),
    })
}

fn select_bank<T: Transport>(dev: &mut GbxCart<T>, bank: usize) -> Result<(), String> {
    let e = |x: zerocart_core::Error| x.to_string();
    for (a, v) in DmgMapper::Mbc5.select_rom_bank(bank).0 {
        dev.cart_write(a, v).map_err(e)?;
    }
    if needs_separator(bank) {
        dev.cart_write(0x4000, 0).map_err(e)?; // MBC5 "ROM write bank switch separator"
    }
    dev.set_var(zerocart_core::device::var::DMG_ROM_BANK, bank as u32).map_err(e)
}

/// Sets the voltage (the profile's, else the other one), verifies the chip answers as `p` expects at that voltage,
/// and loads the programming parameters into the reader. Nothing is written to the chip before the match.
pub fn prepare_chip<T: Transport>(dev: &mut GbxCart<T>, p: &Profile, progress: &mut dyn FnMut(&str)) -> Result<(), String> {
    let first = p.voltage == 3.3;
    let mut found = false;
    for v33 in [first, !first] {
        dev.set_voltage(v33).map_err(|e| e.to_string())?;
        progress(&format!("verifying the flash chip at {} V", if v33 { "3.3" } else { "5" }));
        if detect(dev, std::slice::from_ref(p), "DMG")?.matches == [0] {
            found = true;
            break;
        }
    }
    if !found {
        return Err("the chip did not answer as this profile expects at either voltage: nothing was erased".into());
    }
    dev.configure_flash_programmer(&programmer(p)?).map_err(|e| e.to_string())
}

/// Reads `plan`'s range back from the cartridge.
fn read_range<T: Transport>(dev: &mut GbxCart<T>, plan: &Plan) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    for seg in &plan.segments {
        select_bank(dev, seg.bank)?;
        out.extend(dev.read_rom(0x4000 + seg.start as u32, seg.len).map_err(|x| x.to_string())?);
    }
    Ok(out)
}

/// Positions where the cartridge differs from `data`.
fn diffs<T: Transport>(dev: &mut GbxCart<T>, plan: &Plan, data: &[u8]) -> Result<Vec<usize>, String> {
    let have = read_range(dev, plan)?;
    Ok(have.iter().zip(data).enumerate().filter(|(_, (h, w))| h != w).map(|(i, _)| i).collect())
}

/// Erases and reprograms one sector with `data` (exactly the sector's size), verifying afterwards: bytes that only
/// need bits cleared are repaired in place, anything else repeats the erase (3 attempts). The chip must have been
/// prepared with [`prepare_chip`]. Leaves the chip in read mode.
pub fn rewrite_one<T: Transport>(
    dev: &mut GbxCart<T>,
    p: &Profile,
    offset: u64,
    data: &[u8],
    progress: &mut dyn FnMut(&str),
) -> Result<(), String> {
    let plan = plan_rewrite(p, offset, data.len())?;
    let e = |x: zerocart_core::Error| x.to_string();
    for attempt in 1..=MAX_ATTEMPTS {
        select_bank(dev, plan.segments[0].bank)?;
        progress(&format!("erasing 0x{offset:X} (attempt {attempt})"));
        erase_sector(dev, p, plan.sector_address, Duration::from_millis(50), 100)?;
        for seg in &plan.segments {
            select_bank(dev, seg.bank)?;
            dev.flash_program(0x4000 + seg.start as u32, &data[seg.data_at..seg.data_at + seg.len], 0x400).map_err(e)?;
        }
        dev.write_cmds(&p.reset)?;

        // Verify; repair in place what only needs bits cleared (a flash chip can do nothing else without an erase).
        for round in 0..3 {
            let bad = diffs(dev, &plan, data)?;
            if bad.is_empty() {
                return Ok(());
            }
            progress(&format!("{} byte(s) differ after programming (first at 0x{:X})", bad.len(), offset + bad[0] as u64));
            let have = read_range(dev, &plan)?;
            if round == 2 || bad.iter().any(|&i| data[i] & !have[i] != 0) {
                break; // needs an erase: next attempt
            }
            for (at, len) in runs(&bad) {
                let seg = plan.segments.iter().find(|s| at >= s.data_at && at < s.data_at + s.len).ok_or("position outside the plan")?;
                let len = len.min(seg.data_at + seg.len - at); // a run never crosses into the next bank here
                select_bank(dev, seg.bank)?;
                dev.flash_program(0x4000 + (seg.start + at - seg.data_at) as u32, &data[at..at + len], len).map_err(e)?;
            }
            dev.write_cmds(&p.reset)?;
        }
    }
    Err(format!("sector 0x{offset:X} still differs after {MAX_ATTEMPTS} attempts"))
}

const MAX_ATTEMPTS: usize = 3;

/// Runs `read` until two reads agree (at most 3), because a read can very occasionally return garbage for a few
/// kilobytes. Returns the agreed data, or an error when no two of three reads are identical.
pub fn read_until_stable(mut read: impl FnMut() -> Result<Vec<u8>, String>) -> Result<Vec<u8>, String> {
    let a = read()?;
    let b = read()?;
    if a == b {
        return Ok(a);
    }
    let c = read()?;
    if c == a {
        return Ok(a);
    }
    if c == b {
        return Ok(b);
    }
    Err("three reads of the cartridge all differ: check the contacts".into())
}

/// Contiguous runs `(start, length)` in a sorted list of positions.
pub fn runs(sorted: &[usize]) -> Vec<(usize, usize)> {
    let mut out: Vec<(usize, usize)> = Vec::new();
    for &i in sorted {
        match out.last_mut() {
            Some(r) if r.0 + r.1 == i => r.1 += 1,
            _ => out.push((i, 1)),
        }
    }
    out
}

/// Erases and reprograms one sector, preparing the chip first. See [`rewrite_one`].
pub fn rewrite_sector<T: Transport>(
    dev: &mut GbxCart<T>,
    p: &Profile,
    offset: u64,
    data: &[u8],
    progress: &mut dyn FnMut(&str),
) -> Result<(), String> {
    plan_rewrite(p, offset, data.len())?;
    prepare_chip(dev, p, progress)?;
    rewrite_one(dev, p, offset, data, progress)
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct FlashReport {
    pub rewritten: usize,
    pub unchanged: usize,
}

/// Programs `rom` at offset 0. Only the sectors whose content differs are erased and rewritten; the last one is
/// padded with 0xFF. Sectors beyond the end of `rom` are left as they are. `progress(done, total, message)`.
/// Returns false from `cancel` to stop between sectors.
pub fn flash_rom<T: Transport>(
    dev: &mut GbxCart<T>,
    p: &Profile,
    sectors: &[(u64, u32)],
    rom: &[u8],
    progress: &mut dyn FnMut(usize, usize, &str),
    cancel: &dyn Fn() -> bool,
) -> Result<FlashReport, String> {
    let chip: u64 = sectors.last().map_or(0, |s| s.0 + s.1 as u64);
    if rom.is_empty() || rom.len() as u64 > chip {
        return Err(format!("the ROM ({} bytes) does not fit the chip ({chip} bytes)", rom.len()));
    }
    let used: Vec<&(u64, u32)> = sectors.iter().filter(|s| s.0 < rom.len() as u64).collect();
    prepare_chip(dev, p, &mut |m| progress(0, used.len(), m))?;
    let mut report = FlashReport::default();
    for (i, &&(off, size)) in used.iter().enumerate() {
        if cancel() {
            return Err("cancelled".into());
        }
        let mut target = rom[off as usize..(off as usize + size as usize).min(rom.len())].to_vec();
        target.resize(size as usize, 0xFF);
        let plan = plan_rewrite(p, off, size as usize)?;
        let same = diffs(dev, &plan, &target)?.is_empty();
        progress(i, used.len(), &format!("sector {}/{} at 0x{:X}{}", i + 1, used.len(), off, if same { " (unchanged)" } else { "" }));
        if same {
            report.unchanged += 1;
            continue;
        }
        rewrite_one(dev, p, off, &target, &mut |m| progress(i, used.len(), m))?;
        report.rewritten += 1;
    }
    progress(used.len(), used.len(), "done");
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::profile::parse;

    fn profile() -> Profile {
        parse("fc_t.txt", &std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../configs/fc_DMG_M29W320DT_2.txt")).unwrap()).unwrap()
    }

    #[test]
    fn erase_plan_substitutes_the_sector_and_waits_on_the_last_command() {
        let plan = erase_plan(&profile(), 0x4000).unwrap();
        assert_eq!(plan.len(), 6);
        assert_eq!(plan[0].write, Some((0xAAA, 0xAA)));
        assert_eq!(plan[5].write, Some((0x4000, 0x30)));
        assert!(plan[..5].iter().all(|s| s.wait.is_none()));
        assert_eq!(plan[5].wait, Some((0x4000, 0xFF, 0xFF)));
    }

    #[test]
    fn plan_for_the_first_sector_uses_bank_0_at_0x4000() {
        let plan = plan_rewrite(&profile(), 0, 0x10000).unwrap();
        assert_eq!((plan.segments.len(), plan.sector_address), (4, 0x4000));
        assert_eq!(plan.segments[0], Segment { bank: 0, start: 0, len: 0x4000, data_at: 0 });
        let plan = plan_rewrite(&profile(), 0x10000, 0x10000).unwrap();
        assert_eq!((plan.segments[0].bank, plan.segments[3].bank, plan.sector_address), (4, 7, 0x4000));
    }

    #[test]
    fn an_8_kib_sector_is_half_a_bank() {
        let plan = plan_rewrite(&profile(), 0x3FE000, 0x2000).unwrap();
        assert_eq!(plan.segments, vec![Segment { bank: 255, start: 0x2000, len: 0x2000, data_at: 0 }]);
        assert_eq!(plan.sector_address, 0x4000 + 0x2000, "erase address is inside the window");
        let plan = plan_rewrite(&profile(), 0x3FC000, 0x2000).unwrap();
        assert_eq!((plan.segments[0].bank, plan.segments[0].start), (255, 0));
    }

    #[test]
    fn the_bank_that_collides_with_a_command_gets_a_separator() {
        assert!(needs_separator(0xAA) && needs_separator(0x1AA));
        assert!(!needs_separator(0xA9) && !needs_separator(0));
        // sector 42 holds banks 168..=171 and is planned like any other
        assert!(plan_rewrite(&profile(), 42 * 0x10000, 0x10000).is_ok());
    }

    #[test]
    fn unsupported_requests_are_refused() {
        assert!(plan_rewrite(&profile(), 1, 0x10000).is_err(), "unaligned");
        assert!(plan_rewrite(&profile(), 0, 100).is_err(), "not whole 8 KiB units");
        let mut other = profile();
        other.command_set = Some("INTEL".into());
        assert!(plan_rewrite(&other, 0, 0x10000).is_err());
    }

    #[test]
    fn two_matching_reads_are_enough() {
        let mut n = 0;
        let r = read_until_stable(|| { n += 1; Ok(vec![1, 2, 3]) }).unwrap();
        assert_eq!((r, n), (vec![1, 2, 3], 2));
    }

    #[test]
    fn a_single_glitched_read_is_outvoted() {
        let reads = [vec![9, 9, 9], vec![1, 2, 3], vec![1, 2, 3]];
        let mut i = 0;
        let r = read_until_stable(|| { i += 1; Ok(reads[i - 1].clone()) }).unwrap();
        assert_eq!(r, vec![1, 2, 3]);
        let reads = [vec![1, 2, 3], vec![9, 9, 9], vec![1, 2, 3]];
        let mut i = 0;
        assert_eq!(read_until_stable(|| { i += 1; Ok(reads[i - 1].clone()) }).unwrap(), vec![1, 2, 3]);
    }

    #[test]
    fn three_different_reads_are_an_error() {
        let reads = [vec![1], vec![2], vec![3]];
        let mut i = 0;
        assert!(read_until_stable(|| { i += 1; Ok(reads[i - 1].clone()) }).is_err());
    }

    /// A chip that is busy for a few reads after each program cycle and keeps what is programmed (bits only clear).
    struct ByteChip {
        mem: std::collections::HashMap<u32, u8>,
        seq: Vec<(u32, u16)>,
        busy: u32,
        last: Option<u32>,
        writes: usize,
    }
    impl FlashBus for ByteChip {
        fn write_cmds(&mut self, cmds: &[(u32, u16)]) -> Result<(), String> {
            for &c in cmds {
                self.writes += 1;
                self.seq.push(c);
                if self.seq.len() == 4 && self.seq[..3] == [(0xAAA, 0xAA), (0x555, 0x55), (0xAAA, 0xA0)] {
                    let (a, v) = self.seq[3];
                    let old = *self.mem.get(&a).unwrap_or(&0xFF);
                    self.mem.insert(a, old & v as u8);
                    self.last = Some(a);
                    self.busy = 2;
                    self.seq.clear();
                } else if self.seq.len() >= 4 {
                    self.seq.clear();
                }
            }
            Ok(())
        }
        fn read(&mut self, a: u32, _len: usize) -> Result<Vec<u8>, String> {
            let v = *self.mem.get(&a).unwrap_or(&0xFF);
            if self.busy > 0 && self.last == Some(a) {
                self.busy -= 1;
                return Ok(vec![!v]); // DQ7 inverted while busy
            }
            Ok(vec![v])
        }
        fn set_we_pin(&mut self, _: u8) -> Result<(), String> {
            Ok(())
        }
    }

    #[test]
    fn single_byte_programming_unlocks_each_byte_polls_and_skips_erased_bytes() {
        let mut chip = ByteChip { mem: Default::default(), seq: vec![], busy: 0, last: None, writes: 0 };
        program_single(&mut chip, &profile(), 0x4000, &[0x12, 0xFF, 0x00, 0xFF, 0xA5], 10).unwrap();
        assert_eq!(chip.mem.get(&0x4000), Some(&0x12));
        assert_eq!(chip.mem.get(&0x4001), None, "0xFF is not programmed");
        assert_eq!(chip.mem.get(&0x4002), Some(&0x00));
        assert_eq!(chip.mem.get(&0x4004), Some(&0xA5));
        assert_eq!(chip.writes, 3 * 4, "four bus writes for each of the three programmed bytes");
    }

    #[test]
    fn a_byte_the_chip_never_finishes_times_out() {
        struct Stuck;
        impl FlashBus for Stuck {
            fn write_cmds(&mut self, _: &[(u32, u16)]) -> Result<(), String> { Ok(()) }
            fn read(&mut self, _: u32, _: usize) -> Result<Vec<u8>, String> { Ok(vec![0x7F]) }
            fn set_we_pin(&mut self, _: u8) -> Result<(), String> { Ok(()) }
        }
        let r = program_single(&mut Stuck, &profile(), 0x4000, &[0x80], 5);
        assert!(r.unwrap_err().contains("timed out"));
    }

    #[test]
    fn runs_groups_adjacent_positions() {
        assert_eq!(runs(&[3, 4, 5, 9, 11, 12]), vec![(3, 3), (9, 1), (11, 2)]);
        assert!(runs(&[]).is_empty());
    }

    #[test]
    fn programmer_parameters_come_from_single_write() {
        let p = programmer(&profile()).unwrap();
        assert_eq!((p.command_set, p.method, p.we_pin), (1, 1, 1));
        assert_eq!(p.cmds, vec![(0xAAA, 0xAA), (0x555, 0x55), (0xAAA, 0xA0), (0, 0)]);
        assert!(p.pullup_wr);
    }

    /// A chip whose sector erase finishes after a few polls.
    struct Chip {
        writes: Vec<(u32, u16)>,
        busy_polls: u32,
    }
    impl FlashBus for Chip {
        fn write_cmds(&mut self, cmds: &[(u32, u16)]) -> Result<(), String> {
            self.writes.extend_from_slice(cmds);
            Ok(())
        }
        fn read(&mut self, _a: u32, len: usize) -> Result<Vec<u8>, String> {
            if self.busy_polls > 0 {
                self.busy_polls -= 1;
                Ok(vec![0x00; len])
            } else {
                Ok(vec![0xFF; len])
            }
        }
        fn set_we_pin(&mut self, _: u8) -> Result<(), String> {
            Ok(())
        }
    }

    #[test]
    fn erase_writes_the_sequence_waits_and_resets() {
        let mut chip = Chip { writes: vec![], busy_polls: 4 };
        erase_sector(&mut chip, &profile(), 0x4000, Duration::ZERO, 100).unwrap();
        assert_eq!(chip.writes.first(), Some(&(0, 0xF0)), "reset first");
        assert!(chip.writes.contains(&(0x4000, 0x30)));
        assert_eq!(chip.writes.last(), Some(&(0, 0xF0)), "reset last");
        assert_eq!(chip.busy_polls, 0, "polled until ready");
    }

    #[test]
    fn a_chip_that_never_finishes_times_out_without_reset_hiding_it() {
        let mut chip = Chip { writes: vec![], busy_polls: u32::MAX };
        let r = erase_sector(&mut chip, &profile(), 0x4000, Duration::ZERO, 5);
        assert!(r.unwrap_err().contains("timed out"));
    }
}
