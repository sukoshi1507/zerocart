// SPDX-License-Identifier: GPL-3.0-or-later
//! The whole of ZeroSend in one object: identity, HTTPS server, discovery, the list of devices and sending.
use crate::discovery::{Discovery, Peers};
use crate::identity::Identity;
use crate::protocol::{DeviceInfo, FileMeta, PORT};
use crate::receive::{Receiver, Ui};
use crate::send::{register, send, Cancel, Outcome, Outgoing, Peer, SendEvent};
use crate::tls::{serve, Server};
use std::io;
use std::net::{IpAddr, SocketAddr, ToSocketAddrs};
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// A [`Ui`] shared with the application.
struct DynUi(Arc<dyn Ui>);

impl Ui for DynUi {
    fn ask(&self, from: &DeviceInfo, ip: IpAddr, files: &[FileMeta]) -> bool {
        self.0.ask(from, ip, files)
    }
    fn message(&self, from: &DeviceInfo, text: &str) {
        self.0.message(from, text)
    }
    fn peer_seen(&self, info: &DeviceInfo, ip: IpAddr) {
        self.0.peer_seen(info, ip)
    }
    fn progress(&self, file: &str, done: u64, total: u64) {
        self.0.progress(file, done, total)
    }
    fn saved(&self, path: &Path) {
        self.0.saved(path)
    }
}

pub struct Config {
    /// The name other devices see.
    pub alias: String,
    /// Where the certificate and key are kept.
    pub state_dir: PathBuf,
    /// Where received files go.
    pub downloads: PathBuf,
    /// Address and port of the HTTPS server (normally `0.0.0.0:53317`).
    pub bind: SocketAddr,
}

impl Config {
    pub fn new(alias: &str, state_dir: PathBuf, downloads: PathBuf) -> Self {
        Self { alias: alias.to_string(), state_dir, downloads, bind: SocketAddr::from(([0, 0, 0, 0], PORT)) }
    }
}

pub struct Service {
    identity: Arc<Identity>,
    me: DeviceInfo,
    peers: Arc<Peers>,
    server: Server,
    discovery: Option<Discovery>,
}

impl Service {
    /// Starts the server and the discovery. The discovery is optional: without multicast (a network that blocks it)
    /// devices can still be added by hand and receiving works. `on_change`: the list of devices may have changed.
    pub fn start(config: Config, ui: Arc<dyn Ui>, on_change: Arc<dyn Fn() + Send + Sync>) -> io::Result<Self> {
        let identity = Arc::new(Identity::load_or_create(&config.state_dir)?);
        std::fs::create_dir_all(&config.downloads)?;
        let receiver = Arc::new(Receiver::new(&config.alias, &identity.fingerprint, config.downloads.clone(), DynUi(ui)));
        let server = serve(receiver, &identity, config.bind)?;
        let mut me = DeviceInfo::this_device(&config.alias, &identity.fingerprint, true);
        me.port = Some(server.addr.port());
        let peers = Arc::new(Peers::new(&identity.fingerprint));
        let discovery = Discovery::start(me.clone(), identity.clone(), peers.clone(), on_change).ok();
        if let Some(d) = &discovery {
            d.announce();
        }
        Ok(Self { identity, me, peers, server, discovery })
    }

    pub fn alias(&self) -> &str {
        &self.me.alias
    }

    pub fn fingerprint(&self) -> &str {
        &self.identity.fingerprint
    }

    pub fn port(&self) -> u16 {
        self.server.addr.port()
    }

    /// Whether discovery by multicast is running.
    pub fn discovering(&self) -> bool {
        self.discovery.is_some()
    }

    pub fn peers(&self) -> Vec<Peer> {
        self.peers.list()
    }

    /// Announces this device again.
    pub fn rescan(&self) {
        if let Some(d) = &self.discovery {
            d.announce();
        }
    }

    /// Adds a device by address: `100.64.0.7`, `host` or `host:port` (for networks without multicast, such as a VPN).
    pub fn add_manual(&self, text: &str) -> Result<Peer, String> {
        let text = text.trim();
        let addr = if text.contains(':') { text.to_socket_addrs() } else { (text, PORT).to_socket_addrs() }
            .map_err(|_| format!("adresse invalide : {text}"))?
            .find(|a| a.is_ipv4())
            .ok_or_else(|| format!("adresse introuvable : {text}"))?;
        for https in [true, false] {
            let mut info = DeviceInfo::this_device(&addr.ip().to_string(), "", true);
            info.port = Some(addr.port());
            info.protocol = Some(if https { "https" } else { "http" }.into());
            let probe = Peer { info, ip: addr.ip(), verify: false };
            if let Ok(theirs) = register(&self.identity, &self.me, &probe) {
                let info = DeviceInfo { port: Some(addr.port()), protocol: probe.info.protocol.clone(), ..theirs };
                return self.peers.add(info, addr.ip(), true).ok_or_else(|| "c'est cet appareil".to_string());
            }
        }
        Err(format!("{addr} ne répond pas (LocalSend est-il ouvert ?)"))
    }

    pub fn send(&self, peer: &Peer, files: Vec<Outgoing>, progress: &mut dyn FnMut(SendEvent), cancel: &Cancel) -> Result<Outcome, String> {
        send(&self.identity, &self.me, peer, files, progress, cancel)
    }

    pub fn stop(self) {
        if let Some(d) = self.discovery {
            d.stop();
        }
        self.server.stop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Mutex;

    struct Accepting(Mutex<Vec<String>>);
    impl Ui for Accepting {
        fn ask(&self, _: &DeviceInfo, _: IpAddr, files: &[FileMeta]) -> bool {
            self.0.lock().unwrap().extend(files.iter().map(|f| f.file_name.clone()));
            true
        }
        fn message(&self, _: &DeviceInfo, _: &str) {}
    }

    fn start(tag: &str) -> (Service, PathBuf, Arc<Accepting>) {
        let dir = std::env::temp_dir().join(format!("zerocart-send-svc-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let ui = Arc::new(Accepting(Mutex::new(vec![])));
        let mut config = Config::new(&format!("ZeroCart {tag}"), dir.join("state"), dir.join("downloads"));
        config.bind = "127.0.0.1:0".parse().unwrap(); // a free port, loopback only
        let svc = Service::start(config, ui.clone(), Arc::new(|| {})).unwrap();
        (svc, dir, ui)
    }

    #[test]
    fn two_services_exchange_a_file_after_one_adds_the_other_by_address() {
        let (a, dir_a, _) = start("a");
        let (b, dir_b, ui_b) = start("b");
        assert_ne!(a.fingerprint(), b.fingerprint());
        let peer = a.add_manual(&format!("127.0.0.1:{}", b.port())).unwrap();
        assert_eq!(peer.info.alias, "ZeroCart b");
        assert!(!peer.verify, "added by hand");
        // multicast may also have shown B on its LAN address: what matters is that B is in the list
        assert!(a.peers().iter().any(|p| p.info.fingerprint == b.fingerprint()));
        let src = dir_a.join("save.sav");
        std::fs::write(&src, b"my save").unwrap();
        let events = AtomicUsize::new(0);
        let out = a
            .send(&peer, vec![Outgoing::from_path(&src).unwrap()], &mut |_| {
                events.fetch_add(1, Ordering::SeqCst);
            }, &Cancel::new())
            .unwrap();
        assert_eq!(out, Outcome::Sent { files: 1 });
        assert_eq!(std::fs::read(dir_b.join("downloads/save.sav")).unwrap(), b"my save");
        assert_eq!(*ui_b.0.lock().unwrap(), vec!["save.sav".to_string()]);
        assert!(events.load(Ordering::SeqCst) >= 2);
        a.stop();
        b.stop();
        let _ = std::fs::remove_dir_all(dir_a);
        let _ = std::fs::remove_dir_all(dir_b);
    }

    #[test]
    fn the_identity_survives_a_restart_and_a_bad_address_is_explained() {
        let (a, dir, _) = start("c");
        let fp = a.fingerprint().to_string();
        assert_eq!(a.alias(), "ZeroCart c");
        assert!(a.add_manual("not an address at all").is_err());
        let err = a.add_manual("127.0.0.1:1").unwrap_err();
        assert!(err.contains("ne répond pas"), "{err}");
        a.stop();
        let ui = Arc::new(Accepting(Mutex::new(vec![])));
        let mut config = Config::new("ZeroCart c", dir.join("state"), dir.join("downloads"));
        config.bind = "127.0.0.1:0".parse().unwrap();
        let again = Service::start(config, ui, Arc::new(|| {})).unwrap();
        assert_eq!(again.fingerprint(), fp, "the fingerprint does not change between starts");
        again.stop();
        let _ = std::fs::remove_dir_all(dir);
    }
}
