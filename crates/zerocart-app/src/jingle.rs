//! The jingles of the serial flash mode: square-wave chiptune synthesised in memory (no sound files) and played
//! through the system's `aplay` (or `paplay`) on a thread of its own, so the screen never waits for the sound.
use std::io::Write;
use std::process::{Command, Stdio};

const RATE: u32 = 22_050;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Jingle {
    /// "Put your cartridge in" (the coin-like ding)
    Insert,
    /// "Cartridge recognised, I start" (ding ding)
    Detected,
    /// "Finished, eject" (a little fanfare)
    Done,
    /// "It failed, eject" (low buzz going down)
    Error,
}

/// (frequency in Hz, 0 = silence; duration in ms; duty of the square wave)
type Note = (f32, u32, f32);

fn score(j: Jingle) -> &'static [Note] {
    match j {
        Jingle::Insert => &[(987.8, 70, 0.5), (1318.5, 300, 0.5)],
        Jingle::Detected => &[(1318.5, 70, 0.5), (0.0, 40, 0.5), (1318.5, 70, 0.5), (0.0, 40, 0.5), (1975.5, 260, 0.5)],
        Jingle::Done => &[
            (523.3, 80, 0.25), (659.3, 80, 0.25), (784.0, 80, 0.25), (1046.5, 80, 0.25),
            (784.0, 60, 0.25), (1046.5, 60, 0.25), (1318.5, 380, 0.5),
        ],
        Jingle::Error => &[(220.0, 160, 0.125), (0.0, 30, 0.5), (164.8, 160, 0.125), (0.0, 30, 0.5), (110.0, 420, 0.125)],
    }
}

/// 16-bit mono little-endian samples of a score: a square wave with a short decay and a fade-out against clicks.
pub fn pcm(notes: &[Note]) -> Vec<u8> {
    let mut out = Vec::new();
    for &(freq, ms, duty) in notes {
        let n = (RATE as u64 * ms as u64 / 1000) as usize;
        let fade = (RATE as usize / 200).max(1); // 5 ms
        for i in 0..n {
            let s = if freq <= 0.0 {
                0.0
            } else {
                let phase = (freq * i as f32 / RATE as f32).fract();
                let level = if phase < duty { 1.0 } else { -1.0 };
                let decay = 1.0 - 0.5 * i as f32 / n as f32;
                let edge = (n - i).min(fade) as f32 / fade as f32;
                level * decay * edge * 0.22
            };
            out.extend_from_slice(&((s * 32767.0) as i16).to_le_bytes());
        }
    }
    out
}

/// The samples wrapped in a WAV header (what `aplay` reads from its standard input).
pub fn wav(pcm: &[u8]) -> Vec<u8> {
    let mut v = Vec::with_capacity(44 + pcm.len());
    v.extend_from_slice(b"RIFF");
    v.extend_from_slice(&(36 + pcm.len() as u32).to_le_bytes());
    v.extend_from_slice(b"WAVEfmt ");
    v.extend_from_slice(&16u32.to_le_bytes());
    v.extend_from_slice(&1u16.to_le_bytes()); // PCM
    v.extend_from_slice(&1u16.to_le_bytes()); // mono
    v.extend_from_slice(&RATE.to_le_bytes());
    v.extend_from_slice(&(RATE * 2).to_le_bytes());
    v.extend_from_slice(&2u16.to_le_bytes());
    v.extend_from_slice(&16u16.to_le_bytes());
    v.extend_from_slice(b"data");
    v.extend_from_slice(&(pcm.len() as u32).to_le_bytes());
    v.extend_from_slice(pcm);
    v
}

/// Plays the jingle without waiting for it. A machine with no player stays silent.
pub fn play(j: Jingle) {
    let samples = pcm(score(j));
    std::thread::spawn(move || {
        let players: [(&str, &[&str], bool); 2] = [
            ("aplay", &["-q", "-t", "wav", "-"], true),
            ("paplay", &["--raw", "--rate=22050", "--channels=1", "--format=s16le"], false),
        ];
        for (cmd, args, wants_wav) in players {
            let Ok(mut child) = Command::new(cmd).args(args).stdin(Stdio::piped()).stdout(Stdio::null()).stderr(Stdio::null()).spawn() else {
                continue;
            };
            if let Some(mut stdin) = child.stdin.take() {
                let _ = stdin.write_all(&if wants_wav { wav(&samples) } else { samples.clone() });
            }
            let _ = child.wait();
            return;
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_score_lasts_as_long_as_its_notes() {
        let bytes = pcm(&[(440.0, 100, 0.5), (0.0, 50, 0.5)]);
        assert_eq!(bytes.len(), 2 * (RATE as usize * 150 / 1000));
    }

    #[test]
    fn the_wav_header_matches_the_samples() {
        let w = wav(&pcm(score(Jingle::Insert)));
        assert_eq!(&w[0..4], b"RIFF");
        assert_eq!(&w[8..12], b"WAVE");
        let data = u32::from_le_bytes(w[40..44].try_into().unwrap()) as usize;
        assert_eq!(w.len(), 44 + data);
        assert_eq!(u32::from_le_bytes(w[4..8].try_into().unwrap()) as usize, 36 + data);
    }

    #[test]
    fn every_jingle_makes_a_sound_that_is_not_too_loud() {
        for j in [Jingle::Insert, Jingle::Detected, Jingle::Done, Jingle::Error] {
            let s: Vec<i16> = pcm(score(j)).chunks(2).map(|c| i16::from_le_bytes([c[0], c[1]])).collect();
            let peak = s.iter().map(|v| v.unsigned_abs()).max().unwrap();
            assert!(peak > 3000 && peak < 12000, "{j:?}: {peak}");
        }
    }
}
