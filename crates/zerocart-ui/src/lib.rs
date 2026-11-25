//! The screens of ZeroCart for the 320x170 panel: cartridge info, actions with progress, tools.
//! Pure drawing: the application builds a [`View`] and calls [`render`].
// SPDX-License-Identifier: GPL-3.0-or-later
// canvas, font, style, widgets and i18n were copied from ZeroTag (MIT, see THIRD_PARTY.md) so that this
// project does not depend on it.

pub mod canvas;
pub mod font;
pub mod icons;
pub mod i18n;
pub mod screens;
pub mod share_screen;
pub mod style;
pub mod widgets;

pub use canvas::{Canvas, Rgb};
pub use font::Fonts;
pub use icons::{Sprite, System};
pub use i18n::Lang;

pub const SCREEN_W: i32 = 320;
pub const SCREEN_H: i32 = 170;

use style::*;
use widgets::Painter;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Tab {
    #[default]
    Info,
    Actions,
    Tools,
}

impl Tab {
    pub const ALL: [Tab; 3] = [Tab::Info, Tab::Actions, Tab::Tools];
    pub fn next(self) -> Tab {
        Tab::ALL[(Tab::ALL.iter().position(|&t| t == self).unwrap() + 1) % 3]
    }
    pub fn prev(self) -> Tab {
        Tab::ALL[(Tab::ALL.iter().position(|&t| t == self).unwrap() + 2) % 3]
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    ReadRom,
    ReadSave,
    WriteSave,
    FlashRom,
}

impl Action {
    pub const ALL: [Action; 4] = [Action::ReadRom, Action::ReadSave, Action::WriteSave, Action::FlashRom];
    /// Whether the action changes the cartridge (and so asks for a file and a confirmation).
    pub fn writes(self) -> bool {
        matches!(self, Action::WriteSave | Action::FlashRom)
    }
    pub fn label(self, lang: Lang) -> &'static str {
        let fr = lang == Lang::Fr;
        match (self, fr) {
            (Action::ReadRom, true) => "Lire la ROM",
            (Action::ReadRom, false) => "Read ROM",
            (Action::ReadSave, true) => "Lire la sauvegarde",
            (Action::ReadSave, false) => "Read save",
            (Action::WriteSave, true) => "Écrire la sauvegarde",
            (Action::WriteSave, false) => "Write save",
            (Action::FlashRom, true) => "Flasher une ROM",
            (Action::FlashRom, false) => "Flash a ROM",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReaderView {
    pub name: String,
    pub firmware: String,
    /// "3.3V" or "5V"; empty for a reader that does not set it (the Chisflash).
    pub voltage: &'static str,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CartView {
    /// "GBA" or "GB/GBC"
    pub system: &'static str,
    pub title: String,
    pub code: String,
    pub rom: String,
    pub save: String,
    pub database: Option<String>,
    pub header_ok: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ProgressView {
    pub label: String,
    pub done: u64,
    pub total: u64,
    pub kib_s: f32,
}

/// A screen drawn over the tab content: a file picker or a confirmation.
pub enum Overlay<'a> {
    Picker { title: &'a str, items: &'a [String], selected: usize },
    Confirm { lines: &'a [String] },
}

pub struct View<'a> {
    pub overlay: Option<Overlay<'a>>,
    pub lang: Lang,
    pub tab: Tab,
    pub reader: Option<&'a ReaderView>,
    pub cart: Option<&'a CartView>,
    pub scanning: bool,
    pub action_sel: usize,
    pub progress: Option<&'a ProgressView>,
    pub banner: Option<(&'a str, bool)>,
    /// Result lines of the flash chip analysis (Tools tab) and the index of the first line shown.
    pub analysis: Option<&'a [String]>,
    pub analysis_scroll: usize,
    pub analysing: bool,
}

fn tab_name(t: Tab, lang: Lang) -> &'static str {
    match (t, lang == Lang::Fr) {
        (Tab::Info, true) => "Cartouche",
        (Tab::Info, false) => "Cartridge",
        (Tab::Actions, true) => "Actions",
        (Tab::Actions, false) => "Actions",
        (Tab::Tools, true) => "Outils",
        (Tab::Tools, false) => "Tools",
    }
}

pub fn render(p: &mut Painter, v: &View) {
    let fr = v.lang == Lang::Fr;
    p.canvas.fill_rect(0, 0, SCREEN_W, SCREEN_H, BACKGROUND);
    p.frame();
    match v.reader {
        Some(r) => p.bar(format!("{} {}", r.name, r.firmware).trim(), "", None),
        None => p.bar("ZEROCART", if fr { "aucun lecteur" } else { "no reader" }, None),
    }
    if let Some(r) = v.reader {
        let w = p.fonts.width(r.voltage, 10, true);
        let color = if r.voltage == "5V" { ERROR } else { PRIMARY };
        p.text(r.voltage, 10, true, color, SCREEN_W - INSET - 6 - w, INSET + 3, None);
    }

    // Tab strip
    let mut x = 10;
    for t in Tab::ALL {
        let label = tab_name(t, v.lang);
        let w = p.fonts.width(label, 10, t == v.tab) + 16;
        let (fill, border, color) = if t == v.tab { (SELECTED_ROW, ACCENT, TEXT) } else { (BUTTON, BORDER, DIM) };
        p.button(x, 26, w, label, fill, border, color, t == v.tab);
        x += w + 4;
    }

    if let Some(o) = &v.overlay {
        overlay(p, v, o);
        return;
    }
    match v.tab {
        Tab::Info => info(p, v),
        Tab::Actions => actions(p, v),
        Tab::Tools => tools(p, v),
    }

    let hints: Vec<(&str, &str)> = match (v.tab, v.progress.is_some()) {
        (_, true) => vec![("Esc", if fr { "annuler" } else { "cancel" })],
        (Tab::Tools, _) => vec![
            ("Z C", if fr { "onglet" } else { "tab" }),
            ("Enter", if fr { "analyser" } else { "analyse" }),
            ("F X", if fr { "défiler" } else { "scroll" }),
            ("Esc", if fr { "quitter" } else { "quit" }),
        ],
        (Tab::Actions, _) => vec![
            ("Z C", if fr { "onglet" } else { "tab" }),
            ("F X", if fr { "choisir" } else { "pick" }),
            ("Enter", if fr { "lancer" } else { "run" }),
            ("Esc", if fr { "quitter" } else { "quit" }),
        ],
        _ => vec![
            ("Z C", if fr { "onglet" } else { "tab" }),
            ("R", match (v.reader.is_some_and(|r| r.voltage.is_empty()), fr) {
                (true, true) => "lire GBA",
                (true, false) => "read GBA",
                (false, true) => "lire GBA 3,3V",
                (false, false) => "read GBA 3.3V",
            }),
            ("G", match (v.reader.is_some_and(|r| r.voltage.is_empty()), fr) {
                (true, true) => "lire GB",
                (true, false) => "read GB",
                (false, true) => "lire GB 5V",
                (false, false) => "read GB 5V",
            }),
            ("Esc", if fr { "quitter" } else { "quit" }),
        ],
    };
    p.footer(&hints, v.banner);
}

/// Lines of the analysis that fit under the tab strip.
pub const ANALYSIS_ROWS: usize = 6;

fn tools(p: &mut Painter, v: &View) {
    let fr = v.lang == Lang::Fr;
    let x = 14;
    let Some(lines) = v.analysis else {
        let msg = if v.analysing {
            if fr { "Analyse de la puce flash..." } else { "Analysing the flash chip..." }
        } else if fr {
            "Entrée : analyser la puce flash (CFI) de la cartouche"
        } else {
            "Enter: analyse the cartridge's flash chip (CFI)"
        };
        p.text(msg, 11, false, if v.analysing { PRIMARY } else { DIM }, x, 62, Some(SCREEN_W - 28));
        if !v.analysing {
            let note = if fr { "Lecture seule : rien n'est effacé ni écrit." } else { "Read-only: nothing is erased or written." };
            p.text(note, 10, false, DIM, x, 80, Some(SCREEN_W - 28));
        }
        return;
    };
    let first = v.analysis_scroll.min(lines.len().saturating_sub(ANALYSIS_ROWS));
    for (i, l) in lines.iter().skip(first).take(ANALYSIS_ROWS).enumerate() {
        let color = if l.starts_with('!') { ERROR } else if i == 0 && first == 0 { TEXT } else { DIM };
        p.text(l.trim_start_matches('!'), 10, i == 0 && first == 0, color, x, 47 + i as i32 * 16, Some(SCREEN_W - 28 - 8));
    }
    if lines.len() > ANALYSIS_ROWS {
        let track = ANALYSIS_ROWS as i32 * 16;
        let bar = (track * ANALYSIS_ROWS as i32 / lines.len() as i32).max(6);
        let y = 47 + (track - bar) * first as i32 / (lines.len() - ANALYSIS_ROWS).max(1) as i32;
        p.rect(SCREEN_W - 12, y, 2, bar, BORDER);
    }
}

fn overlay(p: &mut Painter, v: &View, o: &Overlay) {
    let fr = v.lang == Lang::Fr;
    match o {
        Overlay::Picker { title, items, selected } => {
            p.text(title, 11, true, TEXT, 14, 47, Some(SCREEN_W - 28));
            let first = selected.saturating_sub(3).min(items.len().saturating_sub(4));
            for (row, item) in items.iter().enumerate().skip(first).take(4) {
                let y = 62 + (row - first) as i32 * 21;
                let sel = row == *selected;
                if sel {
                    p.rect(10, y, SCREEN_W - 20, 19, SELECTED_ROW);
                    p.cursor(12, y + 5);
                }
                p.text(item, 11, sel, if sel { TEXT } else { DIM }, 26, y + 2, Some(SCREEN_W - 44));
            }
            p.footer(&[("F X", if fr { "choisir" } else { "pick" }), ("Enter", if fr { "ok" } else { "ok" }), ("Esc", if fr { "retour" } else { "back" })], v.banner);
        }
        Overlay::Confirm { lines } => {
            let mut y = 47;
            for (i, l) in lines.iter().enumerate() {
                y += 1 + p.paragraph(l, 11, i == 0, if i == 0 { ERROR } else { TEXT }, 14, y, SCREEN_W - 28) as i32 * 14;
            }
            p.footer(&[("Y", if fr { "confirmer" } else { "confirm" }), ("Esc", if fr { "annuler" } else { "cancel" })], v.banner);
        }
    }
}

fn info(p: &mut Painter, v: &View) {
    let fr = v.lang == Lang::Fr;
    let (x, w) = (14, SCREEN_W - 28);
    let Some(c) = v.cart else {
        let msg = if v.scanning {
            if fr { "Lecture de la cartouche..." } else { "Reading the cartridge..." }
        } else if v.reader.is_none() {
            if fr { "Branchez un GBFlash / GBxCart RW" } else { "Plug in a GBFlash / GBxCart RW" }
        } else if fr {
            "Aucune cartouche reconnue"
        } else {
            "No cartridge recognised"
        };
        p.text(msg, 12, false, DIM, x, 66, Some(w));
        return;
    };
    p.text(&c.title, 14, true, TEXT, x, 48, Some(w - 60));
    let w_sys = p.fonts.width(c.system, 10, true) + 10;
    p.chip(SCREEN_W - 14 - w_sys, 48, w_sys, 16, c.system, 10, true, PRIMARY, BORDER);
    let rows = [
        (if fr { "Code" } else { "Code" }, c.code.clone()),
        ("ROM", c.rom.clone()),
        (if fr { "Sauvegarde" } else { "Save" }, c.save.clone()),
        (if fr { "Infos" } else { "Info" }, c.database.clone().unwrap_or_else(|| (if fr { "jeu inconnu" } else { "unknown game" }).into())),
        (if fr { "En-tête" } else { "Header" }, (if c.header_ok { "OK" } else if fr { "invalide" } else { "invalid" }).into()),
    ];
    for (i, (k, val)) in rows.iter().enumerate() {
        let y = 68 + i as i32 * 15;
        p.text(k, 10, false, DIM, x, y + 1, None);
        let color = if *k == "En-tête" || *k == "Header" { if c.header_ok { PRIMARY } else { ERROR } } else { TEXT };
        p.text(val, 11, false, color, x + 78, y, Some(w - 78));
    }
}

fn actions(p: &mut Painter, v: &View) {
    let fr = v.lang == Lang::Fr;
    if let Some(pr) = v.progress {
        p.text(&pr.label, 12, true, TEXT, 14, 54, Some(SCREEN_W - 28));
        let (bx, by, bw, bh) = (14, 78, SCREEN_W - 28, 14);
        p.canvas.bordered(bx, by, bw, bh, BUTTON, 2, CREAM);
        let pct = if pr.total == 0 { 0 } else { (pr.done * 100 / pr.total) as i32 };
        p.rect(bx + 2, by + 2, (bw - 4) * pct / 100, bh - 4, ACCENT);
        let eta = if pr.kib_s > 0.5 { ((pr.total - pr.done.min(pr.total)) as f32 / 1024.0 / pr.kib_s) as u32 } else { 0 };
        let line = format!("{pct}%   {} / {} KiB   {:.0} KiB/s   {}:{:02}", pr.done / 1024, pr.total / 1024, pr.kib_s, eta / 60, eta % 60);
        p.text(&line, 10, false, DIM, 14, 98, Some(SCREEN_W - 28));
        return;
    }
    for (i, a) in Action::ALL.iter().enumerate() {
        let y = 48 + i as i32 * 24;
        let sel = i == v.action_sel;
        if sel {
            p.rect(10, y, SCREEN_W - 20, 22, SELECTED_ROW);
            p.cursor(12, y + 6);
        }
        p.text(a.label(v.lang), 12, sel, if sel { TEXT } else { DIM }, 26, y + 3, None);
        if a.writes() {
            p.text_right(if fr { "écrit" } else { "writes" }, 9, false, if sel { ERROR } else { BORDER }, 16, y + 6);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Canvas, Fonts};

    #[test]
    fn tabs_cycle() {
        assert_eq!(Tab::Info.next(), Tab::Actions);
        assert_eq!(Tab::Info.prev(), Tab::Tools);
        assert_eq!(Tab::Tools.next(), Tab::Info);
    }

    #[test]
    fn only_the_two_write_actions_are_marked_as_writing() {
        assert!(!Action::ReadRom.writes() && !Action::ReadSave.writes());
        assert!(Action::WriteSave.writes() && Action::FlashRom.writes());
    }

    #[test]
    fn every_tab_renders_when_fonts_exist() {
        let Ok(fonts) = Fonts::load(Lang::Fr) else { return }; // fonts are the device's: skip elsewhere
        let mut canvas = Canvas::new(SCREEN_W, SCREEN_H, 0);
        let reader = ReaderView { name: "GBFlash".into(), firmware: "L15".into(), voltage: "3.3V" };
        let cart = CartView {
            system: "GBA", title: "MR. DRILLER2".into(), code: "AGB-AD2P".into(), rom: "4 MiB".into(),
            save: "256K SRAM".into(), database: Some("Mr. Driller 2 (Europe)".into()), header_ok: true,
        };
        for tab in Tab::ALL {
            let mut p = Painter { canvas: &mut canvas, fonts: &fonts };
            render(&mut p, &View { analysis: None, analysis_scroll: 0, analysing: false, overlay: None, lang: Lang::Fr, tab, reader: Some(&reader), cart: Some(&cart), scanning: false, action_sel: 0, progress: None, banner: None });
        }
        assert!(canvas.get(0, 0).is_some());
    }
}
