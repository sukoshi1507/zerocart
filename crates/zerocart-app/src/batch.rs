//! Serial flash mode: one ROM written on cartridge after cartridge, the operator pressing `Enter` at each step.
//!
//! The loop is: *insert* (the operator puts a cartridge in and presses Enter) -> *flashing* (the chip is looked for,
//! then erase, write, verify) -> the verdict (fanfare or error buzz, with the reason) -> Enter for the next cartridge
//! (or `R` to try the same one again after a failure). Nothing here touches the reader or the speaker: the state
//! machine answers with [`Out`] (commands for the worker, jingles to play), so it is tested without hardware.
use crate::input::Key;
use crate::jingle::Jingle;
use crate::worker::Cmd;
use std::path::PathBuf;
use std::time::Instant;
pub use zerocart_ui::batch_screen::BatchPhase as Phase;

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
    flash_started: Option<Instant>,
}

impl Batch {
    pub fn new(dmg: bool, files: Vec<PathBuf>) -> Self {
        Self { phase: Phase::Pick, dmg, files, selected: 0, file: None, ok: 0, failed: 0, last_secs: None, message: None, flash_started: None }
    }

    /// The reader answers to a cartridge being flashed: the keys wait.
    pub fn flashing(&self) -> bool {
        self.phase == Phase::Flashing
    }

    fn start_flash(&mut self, now: Instant) -> Vec<Out> {
        let Some(path) = self.file.clone() else { return vec![] };
        self.phase = Phase::Flashing;
        self.message = None;
        self.flash_started = Some(now);
        vec![Out::Play(Jingle::Detected), Out::Send(Cmd::BatchFlash { path, dmg: self.dmg })]
    }

    fn back_to_insert(&mut self) -> Vec<Out> {
        self.phase = Phase::Insert;
        self.message = None;
        vec![]
    }

    /// A key of the file picker, `Enter` to start a cartridge or to go on to the next one, `R` to try a failed one again.
    pub fn key(&mut self, key: Key, now: Instant) -> Vec<Out> {
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
                self.phase = Phase::Insert;
                self.message = None;
                vec![Out::Play(Jingle::Insert)]
            }
            (Phase::Insert, Key::Enter) => self.start_flash(now),
            (Phase::Eject { .. }, Key::Enter) => self.back_to_insert(),
            (Phase::Eject { ok: false }, Key::Char('r' | 'R')) => self.start_flash(now),
            _ => vec![],
        }
    }

    /// The worker did not start the cartridge (no chip answers, wrong system): nothing was touched, try again.
    pub fn skipped(&mut self, why: String) -> Vec<Out> {
        self.phase = Phase::Insert;
        self.flash_started = None;
        self.message = Some((why, true));
        vec![Out::Play(Jingle::Error)]
    }

    pub fn flash_result(&mut self, r: Result<String, String>, now: Instant) -> Vec<Out> {
        self.last_secs = self.flash_started.take().map(|t| now.duration_since(t).as_secs());
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
        let now = Instant::now();
        b.key(Key::Down, now);
        let out = b.key(Key::Enter, now);
        assert!(matches!(out.as_slice(), [Out::Play(Jingle::Insert)]));
        assert_eq!(b.file.as_deref(), Some(std::path::Path::new("b.gba")));
        assert_eq!(b.phase, Phase::Insert);
        b
    }

    fn plays(out: &[Out]) -> Vec<Jingle> {
        out.iter().filter_map(|o| if let Out::Play(j) = o { Some(*j) } else { None }).collect()
    }

    #[test]
    fn nothing_starts_before_a_rom_is_chosen_and_an_empty_list_chooses_nothing() {
        let mut b = Batch::new(false, vec![]);
        assert!(b.key(Key::Enter, Instant::now()).is_empty());
        assert_eq!(b.phase, Phase::Pick);
    }

    #[test]
    fn nothing_starts_by_itself_only_enter_starts_a_cartridge() {
        let mut b = picked(true);
        assert!(b.key(Key::Char('x'), Instant::now()).is_empty());
        assert_eq!(b.phase, Phase::Insert, "the cartridge is waited for, not looked for");
        let out = b.key(Key::Enter, Instant::now());
        assert_eq!(plays(&out), vec![Jingle::Detected]);
        assert!(matches!(out.last(), Some(Out::Send(Cmd::BatchFlash { dmg: true, .. }))));
        assert!(b.flashing());
        assert!(b.key(Key::Enter, Instant::now()).is_empty(), "keys wait while writing");
    }

    #[test]
    fn a_whole_loop_counts_the_good_and_the_bad_and_waits_for_enter() {
        let mut b = picked(false);
        let t0 = Instant::now();
        b.key(Key::Enter, t0);
        let out = b.flash_result(Ok("written".into()), t0 + std::time::Duration::from_secs(61));
        assert_eq!(plays(&out), vec![Jingle::Done]);
        assert_eq!((b.ok, b.failed, b.last_secs), (1, 0, Some(61)));
        assert_eq!(b.phase, Phase::Eject { ok: true });
        assert!(b.key(Key::Char('r'), t0).is_empty(), "no retry after a success");
        // next cartridge: Enter, no ding (the operator just pressed a key), then Enter again to flash
        assert!(b.key(Key::Enter, t0).is_empty());
        assert_eq!(b.phase, Phase::Insert);
        b.key(Key::Enter, t0 + std::time::Duration::from_secs(70));
        let out = b.flash_result(Err("verify".into()), t0 + std::time::Duration::from_secs(110));
        assert_eq!(plays(&out), vec![Jingle::Error]);
        assert_eq!((b.ok, b.failed, b.last_secs), (1, 1, Some(40)));
        assert_eq!(b.phase, Phase::Eject { ok: false });
        assert!(b.message.as_ref().is_some_and(|m| m.0 == "verify" && m.1));
    }

    #[test]
    fn r_tries_the_same_failed_cartridge_again_without_counting_a_new_one() {
        let mut b = picked(false);
        let t0 = Instant::now();
        b.key(Key::Enter, t0);
        b.flash_result(Err("verify".into()), t0);
        let out = b.key(Key::Char('R'), t0);
        assert!(matches!(out.last(), Some(Out::Send(Cmd::BatchFlash { .. }))));
        assert!(b.flashing());
        b.flash_result(Ok("ok".into()), t0);
        assert_eq!((b.ok, b.failed), (1, 1), "the retry that works is one more good cartridge, the first failure stays");
    }

    #[test]
    fn a_cartridge_that_was_not_started_leaves_the_counters_alone() {
        let mut b = picked(false);
        let t0 = Instant::now();
        b.key(Key::Enter, t0);
        let out = b.skipped("no chip".into());
        assert_eq!(plays(&out), vec![Jingle::Error]);
        assert_eq!((b.ok, b.failed), (0, 0));
        assert_eq!(b.phase, Phase::Insert);
        assert!(b.message.as_ref().is_some_and(|m| m.1));
        assert!(matches!(b.key(Key::Enter, t0).last(), Some(Out::Send(Cmd::BatchFlash { .. }))), "Enter tries again");
    }
}
