// SPDX-License-Identifier: GPL-3.0-or-later
//! The screens of the file sharing (ZeroSend): the devices around, the files to send, the confirmation, the progress and
//! the request of another device that wants to send here. Plain data in, pixels out.
use crate::style::*;
use crate::widgets::Painter;
use crate::{Lang, ProgressView, SCREEN_H, SCREEN_W};

/// A device that can be sent to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShareRow {
    pub name: String,
    /// Address and model.
    pub detail: String,
}

pub enum ShareBody<'a> {
    Devices { rows: &'a [ShareRow], selected: usize, discovering: bool, port: u16, receiving: Option<&'a ProgressView> },
    Files { to: &'a str, items: &'a [String], selected: usize },
    /// The first line is the warning, the others the details.
    Confirm { lines: &'a [String] },
    Input { text: &'a str, busy: bool },
    Sending { to: &'a str, progress: &'a ProgressView, waiting: bool },
    /// Another device wants to send here: Y accepts, N declines.
    Incoming { lines: &'a [String] },
}

fn bar(p: &mut Painter, lang: Lang, alias: &str) {
    let fr = lang == Lang::Fr;
    p.bar("ZEROSEND", &if fr { format!("visible sous : {alias}") } else { format!("visible as: {alias}") }, None);
}

fn progress_bar(p: &mut Painter, pr: &ProgressView, y: i32) {
    let (bx, bw, bh) = (14, SCREEN_W - 28, 12);
    p.canvas.bordered(bx, y, bw, bh, BUTTON, 2, CREAM);
    let pct = if pr.total == 0 { 0 } else { (pr.done.min(pr.total) * 100 / pr.total) as i32 };
    p.rect(bx + 2, y + 2, (bw - 4) * pct / 100, bh - 4, ACCENT);
    let line = format!("{pct}%   {} / {} KiB", pr.done / 1024, pr.total / 1024);
    p.text(&line, 10, false, DIM, bx, y + bh + 4, Some(bw));
}

pub fn render_share(p: &mut Painter, lang: Lang, alias: &str, body: &ShareBody, banner: Option<(&str, bool)>) {
    let fr = lang == Lang::Fr;
    p.canvas.fill_rect(0, 0, SCREEN_W, SCREEN_H, BACKGROUND);
    p.frame();
    bar(p, lang, alias);
    match body {
        ShareBody::Devices { rows, selected, discovering, port, receiving } => {
            let note = match (*discovering, fr) {
                (true, true) => format!("Port {port}. Les appareils LocalSend du réseau apparaissent ici."),
                (true, false) => format!("Port {port}. LocalSend devices on the network show up here."),
                (false, true) => format!("Port {port}. Détection automatique impossible : A ajoute un appareil par son adresse."),
                (false, false) => format!("Port {port}. Auto-discovery is not available: A adds a device by address."),
            };
            p.text(&note, 9, false, if *discovering { DIM } else { PRIMARY }, 14, 27, Some(SCREEN_W - 28));
            if let Some(r) = receiving {
                p.text(&r.label, 10, true, TEXT, 14, 44, Some(SCREEN_W - 28));
                progress_bar(p, r, 58);
            } else if rows.is_empty() {
                p.text(if fr { "Aucun appareil trouvé." } else { "No device found." }, 12, false, DIM, 14, 56, Some(SCREEN_W - 28));
                p.text(if fr { "Ouvrez LocalSend sur votre téléphone ou PC (même Wi-Fi)." } else { "Open LocalSend on your phone or PC (same Wi-Fi)." }, 9, false, DIM, 14, 74, Some(SCREEN_W - 28));
            } else {
                let first = selected.saturating_sub(3).min(rows.len().saturating_sub(4));
                for (i, r) in rows.iter().enumerate().skip(first).take(4) {
                    let y = 42 + (i - first) as i32 * 26;
                    let sel = i == *selected;
                    if sel {
                        p.rect(10, y, SCREEN_W - 20, 24, SELECTED_ROW);
                        p.cursor(12, y + 8);
                    }
                    p.text(&r.name, 11, sel, if sel { TEXT } else { DIM }, 26, y + 3, Some(SCREEN_W - 44));
                    p.text(&r.detail, 8, false, DIM, 26, y + 14, Some(SCREEN_W - 44));
                }
            }
            p.footer(
                &[
                    ("F X", if fr { "choisir" } else { "pick" }),
                    ("Enter", if fr { "envoyer" } else { "send" }),
                    ("A", if fr { "adresse" } else { "address" }),
                    ("R", if fr { "relire" } else { "rescan" }),
                    ("Esc", if fr { "retour" } else { "back" }),
                ],
                banner,
            );
        }
        ShareBody::Files { to, items, selected } => {
            p.text(&if fr { format!("Envoyer à {to} :") } else { format!("Send to {to}:") }, 11, true, TEXT, 14, 29, Some(SCREEN_W - 28));
            if items.is_empty() {
                p.text(if fr { "Aucun fichier dans ~/zerocart (dumps, sauvegardes)." } else { "No file in ~/zerocart (dumps, saves)." }, 10, false, DIM, 14, 56, Some(SCREEN_W - 28));
            }
            let first = selected.saturating_sub(3).min(items.len().saturating_sub(4));
            for (row, item) in items.iter().enumerate().skip(first).take(4) {
                let y = 46 + (row - first) as i32 * 21;
                let sel = row == *selected;
                if sel {
                    p.rect(10, y, SCREEN_W - 20, 19, SELECTED_ROW);
                    p.cursor(12, y + 5);
                }
                p.text(item, 11, sel, if sel { TEXT } else { DIM }, 26, y + 2, Some(SCREEN_W - 44));
            }
            p.footer(&[("F X", if fr { "choisir" } else { "pick" }), ("Enter", "ok"), ("Esc", if fr { "retour" } else { "back" })], banner);
        }
        ShareBody::Confirm { lines } => {
            let mut y = 28;
            for (i, l) in lines.iter().enumerate() {
                y += 1 + p.paragraph(l, 11, i == 0, if i == 0 { ERROR } else { TEXT }, 14, y, SCREEN_W - 28) as i32 * 14;
            }
            p.footer(&[("Y", if fr { "envoyer" } else { "send" }), ("Esc", if fr { "annuler" } else { "cancel" })], banner);
        }
        ShareBody::Input { text, busy } => {
            p.text(if fr { "Adresse de l'appareil (ex. 100.64.0.7 ou 192.168.1.20:53317) :" } else { "Device address (e.g. 100.64.0.7 or 192.168.1.20:53317):" }, 10, false, DIM, 14, 32, Some(SCREEN_W - 28));
            p.canvas.bordered(14, 56, SCREEN_W - 28, 22, BUTTON, 2, ACCENT);
            p.text(&format!("{text}_"), 12, false, TEXT, 20, 60, Some(SCREEN_W - 40));
            if *busy {
                p.text(if fr { "Connexion..." } else { "Connecting..." }, 10, false, PRIMARY, 14, 88, Some(SCREEN_W - 28));
            }
            p.footer(&[("Enter", if fr { "ajouter" } else { "add" }), ("Esc", if fr { "annuler" } else { "cancel" })], banner);
        }
        ShareBody::Sending { to, progress, waiting } => {
            p.text(&if fr { format!("Envoi à {to}") } else { format!("Sending to {to}") }, 12, true, TEXT, 14, 34, Some(SCREEN_W - 28));
            if *waiting {
                p.text(if fr { "En attente de la réponse de l'autre appareil..." } else { "Waiting for the other device to accept..." }, 10, false, PRIMARY, 14, 58, Some(SCREEN_W - 28));
            } else {
                p.text(&progress.label, 10, false, DIM, 14, 56, Some(SCREEN_W - 28));
                progress_bar(p, progress, 74);
            }
            p.footer(&[("Esc", if fr { "annuler" } else { "cancel" })], banner);
        }
        ShareBody::Incoming { lines } => {
            let mut y = 28;
            for (i, l) in lines.iter().enumerate() {
                y += 1 + p.paragraph(l, 11, i == 0, if i == 0 { PRIMARY } else { TEXT }, 14, y, SCREEN_W - 28) as i32 * 14;
            }
            p.footer(&[("Y", if fr { "accepter" } else { "accept" }), ("N", if fr { "refuser" } else { "decline" })], None);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Canvas, Fonts};

    #[test]
    fn every_share_screen_renders_when_fonts_exist() {
        let Ok(fonts) = Fonts::load(Lang::Fr) else { return };
        let mut canvas = Canvas::new(SCREEN_W, SCREEN_H, 0);
        let mut p = Painter { canvas: &mut canvas, fonts: &fonts };
        let rows = vec![
            ShareRow { name: "Pixel 8".into(), detail: "192.168.1.20  Pixel 8".into() },
            ShareRow { name: "Portable".into(), detail: "192.168.1.21  desktop".into() },
        ];
        let pr = ProgressView { label: "dump.bin".into(), done: 512 * 1024, total: 1024 * 1024, kib_s: 0.0 };
        let lines = vec!["Envoyer ce fichier ?".to_string(), "dump.bin (1024 Ko)".to_string()];
        for lang in [Lang::Fr, Lang::En] {
            render_share(&mut p, lang, "ZeroCart", &ShareBody::Devices { rows: &rows, selected: 1, discovering: true, port: 53317, receiving: None }, None);
            render_share(&mut p, lang, "ZeroCart", &ShareBody::Devices { rows: &[], selected: 0, discovering: false, port: 53317, receiving: Some(&pr) }, Some(("x", true)));
            render_share(&mut p, lang, "ZeroCart", &ShareBody::Files { to: "Pixel 8", items: &["a.bin".to_string()], selected: 0 }, None);
            render_share(&mut p, lang, "ZeroCart", &ShareBody::Files { to: "Pixel 8", items: &[], selected: 0 }, None);
            render_share(&mut p, lang, "ZeroCart", &ShareBody::Confirm { lines: &lines }, None);
            render_share(&mut p, lang, "ZeroCart", &ShareBody::Input { text: "100.64.0.7", busy: true }, None);
            render_share(&mut p, lang, "ZeroCart", &ShareBody::Sending { to: "Pixel 8", progress: &pr, waiting: false }, None);
            render_share(&mut p, lang, "ZeroCart", &ShareBody::Sending { to: "Pixel 8", progress: &pr, waiting: true }, None);
            render_share(&mut p, lang, "ZeroCart", &ShareBody::Incoming { lines: &lines }, None);
        }
        assert!(canvas.get(0, 0).is_some());
    }
}
