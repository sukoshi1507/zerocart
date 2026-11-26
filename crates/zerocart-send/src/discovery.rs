// SPDX-License-Identifier: GPL-3.0-or-later
//! Finding the other devices: LocalSend announces itself by UDP multicast (224.0.0.167:53317). A device that hears an
//! announcement answers by `register` (HTTPS), or by a multicast answer when that fails.
use crate::identity::Identity;
use crate::protocol::{DeviceInfo, MULTICAST_GROUP, MULTICAST_PORT, PORT};
use crate::send::{register, Peer};
use socket2::{Domain, Protocol, Socket, Type};
use std::collections::HashMap;
use std::io;
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpStream, UdpSocket};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

/// The devices known, one per address and port: a device seen again with another fingerprint replaces the old entry.
pub struct Peers {
    own_fingerprint: String,
    map: Mutex<HashMap<String, (Peer, bool)>>,
}

impl Peers {
    pub fn new(own_fingerprint: &str) -> Self {
        Self { own_fingerprint: own_fingerprint.to_string(), map: Mutex::new(HashMap::new()) }
    }

    /// Adds or refreshes a device. `None` for this device itself. `manual`: added by hand (its certificate is not
    /// checked against a fingerprint it never announced).
    pub fn add(&self, info: DeviceInfo, ip: IpAddr, manual: bool) -> Option<Peer> {
        if info.fingerprint == self.own_fingerprint {
            return None;
        }
        let info = info.cleaned(&ip.to_string()); // what a device calls itself is shown on our screen
        let port = info.port.unwrap_or(PORT);
        let mut map = self.map.lock().unwrap();
        let was_manual = map.get(&info.fingerprint).is_some_and(|e| e.1);
        let mut manual = manual || was_manual;
        map.retain(|k, (p, m)| {
            let same_place = p.ip == ip && p.port() == port && *k != info.fingerprint;
            if same_place {
                manual = manual || *m;
            }
            !same_place
        });
        let peer = Peer { info: info.clone(), ip, verify: !manual };
        map.insert(info.fingerprint, (peer.clone(), manual));
        Some(peer)
    }

    pub fn list(&self) -> Vec<Peer> {
        let mut v: Vec<Peer> = self.map.lock().unwrap().values().map(|e| e.0.clone()).collect();
        v.sort_by_key(|a| (a.info.alias.to_lowercase(), a.ip));
        v
    }

    pub fn clear(&self) {
        self.map.lock().unwrap().clear();
    }
}

/// `[(interface, IPv4)]` of the interfaces that are up, from `ip -j -4 addr show up`; loopback excluded.
pub fn parse_ip_addresses(json: &str) -> Vec<(String, Ipv4Addr)> {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(json) else { return vec![] };
    let mut out = Vec::new();
    for link in v.as_array().into_iter().flatten() {
        let name = link["ifname"].as_str().unwrap_or("").to_string();
        for a in link["addr_info"].as_array().into_iter().flatten() {
            if a["family"] == "inet" {
                if let Some(ip) = a["local"].as_str().and_then(|s| s.parse::<Ipv4Addr>().ok()) {
                    if !ip.is_loopback() {
                        out.push((name.clone(), ip));
                    }
                }
            }
        }
    }
    out
}

/// `[(IPv4, prefix length)]` of the same interfaces (the size of the network, for the scan).
pub fn parse_ip_networks(json: &str) -> Vec<(String, Ipv4Addr, u8)> {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(json) else { return vec![] };
    let mut out = Vec::new();
    for link in v.as_array().into_iter().flatten() {
        let name = link["ifname"].as_str().unwrap_or("").to_string();
        for a in link["addr_info"].as_array().into_iter().flatten() {
            if a["family"] == "inet" {
                if let (Some(ip), Some(prefix)) = (a["local"].as_str().and_then(|s| s.parse::<Ipv4Addr>().ok()), a["prefixlen"].as_u64()) {
                    if !ip.is_loopback() {
                        out.push((name.clone(), ip, prefix.min(32) as u8));
                    }
                }
            }
        }
    }
    out
}

fn ip_json() -> String {
    let out = std::process::Command::new("ip").args(["-j", "-4", "addr", "show", "up"]).output();
    out.ok().map(|o| String::from_utf8_lossy(&o.stdout).into_owned()).unwrap_or_default()
}

fn is_lan_link(name: &str) -> bool {
    !["tailscale", "tun", "wg", "docker", "veth"].iter().any(|p| name.starts_with(p))
}

/// The addresses multicast can work on: no VPN or container links.
pub fn lan_addresses() -> Vec<Ipv4Addr> {
    parse_ip_addresses(&ip_json()).into_iter().filter(|(name, _)| is_lan_link(name)).map(|(_, ip)| ip).collect()
}

/// The other hosts to probe on the network of `ip`/`prefix`: its /24 at most (a bigger network is not walked), never
/// the address itself, the network address or the broadcast address.
pub fn scan_candidates(ip: Ipv4Addr, prefix: u8) -> Vec<Ipv4Addr> {
    let prefix = prefix.clamp(24, 30); // /24 at most: 254 hosts; a /31 or /32 has nobody to look for
    let mask = u32::MAX << (32 - prefix);
    let (base, own) = (u32::from(ip) & mask, u32::from(ip));
    let size = 1u32 << (32 - prefix);
    (1..size - 1).map(|i| base + i).filter(|a| *a != own).map(Ipv4Addr::from).collect()
}

/// Probes `hosts` for a LocalSend device: a quick TCP connect on `port`, then `register` (which also introduces this
/// device to it). Many probes run at once; each answer found is added to `peers`. This finds devices that do not hear
/// or send multicast (Wi-Fi power saving, some access points), the way LocalSend's own HTTP discovery does.
pub fn scan_hosts(hosts: Vec<IpAddr>, port: u16, me: &DeviceInfo, id: &Identity, peers: &Peers, on_change: &(dyn Fn() + Sync)) {
    let queue = Mutex::new(hosts);
    let workers = 16usize;
    std::thread::scope(|scope| {
        for _ in 0..workers {
            scope.spawn(|| loop {
                let Some(ip) = queue.lock().unwrap().pop() else { return };
                if TcpStream::connect_timeout(&SocketAddr::new(ip, port), Duration::from_millis(300)).is_err() {
                    continue;
                }
                for protocol in ["https", "http"] {
                    let mut info = me.clone();
                    info.port = Some(port);
                    info.protocol = Some(protocol.to_string());
                    let probe = Peer { info, ip, verify: false };
                    if let Ok(theirs) = register(id, me, &probe) {
                        peers.add(DeviceInfo { port: Some(port), protocol: Some(protocol.to_string()), ..theirs }, ip, false);
                        on_change();
                        break;
                    }
                }
            });
        }
    });
}

/// The scan of every network this device is on.
pub fn scan_lan(me: &DeviceInfo, id: &Identity, peers: &Peers, on_change: &(dyn Fn() + Sync)) {
    let mut hosts: Vec<IpAddr> = Vec::new();
    for (name, ip, prefix) in parse_ip_networks(&ip_json()) {
        if is_lan_link(&name) {
            hosts.extend(scan_candidates(ip, prefix).into_iter().map(IpAddr::from));
        }
    }
    hosts.sort();
    hosts.dedup();
    scan_hosts(hosts, me.port.unwrap_or(PORT), me, id, peers, on_change);
}

/// An announcement (`announce` true) or an answer (false), with the port and protocol.
pub fn announcement(me: &DeviceInfo, announce: bool) -> Vec<u8> {
    let mut m = me.clone();
    m.announce = Some(announce);
    m.announcement = Some(announce);
    serde_json::to_vec(&m).expect("serialisable")
}

/// What to do with a received datagram: the sender is noted; an announcement from another device is to be answered.
/// Returns the peer to answer, if any.
pub fn handle_datagram(data: &[u8], from: IpAddr, peers: &Peers) -> Option<Peer> {
    let info: DeviceInfo = serde_json::from_slice(data).ok()?;
    let announce = info.is_announcement();
    let peer = peers.add(info, from, false)?;
    announce.then_some(peer)
}

pub struct Discovery {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
    socket: Arc<UdpSocket>,
    me: DeviceInfo,
    id: Arc<Identity>,
    peers: Arc<Peers>,
    on_change: Arc<dyn Fn() + Send + Sync>,
    /// A scan is running (two at once would only repeat each other).
    scanning: Arc<AtomicBool>,
}

fn send_all(socket: &UdpSocket, me: &DeviceInfo, announce: bool) {
    let data = announcement(me, announce);
    let sock = socket2::SockRef::from(socket);
    for ip in lan_addresses() {
        if sock.set_multicast_if_v4(&ip).is_ok() {
            let _ = socket.send_to(&data, SocketAddr::new(MULTICAST_GROUP.into(), MULTICAST_PORT));
        }
    }
}

impl Discovery {
    /// Starts listening and announcing. `on_change` is called when the list of devices may have changed.
    pub fn start(me: DeviceInfo, id: Arc<Identity>, peers: Arc<Peers>, on_change: Arc<dyn Fn() + Send + Sync>) -> io::Result<Self> {
        let socket = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))?;
        socket.set_reuse_address(true)?;
        #[cfg(unix)]
        socket.set_reuse_port(true)?;
        socket.bind(&SocketAddr::new(Ipv4Addr::UNSPECIFIED.into(), MULTICAST_PORT).into())?;
        socket.set_multicast_ttl_v4(1)?;
        for ip in lan_addresses() {
            let _ = socket.join_multicast_v4(&MULTICAST_GROUP, &ip); // an interface without multicast is skipped
        }
        let socket: UdpSocket = socket.into();
        socket.set_read_timeout(Some(Duration::from_millis(500)))?;
        let socket = Arc::new(socket);
        let stop = Arc::new(AtomicBool::new(false));
        let (flag, sock, me2) = (stop.clone(), socket.clone(), me.clone());
        let (id_keep, peers_keep, change_keep) = (id.clone(), peers.clone(), on_change.clone());
        let thread = std::thread::spawn(move || {
            let mut buf = vec![0u8; 65535];
            while !flag.load(Ordering::SeqCst) {
                let Ok((n, from)) = sock.recv_from(&mut buf) else { continue };
                let Some(peer) = handle_datagram(&buf[..n], from.ip(), &peers) else {
                    on_change();
                    continue;
                };
                on_change();
                let (id, me, peers, sock, on_change) = (id.clone(), me2.clone(), peers.clone(), sock.clone(), on_change.clone());
                std::thread::spawn(move || match register(&id, &me, &peer) {
                    Ok(info) => {
                        peers.add(DeviceInfo { port: peer.info.port, protocol: peer.info.protocol.clone(), ..info }, peer.ip, false);
                        on_change();
                    }
                    Err(_) => send_all(&sock, &me, false), // could not register: answer by multicast
                });
            }
        });
        Ok(Self { stop, thread: Some(thread), socket, me, id: id_keep, peers: peers_keep, on_change: change_keep, scanning: Arc::new(AtomicBool::new(false)) })
    }

    /// Announces this device a few times, in case one datagram is lost, and looks for the devices that did not hear it
    /// by probing the network (see [`scan_hosts`]).
    pub fn announce(&self) {
        let (sock, me) = (self.socket.clone(), self.me.clone());
        std::thread::spawn(move || {
            for delay in [0u64, 500, 1500] {
                std::thread::sleep(Duration::from_millis(delay));
                send_all(&sock, &me, true);
            }
        });
        if !self.scanning.swap(true, Ordering::SeqCst) {
            let (me, id, peers, on_change, scanning, stop) = (self.me.clone(), self.id.clone(), self.peers.clone(), self.on_change.clone(), self.scanning.clone(), self.stop.clone());
            std::thread::spawn(move || {
                if !stop.load(Ordering::SeqCst) {
                    scan_lan(&me, &id, &peers, &*on_change);
                }
                scanning.store(false, Ordering::SeqCst);
            });
        }
    }

    pub fn stop(mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

impl Drop for Discovery {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn device(alias: &str, fp: &str, port: u16) -> DeviceInfo {
        let mut d = DeviceInfo::this_device(alias, fp, true);
        d.port = Some(port);
        d
    }

    #[test]
    fn this_device_is_never_listed_and_a_new_fingerprint_at_the_same_place_replaces_the_old_one() {
        let peers = Peers::new("ME");
        let ip: IpAddr = "192.168.1.5".parse().unwrap();
        assert!(peers.add(device("Me", "ME", 53317), ip, false).is_none());
        peers.add(device("Phone", "A1", 53317), ip, false).unwrap();
        peers.add(device("Laptop", "B2", 53317), "192.168.1.6".parse().unwrap(), false).unwrap();
        assert_eq!(peers.list().iter().map(|p| p.info.alias.as_str()).collect::<Vec<_>>(), ["Laptop", "Phone"], "sorted by name");
        // the phone was reinstalled: same address, new fingerprint
        peers.add(device("Phone", "A9", 53317), ip, false).unwrap();
        let list = peers.list();
        assert_eq!(list.len(), 2);
        assert!(list.iter().any(|p| p.info.fingerprint == "A9") && !list.iter().any(|p| p.info.fingerprint == "A1"));
        // another port on the same address is another device
        peers.add(device("Second", "C3", 40000), ip, false).unwrap();
        assert_eq!(peers.list().len(), 3);
    }

    #[test]
    fn what_a_device_calls_itself_is_cleaned_before_it_is_listed() {
        let peers = Peers::new("ME");
        let ip: IpAddr = "192.168.1.30".parse().unwrap();
        let p = peers.add(device("Evil\u{1b}[2J\nname", "E1", 53317), ip, false).unwrap();
        assert_eq!(p.info.alias, "Evil [2J name");
        let q = peers.add(device("   ", "E2", 40000), ip, false).unwrap();
        assert_eq!(q.info.alias, "192.168.1.30");
    }

    #[test]
    fn a_device_added_by_hand_is_not_verified_and_stays_so_when_it_announces_itself_later() {
        let peers = Peers::new("ME");
        let ip: IpAddr = "100.64.0.7".parse().unwrap();
        let p = peers.add(device("Tailnet PC", "T1", 53317), ip, true).unwrap();
        assert!(!p.verify);
        let again = peers.add(device("Tailnet PC", "T1", 53317), ip, false).unwrap();
        assert!(!again.verify, "manual stays manual");
        let found = peers.add(device("Phone", "P1", 53317), "192.168.1.9".parse().unwrap(), false).unwrap();
        assert!(found.verify, "a device that announced itself is checked against its fingerprint");
        peers.clear();
        assert!(peers.list().is_empty());
    }

    #[test]
    fn announcements_carry_both_flags_and_an_answer_does_not_ask_for_an_answer() {
        let me = device("ZeroCart", "ME", 53317);
        let a: serde_json::Value = serde_json::from_slice(&announcement(&me, true)).unwrap();
        assert_eq!((a["announce"].as_bool(), a["announcement"].as_bool()), (Some(true), Some(true)));
        assert_eq!(a["port"], 53317);
        let r: DeviceInfo = serde_json::from_slice(&announcement(&me, false)).unwrap();
        assert!(!r.is_announcement());
    }

    #[test]
    fn a_datagram_is_noted_and_only_an_announcement_from_another_device_is_answered() {
        let peers = Peers::new("ME");
        let ip: IpAddr = "192.168.1.20".parse().unwrap();
        let from_phone = announcement(&device("Phone", "P1", 53317), true);
        let peer = handle_datagram(&from_phone, ip, &peers).expect("an announcement is answered");
        assert_eq!(peer.info.alias, "Phone");
        assert_eq!(peers.list().len(), 1);
        // an answer is noted, not answered again (no endless ping-pong)
        let answer = announcement(&device("Laptop", "L1", 53317), false);
        assert!(handle_datagram(&answer, "192.168.1.21".parse().unwrap(), &peers).is_none());
        assert_eq!(peers.list().len(), 2);
        // our own announcement coming back, garbage, and an empty datagram
        assert!(handle_datagram(&announcement(&device("Me", "ME", 53317), true), ip, &peers).is_none());
        assert!(handle_datagram(b"not json", ip, &peers).is_none());
        assert!(handle_datagram(b"", ip, &peers).is_none());
        assert_eq!(peers.list().len(), 2);
    }

    #[test]
    fn the_hosts_to_probe_are_the_24_around_this_address_without_itself() {
        let h = scan_candidates("192.168.1.10".parse().unwrap(), 24);
        assert_eq!(h.len(), 253, "254 hosts minus this one");
        assert!(!h.contains(&"192.168.1.10".parse().unwrap()) && !h.contains(&"192.168.1.0".parse().unwrap()) && !h.contains(&"192.168.1.255".parse().unwrap()));
        assert_eq!((h.first(), h.last()), (Some(&"192.168.1.1".parse().unwrap()), Some(&"192.168.1.254".parse().unwrap())));
        // a big network is not walked: only the /24 of the address
        assert_eq!(scan_candidates("10.1.2.3".parse().unwrap(), 8).len(), 253);
        // a small one is walked whole
        let small = scan_candidates("192.168.1.130".parse().unwrap(), 26);
        assert_eq!(small.len(), 61);
        assert!(small.iter().all(|a| u32::from(*a) >> 6 == u32::from(Ipv4Addr::new(192, 168, 1, 130)) >> 6));
        assert!(scan_candidates("192.168.1.5".parse().unwrap(), 32).len() <= 253);
    }

    #[test]
    fn prefix_lengths_are_read_from_ip_json() {
        let json = r#"[{"ifname":"wlan0","addr_info":[{"family":"inet","local":"192.168.1.10","prefixlen":24}]},{"ifname":"lo","addr_info":[{"family":"inet","local":"127.0.0.1","prefixlen":8}]}]"#;
        assert_eq!(parse_ip_networks(json), vec![("wlan0".to_string(), "192.168.1.10".parse().unwrap(), 24)]);
    }

    #[test]
    fn interface_addresses_are_read_from_ip_json_without_loopback() {
        let json = r#"[{"ifname":"lo","addr_info":[{"family":"inet","local":"127.0.0.1"}]},
            {"ifname":"eth0","addr_info":[{"family":"inet","local":"192.168.1.10"},{"family":"inet6","local":"fe80::1"}]},
            {"ifname":"tailscale0","addr_info":[{"family":"inet","local":"100.64.0.2"}]}]"#;
        assert_eq!(
            parse_ip_addresses(json),
            vec![("eth0".to_string(), "192.168.1.10".parse().unwrap()), ("tailscale0".to_string(), "100.64.0.2".parse().unwrap())]
        );
        assert!(parse_ip_addresses("garbage").is_empty());
        assert!(parse_ip_addresses("[]").is_empty());
    }
}
