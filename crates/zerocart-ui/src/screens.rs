// SPDX-License-Identifier: GPL-3.0-or-later
//! The start screens: splash, choice of the system, choice of the reader.
use crate::icons::{self, Sprite, System};
use crate::loc;
use crate::style::*;
use crate::widgets::Painter;
use crate::{Lang, SCREEN_H, SCREEN_W};

pub fn render_splash(p: &mut Painter, lang: Lang, version: &str) {
    p.canvas.fill_rect(0, 0, SCREEN_W, SCREEN_H, BACKGROUND);
    p.frame();
    let cell = 4;
    let size = icons::cells(Sprite::Logo) * cell;
    icons::draw(p.canvas, Sprite::Logo, (SCREEN_W - size) / 2, 14, cell);
    p.text_centered("ZEROCART", 18, true, TEXT, 0, 96, SCREEN_W, 22);
    let sub = loc!(lang,
        fr: "Lecteur et flasher de cartouches",
        en: "Cartridge reader and flasher",
        ja: "カートリッジ リーダー＆ライター",
        zh: "卡带读取与烧录工具");
    p.text_centered(sub, 10, false, DIM, 0, 120, SCREEN_W, 14);
    p.text_centered(&format!("v{version}"), 9, false, BORDER, 0, 140, SCREEN_W, 12);
}

/// One tile per system; `selected` is an index into [`System::ALL`].
pub fn render_system(p: &mut Painter, lang: Lang, selected: usize, banner: Option<(&str, bool)>) {
    p.canvas.fill_rect(0, 0, SCREEN_W, SCREEN_H, BACKGROUND);
    p.frame();
    p.bar("ZEROCART", loc!(lang, fr: "quelle cartouche ?", en: "which cartridge?", ja: "どのカート？", zh: "哪种卡带？"), None);
    let (tile_w, gap, top, tile_h) = (72, 6, 34, 96);
    let left = (SCREEN_W - (4 * tile_w + 3 * gap)) / 2;
    for (i, sys) in System::ALL.iter().enumerate() {
        let x = left + i as i32 * (tile_w + gap);
        let sel = i == selected;
        p.canvas.bordered(x, top, tile_w, tile_h, if sel { SELECTED_ROW } else { SURFACE }, 2, if sel { ACCENT } else { BORDER });
        let cell = 3;
        let size = icons::cells(sys.sprite()) * cell;
        icons::draw(p.canvas, sys.sprite(), x + (tile_w - size) / 2, top + 8, cell);
        p.text_centered(sys.label(), 10, sel, if sel { TEXT } else { DIM }, x, top + 64, tile_w, 14);
        let v = match sys {
            System::Gba => "3,3 V",
            System::MegaDrive => "MD Flasher",
            _ => "5 V",
        };
        p.text_centered(v, 9, false, if sel { PRIMARY } else { BORDER }, x, top + 78, tile_w, 12);
    }
    p.footer(
        &[
            ("Z C", loc::word::pick(lang)),
            ("Enter", "ok"),
            ("I", loc!(lang, fr: "série", en: "batch", ja: "連続", zh: "连续")),
            ("S", loc!(lang, fr: "partager", en: "share", ja: "共有", zh: "分享")),
            ("L", loc!(lang, fr: "langue", en: "lang", ja: "言語", zh: "语言")),
            ("Esc", loc::word::quit(lang)),
        ],
        banner,
    );
}

/// A reader that can take the chosen system, with what the USB scan says about it.
#[derive(Debug, Clone)]
pub struct ReaderRow {
    pub name: String,
    /// Port when plugged in, else a hint
    pub detail: String,
    pub present: bool,
}

impl ReaderRow {
    /// The same row shown as not plugged in (demos and tests).
    pub fn clone_absent(&self) -> ReaderRow {
        ReaderRow { present: false, ..self.clone() }
    }
}

pub fn render_readers(p: &mut Painter, lang: Lang, system: System, rows: &[ReaderRow], selected: usize, connecting: bool, banner: Option<(&str, bool)>) {
    p.canvas.fill_rect(0, 0, SCREEN_W, SCREEN_H, BACKGROUND);
    p.frame();
    p.bar("ZEROCART", system.label(), None);
    // the chosen system, small, then the question
    icons::draw(p.canvas, system.sprite(), 12, 28, 2);
    p.text(loc!(lang, fr: "Quel lecteur est branché ?", en: "Which reader is plugged in?", ja: "どのリーダーを接続しましたか？", zh: "连接的是哪个读卡器？"), 12, true, TEXT, 52, 30, Some(SCREEN_W - 64));
    let found = rows.iter().filter(|r| r.present).count();
    let line = if found == 0 {
        loc!(lang,
            fr: "Aucun lecteur détecté : branchez-en un.",
            en: "No reader detected: plug one in.",
            ja: "リーダーが見つかりません。接続してください。",
            zh: "未检测到读卡器：请连接一个。")
    } else {
        loc!(lang,
            fr: "Vérification en direct, Entrée pour confirmer.",
            en: "Checked live, Enter to confirm.",
            ja: "リアルタイム検出。Enterで確定。",
            zh: "实时检测，按Enter确认。")
    };
    p.text(line, 9, false, if found == 0 { PRIMARY } else { DIM }, 52, 46, Some(SCREEN_W - 64));
    for (i, r) in rows.iter().enumerate() {
        let y = 70 + i as i32 * 26;
        let sel = i == selected;
        if sel {
            p.rect(10, y, SCREEN_W - 20, 24, SELECTED_ROW);
            p.cursor(12, y + 8);
        }
        let dot = if r.present { 0x4CD964 } else { 0x7A3A3A };
        p.canvas.fill_rounded(26, y + 8, 8, 8, dot);
        p.text(&r.name, 11, sel, if r.present { TEXT } else { DIM }, 42, y + 5, Some(SCREEN_W - 170));
        let state = if r.present {
            loc!(lang, fr: "branché", en: "plugged", ja: "接続済", zh: "已连接")
        } else {
            loc!(lang, fr: "non détecté", en: "not found", ja: "未検出", zh: "未检测到")
        };
        p.text_right(state, 9, false, if r.present { PRIMARY } else { BORDER }, 16, y + 3);
        p.text_right(&r.detail, 8, false, DIM, 16, y + 13);
    }
    let hints: &[(&str, &str)] = &[
        ("F X", loc::word::pick(lang)),
        ("Enter", loc::word::confirm(lang)),
        ("Esc", loc::word::back(lang)),
    ];
    if connecting {
        p.footer(&[], Some((loc!(lang, fr: "Connexion au lecteur...", en: "Connecting...", ja: "リーダーに接続中...", zh: "正在连接读卡器..."), false)));
    } else {
        p.footer(hints, banner);
    }
}

/// A screen for something planned but not implemented yet.
pub fn render_placeholder(p: &mut Painter, lang: Lang, system: System, lines: &[&str]) {
    p.canvas.fill_rect(0, 0, SCREEN_W, SCREEN_H, BACKGROUND);
    p.frame();
    p.bar("ZEROCART", system.label(), None);
    icons::draw(p.canvas, system.sprite(), 14, 34, 4);
    let mut y = 40;
    for (i, l) in lines.iter().enumerate() {
        p.text(l, if i == 0 { 12 } else { 10 }, i == 0, if i == 0 { TEXT } else { DIM }, 92, y, Some(SCREEN_W - 104));
        y += if i == 0 { 20 } else { 15 };
    }
    p.footer(&[("Esc", loc::word::back(lang))], None);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Canvas, Fonts};

    #[test]
    fn every_start_screen_renders_when_fonts_exist() {
        let Ok(fonts) = Fonts::load(Lang::Fr) else { return };
        let mut canvas = Canvas::new(SCREEN_W, SCREEN_H, 0);
        let mut p = Painter { canvas: &mut canvas, fonts: &fonts };
        render_splash(&mut p, Lang::Fr, "0.1.0");
        for sel in 0..4 {
            render_system(&mut p, Lang::Fr, sel, None);
        }
        let rows = vec![
            ReaderRow { name: "GBFlash / GBxCart RW".into(), detail: "/dev/ttyUSB0".into(), present: true },
            ReaderRow { name: "Chisflash".into(), detail: "USB 0483:0721".into(), present: false },
        ];
        render_readers(&mut p, Lang::Fr, System::Gba, &rows, 0, false, None);
        render_readers(&mut p, Lang::En, System::Gb, &[], 0, true, Some(("x", true)));
        render_placeholder(&mut p, Lang::Fr, System::MegaDrive, &["Mega Drive : bientôt", "ligne"]);
        assert!(canvas.get(0, 0).is_some());
    }
}
