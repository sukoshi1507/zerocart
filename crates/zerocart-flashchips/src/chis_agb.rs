// SPDX-License-Identifier: GPL-3.0-or-later
//! GBA flash cartridges in the Chisflash reader (original firmware): identification, chip erase and programming.
//!
//! Everything that depends on the chip (identifier, CFI, erase) is the profile's command list, sent as raw 16-bit
//! writes on the ROM bus (command 0xF5, word addresses: the profile's byte addresses are halved, as the GBxCart
//! path does). Only the programming uses the reader's own command (0xF4, much faster than raw writes), without a
//! write buffer. The firmware's programming (`romProgram` in `uart.c` of beggar_socket) has the AMD sequence fixed:
//! `AA` at word 0x555, `55` at word 0x2AA, `A0` at word 0x555, then the word, polled on DQ7; a profile whose
//! single-write sequence is another one (cartridges with crossed data lines, `A9`/`56`) is programmed word by word
//! with the profile's own sequence sent as raw writes (4 frames a word, about 4.5 ms: 75 minutes for 2 MiB). The result is always read back: if the first block is not programmed as asked, the transfer stops
//! before anything else is written. The caller makes the backup of the chip beforehand.
use crate::agb::{chip_erase, AgbReport};
use crate::detect::FlashBus;
use crate::profile::Profile;
use crate::program::runs;
use std::time::Duration;
use zerocart_core::chis::{Chis, CHUNK};
use zerocart_core::Transport;

/// The GBA cartridge bus of a Chisflash, as the profiles see it (byte addresses, 16-bit commands).
pub struct AgbBus<'a, T: Transport>(pub &'a mut Chis<T>);

impl<T: Transport> FlashBus for AgbBus<'_, T> {
    fn write_cmds(&mut self, cmds: &[(u32, u16)]) -> Result<(), String> {
        for &(a, v) in cmds {
            self.0.write_rom_words(a >> 1, &v.to_le_bytes()).map_err(|e| e.to_string())?;
        }
        Ok(())
    }
    fn read(&mut self, address: u32, len: usize) -> Result<Vec<u8>, String> {
        // The firmware reads whole words from an even address.
        let start = address & !1;
        let end = (address as usize + len).next_multiple_of(2);
        let mut v = self.0.read_rom(start, end - start as usize).map_err(|e| e.to_string())?;
        v.drain(..(address - start) as usize);
        v.truncate(len);
        Ok(v)
    }
    fn set_we_pin(&mut self, _: u8) -> Result<(), String> {
        Ok(())
    }
}

/// The single-write sequence the reader's programming command sends (byte addresses of the profiles).
const FIRMWARE_PROGRAM_SEQUENCE: [(u32, u16); 3] = [(0xAAA, 0xAA), (0x555, 0x55), (0xAAA, 0xA0)];

/// Whether the reader's programming command suits this profile.
pub fn can_program(p: &Profile) -> Result<(), String> {
    let seq = crate::program::programmer(p)?;
    if seq.cmds.get(..3) == Some(&FIRMWARE_PROGRAM_SEQUENCE[..]) {
        Ok(())
    } else {
        Err("this chip needs another programming sequence than the one the Chisflash firmware sends".into())
    }
}

/// Reads `len` bytes of ROM from 0, reporting `progress(done)`.
pub fn read_all<T: Transport>(r: &mut Chis<T>, len: usize, progress: &mut dyn FnMut(usize)) -> Result<Vec<u8>, String> {
    let mut out = Vec::with_capacity(len);
    while out.len() < len {
        let n = CHUNK.min(len - out.len());
        out.extend(r.read_rom(out.len() as u32, n).map_err(|e| e.to_string())?);
        progress(out.len());
    }
    Ok(out)
}

/// Whether `p` is programmed with the reader's own command (fast) or word by word (slow).
pub fn is_fast(p: &Profile) -> bool {
    can_program(p).is_ok()
}

/// Programs `data` at `at` (even): with the reader's command, 4 KiB at a time, when the profile suits it, else one
/// word at a time with the profile's own sequence. Blocks / words that are all 0xFF are skipped (the chip is
/// erased). The pause between two raw frames (a USB round trip, about 1 ms) is longer than a word programming, so
/// the chip is not polled: the read back catches anything that did not program. On an error the reader is
/// resynchronised and the chip sent back to read mode.
fn program<T: Transport>(r: &mut Chis<T>, p: &Profile, at: usize, data: &[u8]) -> Result<(), String> {
    let fail = |r: &mut Chis<T>, a: usize, e: String| {
        let _ = r.resync();
        let _ = AgbBus(r).write_cmds(&p.reset);
        Err(format!("programming at 0x{a:X}: {e}"))
    };
    if is_fast(p) {
        for (i, chunk) in data.chunks(CHUNK).enumerate() {
            if chunk.iter().all(|&b| b == 0xFF) {
                continue;
            }
            let a = at + i * CHUNK;
            if let Err(e) = r.program_rom(a as u32, 0, chunk) {
                return fail(r, a, e.to_string());
            }
        }
        return Ok(());
    }
    let seq = crate::program::programmer(p)?.cmds;
    for (i, w) in data.chunks(2).enumerate() {
        if w == [0xFF, 0xFF] {
            continue;
        }
        let a = at + i * 2;
        let mut bus = AgbBus(r);
        let sent = bus.write_cmds(&seq[..3]).and_then(|_| bus.0.write_rom_words((a >> 1) as u32, w).map_err(|e| e.to_string()));
        if let Err(e) = sent {
            return fail(r, a, e);
        }
    }
    Ok(())
}

/// Erases the whole chip, programs `rom` at offset 0 and verifies, like [`crate::agb::flash_rom`]: bytes that
/// differ and only need bits cleared are repaired in place, anything else repeats the erase (2 attempts at most).
/// `progress(done, total, message)` over the ROM bytes.
pub fn flash_rom<T: Transport>(
    r: &mut Chis<T>,
    p: &Profile,
    rom: &[u8],
    progress: &mut dyn FnMut(usize, usize, &str),
) -> Result<AgbReport, String> {
    if rom.is_empty() || !rom.len().is_multiple_of(2) {
        return Err("a GBA ROM has an even, non-zero size".into());
    }
    let total = rom.len();
    let mut repaired = 0;
    for attempt in 1..=2 {
        progress(0, total, &format!("erasing the chip (attempt {attempt})"));
        chip_erase(&mut AgbBus(r), p, Duration::from_millis(100), Duration::from_millis(500), 600)?;
        let mut pos = 0;
        while pos < total {
            let n = if is_fast(p) { 0x10000 } else { 0x2000 }.min(total - pos);
            program(r, p, pos, &rom[pos..pos + n])?;
            if pos == 0 {
                // First block: stop here if the reader did not program what was asked.
                AgbBus(r).write_cmds(&p.reset)?;
                let first = CHUNK.min(total);
                if r.read_rom(0, first).map_err(|e| e.to_string())? != rom[..first] {
                    return Err("the first block did not program as asked: nothing more was written".into());
                }
            }
            pos += n;
            progress(pos, total, "programming");
        }
        AgbBus(r).write_cmds(&p.reset)?;
        for _ in 0..3 {
            progress(total, total, "verifying");
            let have = read_all(r, total, &mut |_| {})?;
            let bad: Vec<usize> = have.iter().zip(rom).enumerate().filter(|(_, (h, w))| h != w).map(|(i, _)| i).collect();
            if bad.is_empty() {
                return Ok(AgbReport { attempts: attempt, repaired });
            }
            progress(total, total, &format!("{} byte(s) differ (first at 0x{:X})", bad.len(), bad[0]));
            if bad.iter().any(|&i| rom[i] & !have[i] != 0) {
                break; // needs an erase
            }
            for (at, len) in runs(&bad) {
                // Whole words: widen to even bounds (the bytes around are already equal to the ROM).
                let (s, e) = (at & !1, (at + len).next_multiple_of(2));
                program(r, p, s, &rom[s..e])?;
                repaired += len;
            }
            AgbBus(r).write_cmds(&p.reset)?;
        }
    }
    Err("the cartridge still differs from the ROM after 2 attempts".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;
    use zerocart_core::chis::{cmd, crc16, ACK};
    use zerocart_core::Result;

    /// A Chisflash with an AMD flash GBA cartridge: parses frames, keeps a simple chip state machine (unlock,
    /// chip erase, "program" only clears bits like a real flash).
    struct Sim {
        rom: Vec<u8>,
        pending: Vec<u8>,
        out: VecDeque<u8>,
        seq: Vec<(u32, u16)>,
        program_calls: usize,
        /// The two unlock values the chip understands (`AA`/`55`, or `A9`/`56` with crossed data lines).
        unlock: (u16, u16),
        armed: bool,
        raw_programs: usize,
        /// Programming commands that do nothing (a firmware whose sequence does not suit the chip).
        deaf: bool,
    }

    impl Sim {
        fn new(len: usize) -> Self {
            Self { rom: (0..len).map(|i| (i * 13 + 1) as u8).collect(), pending: vec![], out: VecDeque::new(), seq: vec![], program_calls: 0, unlock: (0xAA, 0x55), armed: false, raw_programs: 0, deaf: false }
        }
        fn handle(&mut self, f: Vec<u8>) {
            assert_eq!(crc16(&f[..f.len() - 2]).to_le_bytes(), [f[f.len() - 2], f[f.len() - 1]]);
            let body = &f[3..f.len() - 2];
            let addr = u32::from_le_bytes(body[..4].try_into().unwrap()) as usize;
            match f[2] {
                cmd::ROM_READ => {
                    let n = u16::from_le_bytes([body[4], body[5]]) as usize;
                    assert!(addr % 2 == 0 && n % 2 == 0, "word reads only");
                    self.out.extend([0, 0]);
                    self.out.extend(self.rom[addr..addr + n].to_vec());
                }
                cmd::ROM_WRITE => {
                    let v = u16::from_le_bytes([body[4], body[5]]);
                    let (u1, u2) = self.unlock;
                    if self.armed {
                        self.armed = false;
                        self.rom[addr * 2] &= v as u8;
                        self.rom[addr * 2 + 1] &= (v >> 8) as u8;
                        self.raw_programs += 1;
                    } else {
                        self.seq.push((addr as u32 * 2, v));
                        if self.seq.ends_with(&[(0xAAA, u1), (0x554, u2), (0xAAA, 0xA0)]) {
                            self.armed = true;
                        }
                        if self.seq.ends_with(&[(0xAAA, u1), (0x554, u2), (0xAAA, 0x80), (0xAAA, u1), (0x554, u2), (0xAAA, 0x10)]) {
                            self.rom.fill(0xFF);
                        }
                    }
                    self.out.push_back(ACK);
                }
                cmd::ROM_PROGRAM => {
                    self.program_calls += 1;
                    let data = &body[6..];
                    if !self.deaf {
                        for (i, b) in data.iter().enumerate() {
                            self.rom[addr + i] &= b;
                        }
                    }
                    self.out.push_back(ACK);
                }
                c => panic!("unexpected command {c:#x}"),
            }
        }
    }

    impl Transport for Sim {
        fn write_all(&mut self, data: &[u8]) -> Result<()> {
            self.pending.extend_from_slice(data);
            while self.pending.len() > 2 && u16::from_le_bytes([self.pending[0], self.pending[1]]) as usize == self.pending.len() {
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
        fn reset_device(&mut self) -> Result<()> { self.pending.clear(); Ok(()) }
    }

    fn profile() -> Profile {
        crate::profile::parse("fc_agb.txt", &std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../configs/fc_AGB_Generic_AAA_AA.txt")).unwrap()).unwrap()
    }

    #[test]
    fn profile_commands_become_word_writes_at_half_the_byte_address() {
        let mut r = Chis::new(Sim::new(64)).unwrap();
        AgbBus(&mut r).write_cmds(&[(0xAAA, 0xAA), (0x555, 0x55)]).unwrap();
        assert_eq!(r.into_inner().seq, vec![(0xAAA, 0xAA), (0x554, 0x55)]);
    }

    #[test]
    fn a_chip_with_crossed_data_lines_is_programmed_word_by_word_with_its_own_sequence() {
        let a9 = crate::profile::parse("fc_agb.txt", &std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../configs/fc_AGB_Generic_AAA_A9.txt")).unwrap()).unwrap();
        assert!(!is_fast(&a9) && is_fast(&profile()));
        let mut sim = Sim::new(0x1000);
        sim.unlock = (0xA9, 0x56);
        let mut r = Chis::new(sim).unwrap();
        let mut rom: Vec<u8> = (0..0x800).map(|i| (i * 7 + 2) as u8).collect();
        rom[0x100..0x200].fill(0xFF); // blank words are not written
        let rep = flash_rom(&mut r, &a9, &rom, &mut |_, _, _| {}).unwrap();
        assert_eq!((rep.attempts, rep.repaired), (1, 0));
        let sim = r.into_inner();
        assert_eq!(&sim.rom[..rom.len()], &rom[..]);
        assert_eq!(sim.program_calls, 0, "the reader's own command was not used");
        assert_eq!(sim.raw_programs, 0x400 - 0x80);
    }

    #[test]
    fn an_odd_read_is_cut_out_of_a_word_aligned_one() {
        let sim = Sim::new(64);
        let want = sim.rom[3..8].to_vec();
        let mut r = Chis::new(sim).unwrap();
        assert_eq!(AgbBus(&mut r).read(3, 5).unwrap(), want);
    }

    #[test]
    fn a_rom_is_erased_programmed_and_read_back() {
        let mut r = Chis::new(Sim::new(0x3000)).unwrap();
        let mut rom: Vec<u8> = (0..0x2800).map(|i| (i * 5 + 3) as u8).collect();
        rom[0x1000..0x2000].fill(0xFF); // a blank block is not sent
        let rep = flash_rom(&mut r, &profile(), &rom, &mut |_, _, _| {}).unwrap();
        assert_eq!((rep.attempts, rep.repaired), (1, 0));
        let sim = r.into_inner();
        assert_eq!(&sim.rom[..rom.len()], &rom[..]);
        assert_eq!(sim.program_calls, 2, "0x0000-0x1000 and the two blocks after the blank one, in 64 KiB passes");
    }

    #[test]
    fn a_reader_that_does_not_program_stops_after_the_first_block() {
        let mut sim = Sim::new(0x20000);
        sim.deaf = true;
        let mut r = Chis::new(sim).unwrap();
        let rom = vec![0x12u8; 0x20000];
        let err = flash_rom(&mut r, &profile(), &rom, &mut |_, _, _| {}).unwrap_err();
        assert!(err.contains("first block"), "{err}");
        assert_eq!(r.into_inner().program_calls, 16, "only the first 64 KiB pass was sent");
    }
}
