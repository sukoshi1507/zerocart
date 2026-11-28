// SPDX-License-Identifier: GPL-3.0-or-later
//! The serial flash screen, dressed as a 1990s cracktro: copper bars behind a shadowed logo, a starfield and a
//! sine-wave scroller. Everything is a function of the time `t_ms`, so the application only has to redraw.
use crate::style::*;
use crate::widgets::Painter;
use crate::{Lang, ProgressView, Rgb, SCREEN_W};

/// What the operator has to do now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BatchPhase {
    /// Choosing the ROM to put on every cartridge
    Pick,
    /// "Put a cartridge in"
    Insert,
    /// Erasing, writing, checking
    Flashing,
    /// Finished (`ok`) or failed: take the cartridge out
    Eject { ok: bool },
}

pub struct BatchView<'a> {
    pub lang: Lang,
    pub phase: BatchPhase,
    /// ROM file names to pick from (phase `Pick`) and the selected one
    pub files: &'a [String],
    pub selected: usize,
    /// The ROM being written, its name
    pub file: &'a str,
    pub ok: u32,
    pub failed: u32,
    /// Seconds the last cartridge took, if there was one
    pub last_secs: Option<u64>,
    pub progress: Option<&'a ProgressView>,
    /// A problem or a result to show under the status line
    pub message: Option<(&'a str, bool)>,
    /// The animation clock
    pub t_ms: u64,
}

const BAND_TOP: i32 = CONTENT_TOP + 2;
const BAND_H: i32 = 40;
const STATUS_TOP: i32 = BAND_TOP + BAND_H + 4;
const SCROLL_TOP: i32 = FOOT_TOP - 33;
const SCROLL_H: i32 = 30;
const X0: i32 = INSET + 2;
const W: i32 = INNER_W - 4;

const SCROLL_TEXT: &str = "   *** ZEROCART SERIAL FLASH ***   INSERT A CARTRIDGE - PRESS ENTER - \
    WAIT FOR THE VERDICT - EJECT - NEXT !   NO BACKUP, NO MERCY, BLANK CARTRIDGES ONLY   \
    KEEP THE PACE, KEEP THE FLASH   *** ZEROSTACK ***   ";

/// The intensity (0..=255) of a copper bar `dy` lines from its middle, `half` lines to each side: a bright core
/// with a smooth falloff, like the gradient lines the Amiga's copper drew.
pub fn copper_level(dy: i32, half: i32) -> u8 {
    if dy.abs() > half || half == 0 {
        return 0;
    }
    let x = 1.0 - dy.abs() as f32 / half as f32;
    (255.0 * x * x * (3.0 - 2.0 * x)) as u8
}

fn shade(c: Rgb, level: u8) -> Rgb {
    let f = |sh: u32| ((((c >> sh) & 0xff) * level as u32) / 255) << sh;
    f(16) | f(8) | f(0)
}

fn mix(a: Rgb, b: Rgb, k: f32) -> Rgb {
    let f = |sh: u32| {
        let (x, y) = (((a >> sh) & 0xff) as f32, ((b >> sh) & 0xff) as f32);
        ((x + (y - x) * k) as u32) << sh
    };
    f(16) | f(8) | f(0)
}

/// A colour that cycles through a fire palette with `phase` (any real number).
pub fn fire(phase: f32) -> Rgb {
    const STOPS: [Rgb; 5] = [0x8a1020, 0xe03a2a, 0xf3c93a, 0xf1ebd8, 0xf3c93a];
    let p = phase.rem_euclid(STOPS.len() as f32);
    let i = p as usize;
    mix(STOPS[i], STOPS[(i + 1) % STOPS.len()], p.fract())
}

fn sin(x: f32) -> f32 {
    x.sin()
}

fn copper_band(p: &mut Painter, t: f32) {
    p.rect(X0, BAND_TOP, W, BAND_H, 0x0b0608);
    const BARS: [Rgb; 4] = [0xc8202f, 0xf3c93a, 0xece6d3, 0xff7a3a];
    // far bars first: the one with the larger "depth" is drawn behind the others
    let mut order: Vec<usize> = (0..BARS.len()).collect();
    let depth = |k: usize| sin(t * 1.7 + k as f32 * 1.57);
    order.sort_by(|&a, &b| depth(a).partial_cmp(&depth(b)).unwrap_or(std::cmp::Ordering::Equal));
    for k in order {
        let mid = BAND_TOP + BAND_H / 2 + (sin(t * 1.3 + k as f32 * 0.9) * (BAND_H as f32 / 2.0 - 8.0)) as i32;
        for dy in -8..=8 {
            let y = mid + dy;
            if y < BAND_TOP || y >= BAND_TOP + BAND_H {
                continue;
            }
            let lvl = copper_level(dy, 8);
            if lvl > 0 {
                p.rect(X0, y, W, 1, shade(BARS[k], lvl));
            }
        }
    }
    // the scanline sparkle of the top and bottom edges
    p.rect(X0, BAND_TOP, W, 1, ACCENT);
    p.rect(X0, BAND_TOP + BAND_H - 1, W, 1, ACCENT);
}

fn logo(p: &mut Painter, text: &str, sub: &str, t: f32) {
    let w = p.fonts.width(text, 18, true);
    let x = (SCREEN_W - w) / 2;
    let y = BAND_TOP + 2;
    p.text(text, 18, true, 0x000000, x + 2, y + 2, None);
    p.text(text, 18, true, 0x240810, x + 1, y + 1, None);
    p.text(text, 18, true, TEXT, x, y, None);
    // a soft glint that sweeps over the logo
    let centre = x + ((t * 0.7).fract() * (w + 60) as f32) as i32 - 30;
    for dx in -8..=8i32 {
        let a = (70 - dx.abs() * 8).max(0) as u8;
        for yy in y + 2..y + 22 {
            p.canvas.blend(centre + dx, yy, 0xffffff, a);
        }
    }
    let sw = p.fonts.width(sub, 9, true);
    p.text(sub, 9, true, 0x000000, (SCREEN_W - sw) / 2 + 1, BAND_TOP + 27, Some(W));
    p.text(sub, 9, true, PRIMARY, (SCREEN_W - sw) / 2, BAND_TOP + 26, Some(W));
}

fn starfield(p: &mut Painter, t_ms: u64) {
    p.rect(X0, SCROLL_TOP, W, SCROLL_H, 0x07070b);
    let mut seed = 0x2545_f491u32;
    for i in 0..48 {
        seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        let layer = 1 + i % 3; // 1..=3: speed and brightness
        let y = SCROLL_TOP + 1 + ((seed >> 8) % (SCROLL_H as u32 - 2)) as i32;
        let base = ((seed >> 16) % W as u32) as i32;
        let x = X0 + (base - (t_ms * layer as u64 / 22) as i32).rem_euclid(W);
        p.rect(x, y, 1, 1, shade(0xf1ebd8, (60 + 60 * layer) as u8));
    }
    p.rect(X0, SCROLL_TOP, W, 1, BORDER);
    p.rect(X0, SCROLL_TOP + SCROLL_H - 1, W, 1, BORDER);
}

fn scroller(p: &mut Painter, t_ms: u64) {
    let t = t_ms as f32 / 1000.0;
    let mut x = X0 + W - (t_ms as f32 * 0.075) as i32 % (text_width(p) + W);
    for (i, ch) in SCROLL_TEXT.chars().enumerate() {
        let s = ch.to_string();
        let w = p.fonts.width(&s, 14, true).max(4);
        if x + w > X0 + 2 && x < X0 + W - 2 && ch != ' ' {
            let y = SCROLL_TOP + 5 + (sin(x as f32 * 0.045 + t * 4.0) * 6.0) as i32;
            let c = fire(i as f32 * 0.35 - t * 3.0);
            p.text(&s, 14, true, 0x000000, x + 1, y + 1, None);
            p.text(&s, 14, true, c, x, y, None);
        }
        x += w;
    }
}

fn text_width(p: &mut Painter) -> i32 {
    SCROLL_TEXT.chars().map(|c| p.fonts.width(&c.to_string(), 14, true).max(4)).sum()
}

/// A big line with a drop shadow, centred, optionally blinking.
fn headline(p: &mut Painter, s: &str, color: Rgb, y: i32, t_ms: u64, blink: bool) {
    if blink && (t_ms / 450) % 2 == 1 {
        return;
    }
    let w = p.fonts.width(s, 16, true);
    let x = ((SCREEN_W - w) / 2).max(X0);
    p.text(s, 16, true, 0x000000, x + 1, y + 1, Some(W));
    p.text(s, 16, true, color, x, y, Some(W));
}

fn centered(p: &mut Painter, s: &str, px: u32, color: Rgb, y: i32) {
    p.text_centered(s, px, false, color, X0, y, W, px as i32 + 4);
}

/// "OK 11  FAILED 1  1:34", shown in the title bar once a ROM is chosen.
fn counters(v: &BatchView) -> String {
    let l = v.lang;
    let mut s = format!(
        "{} {}  {} {}",
        loc!(l, fr: "OK", en: "OK", ja: "成功", zh: "成功"),
        v.ok,
        loc!(l, fr: "ÉCHEC", en: "FAILED", ja: "失敗", zh: "失败"),
        v.failed
    );
    if let Some(secs) = v.last_secs {
        s.push_str(&format!("  {}:{:02}", secs / 60, secs % 60));
    }
    s
}

fn picker(p: &mut Painter, v: &BatchView) {
    let l = v.lang;
    p.text(
        loc!(l, fr: "ROM à mettre sur toutes les cartouches :", en: "ROM for every cartridge:", ja: "全カートに書き込むROM：", zh: "要写入所有卡带的ROM："),
        10, true, TEXT, X0 + 6, STATUS_TOP, Some(W - 12),
    );
    let rows = 4;
    let first = v.selected.saturating_sub(rows - 1).min(v.files.len().saturating_sub(rows));
    for (n, name) in v.files.iter().enumerate().skip(first).take(rows) {
        let y = STATUS_TOP + 16 + (n - first) as i32 * 17;
        if n == v.selected {
            p.rect(X0, y, W, 16, SELECTED_ROW);
            p.cursor(X0 + 3, y + 4);
        }
        p.text(name, 10, n == v.selected, if n == v.selected { TEXT } else { DIM }, X0 + 16, y + 2, Some(W - 24));
    }
}

fn flashing(p: &mut Painter, v: &BatchView, t_ms: u64) {
    let l = v.lang;
    headline(p, loc!(l, fr: "FLASH EN COURS", en: "FLASHING", ja: "書き込み中", zh: "烧录中"), fire(t_ms as f32 / 250.0), STATUS_TOP, t_ms, false);
    let (bx, by, bw, bh) = (X0 + 10, STATUS_TOP + 21, W - 20, 12);
    p.canvas.bordered(bx, by, bw, bh, BUTTON, 2, CREAM);
    if let Some(pr) = v.progress {
        let pct = (pr.done * 100).checked_div(pr.total).unwrap_or(0).min(100) as i32;
        let fill = (bw - 4) * pct / 100;
        // the filled part scrolls like a barber's pole
        for i in 0..fill {
            let c = fire((i as f32 + t_ms as f32 * 0.06) / 14.0);
            p.rect(bx + 2 + i, by + 2, 1, bh - 4, c);
        }
        p.text(&pr.label, 9, false, DIM, X0 + 10, STATUS_TOP + 36, Some(W - 60));
        p.text_right(&format!("{pct}%"), 9, true, TEXT, 12, STATUS_TOP + 36);
    } else {
        centered(p, loc!(l, fr: "Cartouche reconnue", en: "Cartridge recognised", ja: "カートを認識", zh: "已识别卡带"), 10, DIM, STATUS_TOP + 36);
    }
}

pub fn render_batch(p: &mut Painter, v: &BatchView) {
    let l = v.lang;
    let t_ms = v.t_ms;
    let t = t_ms as f32 / 1000.0;
    p.canvas.fill_rect(0, 0, crate::SCREEN_W, crate::SCREEN_H, BACKGROUND);
    p.frame();
    let title_right = if v.phase == BatchPhase::Pick {
        loc!(l, fr: "mode série", en: "serial mode", ja: "連続書込", zh: "连续烧录").to_string()
    } else {
        counters(v)
    };
    p.bar("ZEROCART", &title_right, None);
    copper_band(p, t);
    let n = v.ok + v.failed + u32::from(v.phase != BatchPhase::Pick && !matches!(v.phase, BatchPhase::Eject { .. }));
    let sub = match v.phase {
        BatchPhase::Pick => loc!(l, fr: "FLASH EN SÉRIE", en: "SERIAL FLASH", ja: "連続書き込み", zh: "连续烧录").to_string(),
        _ => format!("{}  #{n}", v.file),
    };
    logo(p, "ZEROCART", &sub, t);

    match v.phase {
        BatchPhase::Pick => picker(p, v),
        BatchPhase::Insert => {
            headline(p, loc!(l, fr: "METTEZ UNE CARTOUCHE", en: "INSERT A CARTRIDGE", ja: "カートを挿入", zh: "请插入卡带"), PRIMARY, STATUS_TOP, t_ms, true);
            if v.message.is_none() {
                centered(p, loc!(l, fr: "bien enfoncée, puis Entrée", en: "pushed all the way in, then Enter", ja: "奥まで挿して Enter", zh: "插到底后按 Enter"), 10, DIM, STATUS_TOP + 20);
            }
        }
        BatchPhase::Flashing => flashing(p, v, t_ms),
        BatchPhase::Eject { ok } => {
            if ok {
                headline(p, loc!(l, fr: "TERMINÉ - ÉJECTEZ", en: "DONE - EJECT", ja: "完了 - 取り出して", zh: "完成 - 请取出"), 0x6cff7a, STATUS_TOP, t_ms, false);
            } else {
                headline(p, loc!(l, fr: "ÉCHEC - ÉJECTEZ", en: "FAILED - EJECT", ja: "失敗 - 取り出して", zh: "失败 - 请取出"), ERROR, STATUS_TOP, t_ms, true);
            }
        }
    }
    starfield(p, t_ms);
    scroller(p, t_ms);
    if let Some((m, err)) = v.message {
        if !matches!(v.phase, BatchPhase::Pick | BatchPhase::Flashing) {
            p.paragraph(m, 9, false, if err { ERROR } else { DIM }, X0 + 4, STATUS_TOP + 19, W - 8);
        }
    }

    let hints: Vec<(&str, &str)> = match v.phase {
        BatchPhase::Pick => vec![("F X", crate::loc::word::pick(l)), ("Enter", crate::loc::word::confirm(l)), ("Esc", crate::loc::word::back(l))],
        BatchPhase::Insert => vec![
            ("Enter", loc!(l, fr: "flasher", en: "flash", ja: "書込", zh: "烧录")),
            ("Esc", loc!(l, fr: "arrêter", en: "stop", ja: "停止", zh: "停止")),
        ],
        BatchPhase::Flashing => vec![],
        BatchPhase::Eject { ok } => {
            let mut h = vec![("Enter", loc!(l, fr: "suivante", en: "next", ja: "次へ", zh: "下一个"))];
            if !ok {
                h.push(("R", loc!(l, fr: "réessayer", en: "retry", ja: "再試行", zh: "重试")));
            }
            h.push(("Esc", loc!(l, fr: "arrêter", en: "stop", ja: "停止", zh: "停止")));
            h
        }
    };
    if v.phase == BatchPhase::Flashing {
        p.footer(&[], Some((loc!(l, fr: "Ne pas débrancher.", en: "Do not unplug.", ja: "抜かないでください。", zh: "请勿拔出。"), false)));
    } else {
        p.footer(&hints, None);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Canvas, Fonts, SCREEN_H};

    #[test]
    fn a_copper_bar_is_brightest_in_the_middle_and_dark_at_its_edges() {
        assert_eq!(copper_level(0, 8), 255);
        assert_eq!(copper_level(9, 8), 0);
        assert!(copper_level(2, 8) > copper_level(5, 8));
        assert_eq!(copper_level(-3, 8), copper_level(3, 8));
    }

    #[test]
    fn the_fire_palette_wraps_and_stays_a_colour() {
        for i in -20..60 {
            assert!(fire(i as f32 * 0.37) <= 0xffffff);
        }
        assert_eq!(fire(0.0), fire(5.0));
    }

    #[test]
    fn every_phase_renders_in_every_language_and_the_animation_moves() {
        let Ok(fonts) = Fonts::load(Lang::Fr) else { return };
        let files = vec!["a.gba".to_string(), "b.gba".to_string()];
        let pr = ProgressView { label: "x".into(), done: 3, total: 10, kib_s: 1.0 };
        let mut frames = Vec::new();
        for phase in [BatchPhase::Pick, BatchPhase::Insert, BatchPhase::Flashing, BatchPhase::Eject { ok: true }, BatchPhase::Eject { ok: false }] {
            for t_ms in [0u64, 777] {
                let mut canvas = Canvas::new(SCREEN_W, SCREEN_H, 0);
                let mut p = Painter { canvas: &mut canvas, fonts: &fonts };
                let v = BatchView {
                    lang: Lang::Fr, phase, files: &files, selected: 1, file: "a.gba", ok: 3, failed: 1, last_secs: Some(75),
                    progress: Some(&pr), message: Some(("m", true)), t_ms,
                };
                render_batch(&mut p, &v);
                frames.push(canvas.to_rgba());
            }
        }
        assert!(frames.chunks(2).all(|f| f[0] != f[1]), "each phase must animate");
    }
}
