//! ZeroCart for the CardputerZero.
//!   zerocart-app [--db DIR] [--out DIR] [--lang fr|en] [--png OUT.png --tab info|actions|tools --reader gbflash|chis]
//! `--png` renders a demo frame to a file and exits (no display, no reader): used to check the screens.
mod app;
mod fb;
mod readers;
mod input;
mod screenshot;
mod share;
mod worker;

use app::App;
use zerocart_ui::{CartView, ProgressView, ReaderView, Tab, View};
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::mpsc::channel;
use std::sync::Arc;
use std::time::{Duration, Instant};
use input::{KeyRouter, Keypad};
use zerocart_ui::widgets::Painter;
use zerocart_ui::{Canvas, Fonts, Lang, SCREEN_H, SCREEN_W};

fn arg(args: &[String], name: &str) -> Option<String> {
    args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).cloned()
}

fn parse_key(s: &str) -> Option<input::Key> {
    use input::Key;
    Some(match s {
        "Right" => Key::Right,
        "Left" => Key::Left,
        "Up" => Key::Up,
        "Down" => Key::Down,
        "Enter" => Key::Enter,
        "Esc" => Key::Esc,
        c if c.chars().count() == 1 => Key::Char(c.chars().next()?),
        _ => return None,
    })
}

fn find_db(args: &[String]) -> PathBuf {
    if let Some(a) = arg(args, "--db") {
        return a.into();
    }
    for c in ["/usr/share/zerocart/configs", "configs"] {
        if std::path::Path::new(c).join("db_AGB.json").exists() {
            return c.into();
        }
    }
    "/usr/share/zerocart/configs".into()
}

/// Test aid: `--png out.png --share devices|none|files|confirm|input|sending|waiting|incoming|receiving` draws a sharing screen.
fn demo_share(which: &str, out: &str, lang: Lang, fonts: &Fonts) -> Result<(), String> {
    use zerocart_ui::share_screen::{render_share, ShareBody, ShareRow};
    let mut canvas = Canvas::new(SCREEN_W, SCREEN_H, 0);
    let mut p = Painter { canvas: &mut canvas, fonts };
    let rows = vec![
        ShareRow { name: "Pixel 8".into(), detail: "192.168.1.20  Pixel 8".into() },
        ShareRow { name: "Portable de Laurent".into(), detail: "192.168.1.21  desktop".into() },
    ];
    let pr = zerocart_ui::ProgressView { label: "MEGA PANG.bin".into(), done: 600 * 1024, total: 1024 * 1024, kib_s: 0.0 };
    let files: Vec<String> = ["MEGA PANG.bin  (1.0 Mo)", "Mr. Driller 2.gba  (4.0 Mo)", "backups/Mr. Driller 2 (non vérifiée).sav  (32 Ko)"].iter().map(|s| s.to_string()).collect();
    let confirm = vec!["Envoyer ce fichier à Pixel 8 ?".to_string(), "MEGA PANG.bin (1.0 Mo)".to_string(), "Le fichier quitte l'appareil et passe par le réseau local.".to_string()];
    let incoming = vec!["Pixel 8 veut vous envoyer :".to_string(), "rom.gba (4.0 Mo)".to_string(), "Enregistré dans ~/Downloads.".to_string()];
    let none: &[ShareRow] = &[];
    let body = match which {
        "none" => ShareBody::Devices { rows: none, selected: 0, discovering: true, port: 53317, receiving: None },
        "receiving" => ShareBody::Devices { rows: &rows, selected: 0, discovering: true, port: 53317, receiving: Some(&pr) },
        "files" => ShareBody::Files { to: "Pixel 8", items: &files, selected: 1 },
        "confirm" => ShareBody::Confirm { lines: &confirm },
        "input" => ShareBody::Input { text: "100.64.0.7", busy: false },
        "sending" => ShareBody::Sending { to: "Pixel 8", progress: &pr, waiting: false },
        "waiting" => ShareBody::Sending { to: "Pixel 8", progress: &pr, waiting: true },
        "incoming" => ShareBody::Incoming { lines: &incoming },
        _ => ShareBody::Devices { rows: &rows, selected: 1, discovering: true, port: 53317, receiving: None },
    };
    render_share(&mut p, lang, "ZeroCart", &body, None);
    screenshot::write_png(std::path::Path::new(out), &canvas.to_rgba(), SCREEN_W as u32, SCREEN_H as u32).map_err(|e| e.to_string())
}

fn demo_png(args: &[String], out: &str, lang: Lang, fonts: &Fonts) -> Result<(), String> {
    if let Some(which) = arg(args, "--share") {
        return demo_share(&which, out, lang, fonts);
    }

    let tab = match arg(args, "--tab").as_deref() {
        Some("actions") => Tab::Actions,
        Some("tools") => Tab::Tools,
        _ => Tab::Info,
    };
    let reader = if arg(args, "--reader").as_deref() == Some("chis") {
        ReaderView { name: "Chisflash".into(), firmware: String::new(), voltage: "" }
    } else {
        ReaderView { name: "GBFlash".into(), firmware: "v1.3 L15".into(), voltage: "3.3V" }
    };
    let cart = CartView {
        system: "GBA", title: "MR. DRILLER2".into(), code: "AGB-AD2P".into(), rom: "4 MiB".into(),
        save: "256K SRAM/FRAM (32 KiB)".into(), database: Some("Mr. Driller 2 (Europe)".into()), header_ok: true,
    };
    let progress = args.iter().any(|a| a == "--progress").then(|| ProgressView {
        label: "Lecture ROM : MR. DRILLER2".into(), done: 2 << 20, total: 4 << 20, kib_s: 164.0,
    });
    let mut canvas = Canvas::new(SCREEN_W, SCREEN_H, 0);
    let mut p = Painter { canvas: &mut canvas, fonts };
    // the start screens
    {
        use zerocart_ui::screens::{self, ReaderRow};
        use zerocart_ui::System;
        let sel: usize = arg(args, "--select").and_then(|s| s.parse().ok()).unwrap_or(0);
        let rows = vec![
            ReaderRow { name: "GBxCart RW / GBFlash".into(), detail: "/dev/ttyUSB0".into(), present: true },
            ReaderRow { name: "Chisflash".into(), detail: "USB 0483:0721".into(), present: false },
        ];
        let done = match arg(args, "--screen").as_deref() {
            Some("splash") => { screens::render_splash(&mut p, lang, env!("CARGO_PKG_VERSION")); true }
            Some("system") => { screens::render_system(&mut p, lang, sel, None); true }
            Some("readers") => { screens::render_readers(&mut p, lang, System::Gba, &rows, sel, false, None); true }
            Some("readers-none") => { screens::render_readers(&mut p, lang, System::Gb, &[rows[0].clone_absent(), rows[1].clone_absent()], 0, false, None); true }
            Some("placeholder") => {
                screens::render_placeholder(&mut p, lang, System::MegaDrive, &["Mega Drive : bientôt dans l'application", "Le lecteur Krikzz répond.", "Utilisez zerocart-md en ligne de commande."]);
                true
            }
            _ => false,
        };
        if done {
            return screenshot::write_png(std::path::Path::new(out), &canvas.to_rgba(), SCREEN_W as u32, SCREEN_H as u32)
                .map_err(|e| e.to_string());
        }
    }
    let items = vec!["REPUGNANTBOUNTY.gb  (4096 Ko)".to_string(), "Mr. Driller 2.gb  (4096 Ko)".to_string(), "test.gbc  (1024 Ko)".to_string()];
    let lines = vec![
        "ATTENTION : la ROM de la cartouche va être effacée".to_string(),
        "Écrire : REPUGNANTBOUNTY.gb".to_string(),
        "L'ancienne ROM est d'abord sauvegardée dans le dossier backups. Ne pas débrancher pendant l'écriture.".to_string(),
    ];
    let overlay = match arg(args, "--overlay").as_deref() {
        Some("picker") => Some(zerocart_ui::Overlay::Picker { title: "ROM à écrire sur la cartouche :", items: &items, selected: 1 }),
        Some("confirm") => Some(zerocart_ui::Overlay::Confirm { lines: &lines }),
        _ => None,
    };
    let analysis: Vec<String> = [
        "Mode : GBA, 3,3 V",
        "Réponse [ AAA/A9] C1 00 CB 22 00 00 08 00",
        "Aucun profil exact. Profil probable :",
        "  Generic Flash Cartridge (AAA/A9)",
        "CFI : 8 Mo, 2.7 à 3.6 V",
        "Écriture par tampon : non",
        "Secteurs : 8 × 8 Ko + 127 × 64 Ko",
        "!Lignes de données croisées : D0↔D1",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    let with_analysis = arg(args, "--analysis").is_some();
    zerocart_ui::render(&mut p, &View {
        overlay, lang, tab, reader: Some(&reader), cart: Some(&cart), scanning: false, action_sel: 1,
        progress: progress.as_ref(), banner: None,
        analysis: with_analysis.then_some(analysis.as_slice()),
        analysis_scroll: arg(args, "--scroll").and_then(|s| s.parse().ok()).unwrap_or(0),
        analysing: false,
    });
    screenshot::write_png(std::path::Path::new(out), &canvas.to_rgba(), SCREEN_W as u32, SCREEN_H as u32)
        .map_err(|e| e.to_string())
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let lang = if arg(&args, "--lang").as_deref() == Some("en") { Lang::En } else { Lang::Fr };
    let fonts = match Fonts::load(lang) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("zerocart: {e}");
            std::process::exit(1);
        }
    };
    if let Some(out) = arg(&args, "--png") {
        if let Err(e) = demo_png(&args, &out, lang, &fonts) {
            eprintln!("zerocart: {e}");
            std::process::exit(1);
        }
        return;
    }
    let framebuffer = match fb::Framebuffer::open(SCREEN_W as usize, SCREEN_H as usize) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("zerocart: {e}");
            std::process::exit(1);
        }
    };
    let home = PathBuf::from(std::env::var_os("HOME").unwrap_or_else(|| "/tmp".into()));
    let out_dir = arg(&args, "--out").map(PathBuf::from).unwrap_or_else(|| home.join("zerocart"));
    let cancel = Arc::new(AtomicBool::new(false));
    let (cmd_tx, cmd_rx) = channel();
    let (ev_tx, ev_rx) = channel();
    let worker = worker::Worker {
        cancel: cancel.clone(),
        out_dir: out_dir.clone(),
        db_dir: find_db(&args),
    };
    let handle = std::thread::spawn(move || worker.run(cmd_rx, ev_tx));
    let mut app = App::new(lang, cmd_tx.clone(), cancel, out_dir);
    // Test aid: `--reader gbx|chis|krikzz --system gb|gbc|gba|md` skips the start screens.
    if let (Some(r), Some(sy)) = (arg(&args, "--reader"), arg(&args, "--system")) {
        let kind = match r.as_str() {
            "gbx" | "gbflash" => zerocart_core::ReaderKind::GbxCart,
            "chis" | "chisflash" => zerocart_core::ReaderKind::Chisflash,
            "krikzz" | "md" => zerocart_core::ReaderKind::KrikzzMd,
            other => {
                eprintln!("zerocart: unknown --reader '{other}' (gbx, chis, krikzz)");
                std::process::exit(2);
            }
        };
        let system = match sy.as_str() {
            "gb" => zerocart_ui::System::Gb,
            "gbc" => zerocart_ui::System::Gbc,
            "gba" => zerocart_ui::System::Gba,
            "md" => zerocart_ui::System::MegaDrive,
            other => {
                eprintln!("zerocart: unknown --system '{other}' (gb, gbc, gba, md)");
                std::process::exit(2);
            }
        };
        app.start_with(kind, system);
    }
    let mut keypads = Keypad::discover();
    let mut router = KeyRouter::default();
    let mut canvas = Canvas::new(SCREEN_W, SCREEN_H, 0);
    // Test aid: `--press Right,Enter` sends these keys once the first detection has finished, and `--exit-after-idle`
    // quits when a transfer ends (the APPLaunch keyboard is not reachable over ssh).
    let mut script: Vec<input::Key> = arg(&args, "--press")
        .map(|s| s.split(',').filter_map(parse_key).collect())
        .unwrap_or_default();
    script.reverse();
    let exit_when_done = args.iter().any(|a| a == "--exit-after-idle");
    let mut transfer_seen = false;
    while !app.want_quit {
        let now = Instant::now();
        app.tick(now);
        if !app.busy() && !script.is_empty() {
            let k = script.pop().unwrap();
            app.on_routed(input::Routed::Down(k));
        }
        transfer_seen |= app.progress.is_some();
        if exit_when_done && transfer_seen && app.progress.is_none() && app.banner.is_some() {
            if let Some((m, e)) = &app.banner {
                eprintln!("zerocart: {} {m}", if *e { "ERROR" } else { "OK" });
            }
            break;
        }
        for kp in &mut keypads {
            for ev in kp.poll() {
                for r in router.on_event(ev, now) {
                    app.on_routed(r);
                }
            }
        }
        while let Ok(e) = ev_rx.try_recv() {
            app.on_event(e);
        }
        if app.take_dirty() {
            let mut p = Painter { canvas: &mut canvas, fonts: &fonts };
            app.render(&mut p);
            if let Err(e) = framebuffer.present(&canvas) {
                eprintln!("zerocart: framebuffer: {e}");
            }
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let _ = cmd_tx.send(worker::Cmd::Quit);
    let _ = handle.join(); // powers the cartridge off before leaving
}
