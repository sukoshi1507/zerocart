//! Serial flash mode: one ROM written on cartridge after cartridge, with as little to do as possible for the operator.
//!
//! The loop is: *insert* (ding) -> the chip answers -> *flashing* (ding ding, erase, write, verify) -> *eject*
//! (fanfare, or the error buzz) -> the connector is seen empty -> *insert* again. Nothing here touches the reader or
//! the speaker: the state machine answers with [`Out`] (commands for the worker, jingles to play), so it is tested
//! without hardware.
use crate::input::Key;
use crate::jingle::Jingle;
use crate::worker::Cmd;
use std::path::PathBuf;
use std::time::{Duration, Instant};
pub use zerocart_ui::batch_screen::BatchPhase as Phase;

/// How often the connector is looked at while waiting for a cartridge (each look identifies the flash chip).
const PROBE_EVERY: Duration = Duration::from_millis(900);

#[derive(Debug)]
pub enum Out {
    Send(Cmd),
    Play(Jingle),
}

pub struct Batch {
    pub phase: Phase,
    /// Game Boy mode (5 V) rather than Game Boy Advance (3.3 V)
    dmg: bool,
    pub files: Vec<PathBuf>,
    pub selected: usize,
    pub file: Option<PathBuf>,
    pub ok: u32,
    pub failed: u32,
    pub last_secs: Option<u64>,
    /// A problem or the outcome of the last cartridge
    pub message: Option<(String, bool)>,
    probing: bool,
    last_probe: Option<Instant>,
    flash_started: Option<Instant>,
}

impl Batch {
    pub fn new(dmg: bool, files: Vec<PathBuf>) -> Self {
        Self {
            phase: Phase::Pick, dmg, files, selected: 0, file: None, ok: 0, failed: 0, last_secs: None, message: None,
            probing: false, last_probe: None, flash_started: None,
        }
    }

    /// The reader answers to a cartridge being flashed: the keys wait.
    pub fn flashing(&self) -> bool {
        self.phase == Phase::Flashing
    }

    fn start_waiting(&mut self) -> Vec<Out> {
        self.phase = Phase::Insert;
        self.message = None;
        self.last_probe = None;
        vec![Out::Play(Jingle::Insert)]
    }

    /// A key of the file picker, or Enter on the eject screen ("next", when the empty connector went unnoticed).
    pub fn key(&mut self, key: Key) -> Vec<Out> {
        match (self.phase, key) {
            (Phase::Pick, Key::Up) if !self.files.is_empty() => {
                self.selected = (self.selected + self.files.len() - 1) % self.files.len();
                vec![]
            }
            (Phase::Pick, Key::Down) if !self.files.is_empty() => {
                self.selected = (self.selected + 1) % self.files.len();
                vec![]
            }
            (Phase::Pick, Key::Enter) if !self.files.is_empty() => {
                self.file = Some(self.files[self.selected].clone());
                self.start_waiting()
            }
            (Phase::Eject { .. }, Key::Enter) => self.start_waiting(),
            _ => vec![],
        }
    }

    /// Called every loop turn: asks the worker whether a cartridge is in, at a steady pace.
    pub fn tick(&mut self, now: Instant) -> Vec<Out> {
        if !matches!(self.phase, Phase::Insert | Phase::Eject { .. }) || self.probing {
            return vec![];
        }
        if self.last_probe.is_some_and(|t| now.duration_since(t) < PROBE_EVERY) {
            return vec![];
        }
        self.probing = true;
        self.last_probe = Some(now);
        vec![Out::Send(Cmd::BatchProbe { dmg: self.dmg })]
    }

    pub fn probe_result(&mut self, r: Result<bool, String>, now: Instant) -> Vec<Out> {
        self.probing = false;
        self.last_probe = Some(now);
        match (self.phase, r) {
            (Phase::Insert, Ok(true)) => {
                let Some(path) = self.file.clone() else { return vec![] };
                self.phase = Phase::Flashing;
                self.message = None;
                self.flash_started = Some(now);
                vec![Out::Play(Jingle::Detected), Out::Send(Cmd::BatchFlash { path, dmg: self.dmg })]
            }
            // the same wrong cartridge is reported once, not at every look
            (Phase::Insert, Err(e)) => {
                let again = self.message.as_ref().is_some_and(|(m, _)| *m == e);
                self.message = Some((e, true));
                if again { vec![] } else { vec![Out::Play(Jingle::Error)] }
            }
            (Phase::Insert, Ok(false)) => {
                self.message = None;
                vec![]
            }
            // the previous cartridge is out: ready for the next
            (Phase::Eject { .. }, Ok(false)) => self.start_waiting(),
            _ => vec![],
        }
    }

    pub fn flash_result(&mut self, r: Result<String, String>, now: Instant) -> Vec<Out> {
        self.last_secs = self.flash_started.take().map(|t| now.duration_since(t).as_secs());
        self.probing = false;
        self.last_probe = Some(now);
        match r {
            Ok(m) => {
                self.ok += 1;
                self.phase = Phase::Eject { ok: true };
                self.message = Some((m.trim_start_matches('!').to_string(), false));
                vec![Out::Play(Jingle::Done)]
            }
            Err(e) => {
                self.failed += 1;
                self.phase = Phase::Eject { ok: false };
                self.message = Some((e, true));
                vec![Out::Play(Jingle::Error)]
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn picked(dmg: bool) -> Batch {
        let mut b = Batch::new(dmg, vec!["a.gba".into(), "b.gba".into()]);
        b.key(Key::Down);
        let out = b.key(Key::Enter);
        assert!(matches!(out.as_slice(), [Out::Play(Jingle::Insert)]));
        assert_eq!(b.file.as_deref(), Some(std::path::Path::new("b.gba")));
        b
    }

    fn plays(out: &[Out]) -> Vec<Jingle> {
        out.iter().filter_map(|o| if let Out::Play(j) = o { Some(*j) } else { None }).collect()
    }

    #[test]
    fn nothing_starts_before_a_rom_is_chosen_and_an_empty_list_chooses_nothing() {
        let mut b = Batch::new(false, vec![]);
        assert!(b.key(Key::Enter).is_empty());
        assert_eq!(b.phase, Phase::Pick);
        assert!(b.tick(Instant::now()).is_empty());
    }

    #[test]
    fn the_connector_is_looked_at_a_steady_pace_and_never_twice_at_once() {
        let mut b = picked(false);
        let t0 = Instant::now();
        assert!(matches!(b.tick(t0).as_slice(), [Out::Send(Cmd::BatchProbe { dmg: false })]));
        assert!(b.tick(t0 + Duration::from_secs(5)).is_empty(), "the worker has not answered yet");
        b.probe_result(Ok(false), t0 + Duration::from_millis(300));
        assert!(b.tick(t0 + Duration::from_millis(500)).is_empty(), "too soon");
        assert!(!b.tick(t0 + Duration::from_millis(1300)).is_empty());
    }

    #[test]
    fn a_cartridge_that_answers_is_flashed_at_once_with_two_dings() {
        let mut b = picked(true);
        let t0 = Instant::now();
        b.tick(t0);
        let out = b.probe_result(Ok(true), t0);
        assert_eq!(plays(&out), vec![Jingle::Detected]);
        assert!(matches!(out.last(), Some(Out::Send(Cmd::BatchFlash { dmg: true, .. }))));
        assert!(b.flashing());
        assert!(b.tick(t0 + Duration::from_secs(9)).is_empty(), "no look while writing");
    }

    #[test]
    fn a_whole_loop_counts_the_good_and_the_bad_and_waits_for_the_empty_connector() {
        let mut b = picked(false);
        let t0 = Instant::now();
        b.tick(t0);
        b.probe_result(Ok(true), t0);
        let out = b.flash_result(Ok("written".into()), t0 + Duration::from_secs(61));
        assert_eq!(plays(&out), vec![Jingle::Done]);
        assert_eq!((b.ok, b.failed, b.last_secs), (1, 0, Some(61)));
        assert_eq!(b.phase, Phase::Eject { ok: true });
        // the same cartridge is still in: nothing happens
        let t1 = t0 + Duration::from_secs(63);
        assert!(!b.tick(t1).is_empty());
        assert!(b.probe_result(Ok(true), t1).is_empty());
        assert_eq!(b.phase, Phase::Eject { ok: true });
        // taken out: the next one is asked for, with the ding
        let t2 = t1 + Duration::from_secs(2);
        b.tick(t2);
        assert_eq!(plays(&b.probe_result(Ok(false), t2)), vec![Jingle::Insert]);
        assert_eq!(b.phase, Phase::Insert);
        // second cartridge fails
        b.tick(t2 + Duration::from_secs(2));
        b.probe_result(Ok(true), t2 + Duration::from_secs(2));
        let out = b.flash_result(Err("verify".into()), t2 + Duration::from_secs(40));
        assert_eq!(plays(&out), vec![Jingle::Error]);
        assert_eq!((b.ok, b.failed), (1, 1));
        assert_eq!(b.phase, Phase::Eject { ok: false });
        assert!(b.message.as_ref().is_some_and(|m| m.1));
    }

    #[test]
    fn enter_goes_on_when_the_empty_connector_went_unnoticed() {
        let mut b = picked(false);
        let t0 = Instant::now();
        b.tick(t0);
        b.probe_result(Ok(true), t0);
        b.flash_result(Ok("x".into()), t0);
        assert_eq!(plays(&b.key(Key::Enter)), vec![Jingle::Insert]);
        assert_eq!(b.phase, Phase::Insert);
    }

    #[test]
    fn a_wrong_cartridge_is_reported_once_and_not_flashed() {
        let mut b = picked(true);
        let t0 = Instant::now();
        b.tick(t0);
        assert_eq!(plays(&b.probe_result(Err("GBA".into()), t0)), vec![Jingle::Error]);
        b.tick(t0 + Duration::from_secs(2));
        assert!(b.probe_result(Err("GBA".into()), t0 + Duration::from_secs(2)).is_empty());
        assert_eq!(b.phase, Phase::Insert);
        // it is taken out: the message goes
        b.tick(t0 + Duration::from_secs(4));
        b.probe_result(Ok(false), t0 + Duration::from_secs(4));
        assert!(b.message.is_none());
    }
}
