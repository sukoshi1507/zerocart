// SPDX-License-Identifier: GPL-3.0-or-later
//! `zerocart-share`: ZeroSend (LocalSend v2) without the screen, to test it and to script it.
//!
//!   zerocart-share --list [--wait SECONDS]                 the devices found by multicast
//!   zerocart-share --send TARGET FILE...                   send files; TARGET = a name from --list, an address or host[:port]
//!   zerocart-share --text TARGET "message"                 send a text message
//!   zerocart-share --serve [--accept-all] [--for SECONDS]  receive; asks on the terminal unless --accept-all
//!   common: --downloads DIR  --state DIR  --bind ADDR:PORT  --alias NAME
//!
//! `--accept-all` skips the question: it is for tests on a network you trust. The application never does that.
use std::io::{BufRead, Write};
use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use zerocart_send::protocol::{DeviceInfo, FileMeta};
use zerocart_send::receive::Ui;
use zerocart_send::send::{Cancel, Outcome, Outgoing, Peer, SendEvent};
use zerocart_send::service::{Config, Service};

fn arg(args: &[String], name: &str) -> Option<String> {
    args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).cloned()
}

struct Terminal {
    accept_all: bool,
    last_report: AtomicU64,
}

impl Ui for Terminal {
    fn ask(&self, from: &DeviceInfo, ip: IpAddr, files: &[FileMeta]) -> bool {
        println!("request from {} ({ip}, fingerprint {}):", from.alias, from.fingerprint.chars().take(8).collect::<String>());
        for f in files {
            println!("  {} ({} bytes)", f.file_name, f.size);
        }
        if self.accept_all {
            println!("  accepted (--accept-all)");
            return true;
        }
        print!("accept? [y/N] ");
        let _ = std::io::stdout().flush();
        let mut line = String::new();
        std::io::stdin().lock().read_line(&mut line).is_ok() && line.trim().eq_ignore_ascii_case("y")
    }
    fn message(&self, from: &DeviceInfo, text: &str) {
        println!("message from {}: {text}", from.alias);
    }
    fn progress(&self, file: &str, done: u64, total: u64) {
        let step = done * 10 / total.max(1);
        if self.last_report.swap(step, Ordering::SeqCst) != step {
            println!("  receiving {file}: {}%", done * 100 / total.max(1));
        }
    }
    fn saved(&self, path: &std::path::Path) {
        println!("saved {}", path.display());
    }
}

fn run() -> Result<(), String> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let home = PathBuf::from(std::env::var_os("HOME").unwrap_or_else(|| "/tmp".into()));
    let alias = arg(&args, "--alias").unwrap_or_else(|| "ZeroCart".into());
    let mut config = Config::new(
        &alias,
        arg(&args, "--state").map(PathBuf::from).unwrap_or_else(|| home.join(".config/zerocart/send")),
        arg(&args, "--downloads").map(PathBuf::from).unwrap_or_else(|| home.join("Downloads")),
    );
    if let Some(b) = arg(&args, "--bind") {
        config.bind = b.parse::<SocketAddr>().map_err(|e| format!("--bind: {e}"))?;
    }
    let serve = args.iter().any(|a| a == "--serve");
    let list = args.iter().any(|a| a == "--list");
    let send = args.iter().position(|a| a == "--send");
    let text = args.iter().position(|a| a == "--text");
    if !(serve || list || send.is_some() || text.is_some()) {
        println!("usage: zerocart-share --list | --send TARGET FILE... | --text TARGET MESSAGE | --serve [--accept-all] [--for SECONDS]");
        return Ok(());
    }
    let ui = Arc::new(Terminal { accept_all: args.iter().any(|a| a == "--accept-all"), last_report: AtomicU64::new(99) });
    let service = Service::start(config, ui, Arc::new(|| {})).map_err(|e| format!("cannot start: {e}"))?;
    println!("{} on port {}, fingerprint {}, discovery {}", service.alias(), service.port(), service.fingerprint(), if service.discovering() { "on" } else { "off" });

    if serve {
        let limit = arg(&args, "--for").and_then(|s| s.parse::<u64>().ok());
        let started = Instant::now();
        println!("receiving... (Ctrl-C to stop)");
        while limit.is_none_or(|l| started.elapsed() < Duration::from_secs(l)) {
            std::thread::sleep(Duration::from_millis(200));
        }
        service.stop();
        return Ok(());
    }
    if list {
        let wait = arg(&args, "--wait").and_then(|s| s.parse::<u64>().ok()).unwrap_or(4);
        std::thread::sleep(Duration::from_secs(wait));
        for p in service.peers() {
            println!("{}  {}:{}  {}  {}", p.info.alias, p.ip, p.port(), p.info.device_model.clone().unwrap_or_default(), p.info.fingerprint.chars().take(8).collect::<String>());
        }
        service.stop();
        return Ok(());
    }
    let at = send.or(text).unwrap();
    let target = args.get(at + 1).ok_or("a TARGET is needed")?.clone();
    // a name from the list: wait for the announcements (a few seconds on Wi-Fi); an address is tried at once
    let looks_like_address = target.chars().next().is_some_and(|c| c.is_ascii_digit()) || target.contains(':');
    let waited = Instant::now();
    let found = loop {
        if let Some(p) = service.peers().into_iter().find(|p| p.info.alias == target) {
            break Some(p);
        }
        if looks_like_address || waited.elapsed() > Duration::from_secs(12) {
            break None;
        }
        std::thread::sleep(Duration::from_millis(250));
    };
    let peer: Peer = match found {
        Some(p) => p,
        None if looks_like_address => service.add_manual(&target)?,
        None => return Err(format!("no device named '{target}' found in 12 s (is LocalSend open on it?)")),
    };
    println!("target: {} ({}:{})", peer.info.alias, peer.ip, peer.port());
    let files: Vec<Outgoing> = if send.is_some() {
        args[at + 2..].iter().take_while(|a| !a.starts_with("--")).map(|f| Outgoing::from_path(std::path::Path::new(f)).map_err(|e| format!("{f}: {e}"))).collect::<Result<_, _>>()?
    } else {
        vec![Outgoing::text(args.get(at + 2).ok_or("a message is needed")?)]
    };
    let mut last = 99;
    let started = Instant::now();
    let outcome = service.send(&peer, files, &mut |e| match e {
        SendEvent::Waiting => println!("waiting for the other device to accept..."),
        SendEvent::Uploading { file, done, total, .. } => {
            let step = done * 10 / total.max(1);
            if step != last {
                last = step;
                println!("  sending {file}: {}%", done * 100 / total.max(1));
            }
        }
    }, &Cancel::new());
    service.stop();
    match outcome? {
        Outcome::Sent { files } => println!("sent {files} file(s) in {:.1} s", started.elapsed().as_secs_f32()),
        Outcome::MessageDelivered => println!("message delivered"),
        Outcome::Declined => return Err("declined by the other device".into()),
        Outcome::Busy => return Err("the other device is busy".into()),
        Outcome::PinRequired => return Err("the other device asks for a PIN (not supported)".into()),
    }
    Ok(())
}

fn main() {
    if let Err(e) = run() {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}
