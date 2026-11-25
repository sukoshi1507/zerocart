// SPDX-License-Identifier: GPL-3.0-or-later
//! Game Boy (MBC5) cartridges in the Chisflash reader (original firmware): reading the ROM and flashing it.
//!
//! The erase is the chip's command sequence sent with plain bus writes (the reader has no erase command for the
//! Game Boy), the programming is the reader's own command (0xFC, about 20 KiB/s, which sends the AMD single-byte
//! sequence AA@AAA 55@555 A0@AAA), and everything is read back. The chip must have been identified before with
//! `detect(r, &profiles, "DMG")`; the caller makes the backup (`program::read_until_stable(|| read_cart(..))`).
use crate::detect::FlashBus;
use crate::profile::Profile;
use crate::program::{erase_sector, needs_separator, plan_rewrite, FlashReport, Plan};
use std::time::Duration;
use zerocart_core::chis::Chis;
use zerocart_core::mapper::{DmgMapper, ROM_BANK_SIZE};
use zerocart_core::Transport;

/// Puts ROM bank `bank` at 0x4000 (MBC5), with the separator write for banks whose number looks like a flash
/// command.
pub fn select_bank<T: Transport>(r: &mut Chis<T>, bank: usize) -> Result<(), String> {
    for (a, v) in DmgMapper::Mbc5.select_rom_bank(bank).0 {
        r.gbc_write(a, &[v]).map_err(|e| format!("selecting bank {bank}: {e}"))?;
    }
    if needs_separator(bank) {
        r.gbc_write(0x4000, &[0]).map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// Reads `size` bytes of ROM, bank by bank (bank 0 at 0x0000, the others at 0x4000). `progress(done)` returns false
/// to cancel.
pub fn read_cart<T: Transport>(r: &mut Chis<T>, size: usize, progress: &mut dyn FnMut(usize) -> bool) -> Result<Vec<u8>, String> {
    let mut out = Vec::with_capacity(size);
    for bank in 0..size.div_ceil(ROM_BANK_SIZE) {
        let (writes, start) = DmgMapper::Mbc5.select_rom_bank(bank);
        for (a, v) in writes {
            r.gbc_write(a, &[v]).map_err(|e| format!("selecting bank {bank}: {e}"))?;
        }
        out.extend(r.gbc_read(start, ROM_BANK_SIZE.min(size - out.len())).map_err(|e| format!("bank {bank}: {e}"))?);
        if !progress(out.len()) {
            return Err("cancelled".into());
        }
    }
    Ok(out)
}

/// The sector of `plan` as the programming sees it (every bank through the 0x4000 window).
pub fn read_sector<T: Transport>(r: &mut Chis<T>, plan: &Plan) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    for seg in &plan.segments {
        select_bank(r, seg.bank)?;
        out.extend(r.gbc_read(0x4000 + seg.start as u32, seg.len).map_err(|e| e.to_string())?);
    }
    Ok(out)
}

/// Programs with the reader's command, 4 KiB at a time, skipping blocks that are entirely 0xFF. The firmware has no
/// time limit of its own: on an error the reader is resynchronised and the chip sent back to read mode before the
/// error is returned.
pub fn program_fast<T: Transport>(r: &mut Chis<T>, p: &Profile, base: u32, data: &[u8]) -> Result<(), String> {
    for (i, chunk) in data.chunks(0x1000).enumerate() {
        if chunk.iter().all(|&b| b == 0xFF) {
            continue;
        }
        let at = base + (i * 0x1000) as u32;
        if let Err(e) = r.gbc_program(at, 0, chunk) {
            let _ = r.resync();
            let _ = r.write_cmds(&p.reset);
            return Err(format!("gbc_program at 0x{at:X}: {e}"));
        }
    }
    Ok(())
}

/// Erases and reprograms one sector with `data` (exactly its size) and verifies it, twice at most.
pub fn rewrite_sector<T: Transport>(
    r: &mut Chis<T>,
    p: &Profile,
    offset: u64,
    data: &[u8],
    progress: &mut dyn FnMut(&str),
) -> Result<(), String> {
    let plan = plan_rewrite(p, offset, data.len())?;
    for attempt in 1..=2 {
        progress(&format!("erasing 0x{offset:X} (attempt {attempt})"));
        select_bank(r, plan.segments[0].bank)?;
        erase_sector(r, p, plan.sector_address, Duration::from_millis(50), 100)?;
        if read_sector(r, &plan)?.iter().any(|&b| b != 0xFF) {
            if attempt == 2 {
                return Err(format!("sector 0x{offset:X} is not blank after the erase"));
            }
            continue;
        }
        for seg in &plan.segments {
            select_bank(r, seg.bank)?;
            program_fast(r, p, 0x4000 + seg.start as u32, &data[seg.data_at..seg.data_at + seg.len])?;
        }
        r.write_cmds(&p.reset)?;
        if read_sector(r, &plan)? == data {
            return Ok(());
        }
        progress(&format!("sector 0x{offset:X} differs after programming"));
    }
    Err(format!("sector 0x{offset:X} still differs after 2 attempts"))
}

/// The sectors that start inside the ROM (the last one may reach past its end).
pub fn used_sectors(sectors: &[(u64, u32)], rom_len: usize) -> Vec<(u64, u32)> {
    sectors.iter().copied().filter(|s| s.0 < rom_len as u64).collect()
}

/// A sector's target content: the ROM bytes, padded with 0xFF up to the sector size.
pub fn sector_target(rom: &[u8], offset: u64, size: u32) -> Vec<u8> {
    let (o, e) = (offset as usize, (offset as usize + size as usize).min(rom.len()));
    let mut t = rom[o..e].to_vec();
    t.resize(size as usize, 0xFF);
    t
}

/// Programs `rom` at offset 0: only the sectors whose content differs are erased and rewritten (the last one is
/// padded with 0xFF), sectors beyond the end of `rom` are left alone. `progress(done, total, message)` counts
/// sectors. `cancel` is checked between sectors only: a sector is never abandoned half-written.
pub fn flash_rom<T: Transport>(
    r: &mut Chis<T>,
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
    let used = used_sectors(sectors, rom.len());
    let mut report = FlashReport::default();
    for (i, &(off, size)) in used.iter().enumerate() {
        if cancel() {
            return Err("cancelled".into());
        }
        let target = sector_target(rom, off, size);
        let plan = plan_rewrite(p, off, size as usize)?;
        let same = read_sector(r, &plan)? == target;
        progress(i, used.len(), &format!("sector {}/{} at 0x{:X}{}", i + 1, used.len(), off, if same { " (unchanged)" } else { "" }));
        if same {
            report.unchanged += 1;
            continue;
        }
        rewrite_sector(r, p, off, &target, &mut |m| progress(i, used.len(), m))?;
        report.rewritten += 1;
    }
    progress(used.len(), used.len(), "done");
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_sectors_that_start_inside_the_rom_are_used() {
        let sectors = [(0u64, 0x10000u32), (0x10000, 0x10000), (0x20000, 0x2000)];
        assert_eq!(used_sectors(&sectors, 0x10000), vec![(0, 0x10000)]);
        assert_eq!(used_sectors(&sectors, 0x10001).len(), 2);
        assert_eq!(used_sectors(&sectors, 0x30000).len(), 3);
    }

    #[test]
    fn a_short_last_sector_is_padded_with_ff() {
        let rom = vec![0x11u8; 0x8000];
        let t = sector_target(&rom, 0, 0x10000);
        assert_eq!(t.len(), 0x10000);
        assert!(t[..0x8000].iter().all(|&b| b == 0x11) && t[0x8000..].iter().all(|&b| b == 0xFF));
        assert_eq!(sector_target(&rom, 0x4000, 0x2000), vec![0x11u8; 0x2000]);
    }
}
