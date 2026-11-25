// SPDX-License-Identifier: GPL-3.0-or-later
//! TLS for LocalSend: every device has a self-signed certificate, so nothing is checked against a certificate
//! authority. The server presents the device's certificate. The client presents it too (LocalSend 1.18+ asks for a
//! client certificate) and accepts the server's certificate only if its SHA-256 is the fingerprint the server announced
//! (when that is known).
use crate::identity::{fingerprint, Identity};
use crate::receive::{Receiver, Ui};
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::{ring, verify_tls12_signature, verify_tls13_signature, CryptoProvider};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer, ServerName, UnixTime};
use rustls::{ClientConfig, DigitallySignedStruct, ServerConfig, SignatureScheme};
use std::io::{self, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Duration;

/// A connection the protocol runs over: TLS or plain TCP.
pub trait Stream: Read + Write + Send {}
impl<T: Read + Write + Send> Stream for T {}

fn provider() -> Arc<CryptoProvider> {
    Arc::new(ring::default_provider())
}

fn cert_and_key(id: &Identity) -> (CertificateDer<'static>, PrivateKeyDer<'static>) {
    (CertificateDer::from(id.cert_der.clone()), PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(id.key_der.clone())))
}

pub fn server_config(id: &Identity) -> Result<Arc<ServerConfig>, String> {
    let (cert, key) = cert_and_key(id);
    let config = ServerConfig::builder_with_provider(provider())
        .with_safe_default_protocol_versions()
        .map_err(|e| e.to_string())?
        .with_no_client_auth() // the client's certificate is not checked: access is the user's accept on the screen
        .with_single_cert(vec![cert], key)
        .map_err(|e| e.to_string())?;
    Ok(Arc::new(config))
}

#[derive(Debug)]
struct FingerprintVerifier {
    expected: Option<String>,
    provider: Arc<CryptoProvider>,
}

impl ServerCertVerifier for FingerprintVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        if let Some(expected) = &self.expected {
            if fingerprint(end_entity.as_ref()) != expected.to_ascii_uppercase() {
                return Err(rustls::Error::General("the certificate does not match the device's fingerprint".into()));
            }
        }
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(&self, message: &[u8], cert: &CertificateDer<'_>, dss: &DigitallySignedStruct) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls12_signature(message, cert, dss, &self.provider.signature_verification_algorithms)
    }

    fn verify_tls13_signature(&self, message: &[u8], cert: &CertificateDer<'_>, dss: &DigitallySignedStruct) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls13_signature(message, cert, dss, &self.provider.signature_verification_algorithms)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.provider.signature_verification_algorithms.supported_schemes()
    }
}

/// `expected`: the fingerprint the device announced; `None` for a device added by hand (trusted on first use).
pub fn client_config(id: &Identity, expected: Option<&str>) -> Result<Arc<ClientConfig>, String> {
    let (cert, key) = cert_and_key(id);
    let p = provider();
    let config = ClientConfig::builder_with_provider(p.clone())
        .with_safe_default_protocol_versions()
        .map_err(|e| e.to_string())?
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(FingerprintVerifier { expected: expected.map(str::to_string), provider: p }))
        .with_client_auth_cert(vec![cert], key)
        .map_err(|e| e.to_string())?;
    Ok(Arc::new(config))
}

/// Opens a TLS client connection over `tcp`.
pub fn wrap_client(config: Arc<ClientConfig>, tcp: TcpStream) -> io::Result<Box<dyn Stream>> {
    let name = ServerName::IpAddress(tcp.peer_addr()?.ip().into());
    let conn = rustls::ClientConnection::new(config, name).map_err(io::Error::other)?;
    Ok(Box::new(rustls::StreamOwned::new(conn, tcp)))
}

/// The running HTTPS server.
pub struct Server {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
    pub addr: SocketAddr,
}

impl Server {
    pub fn stop(mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
    }
}

/// Serves `receiver` over TLS on `bind`. Each connection has its own thread; the handshake is made there so that a
/// slow client cannot hold the others up.
pub fn serve<U: Ui + 'static>(receiver: Arc<Receiver<U>>, id: &Identity, bind: SocketAddr) -> io::Result<Server> {
    let config = server_config(id).map_err(io::Error::other)?;
    let listener = TcpListener::bind(bind)?;
    listener.set_nonblocking(true)?;
    let addr = listener.local_addr()?;
    let stop = Arc::new(AtomicBool::new(false));
    let flag = stop.clone();
    let thread = std::thread::spawn(move || {
        while !flag.load(Ordering::SeqCst) {
            match listener.accept() {
                Ok((tcp, from)) => {
                    let (receiver, config) = (receiver.clone(), config.clone());
                    std::thread::spawn(move || {
                        let _ = tcp.set_nonblocking(false);
                        let _ = tcp.set_read_timeout(Some(Duration::from_secs(120)));
                        let _ = tcp.set_write_timeout(Some(Duration::from_secs(120)));
                        let Ok(conn) = rustls::ServerConnection::new(config) else { return };
                        let mut stream = rustls::StreamOwned::new(conn, tcp);
                        let _ = receiver.serve(&mut stream, from.ip());
                        stream.conn.send_close_notify();
                        let _ = stream.flush();
                    });
                }
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => std::thread::sleep(Duration::from_millis(50)),
                Err(_) => std::thread::sleep(Duration::from_millis(200)),
            }
        }
    });
    Ok(Server { stop, thread: Some(thread), addr })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{DeviceInfo, FileMeta};
    use crate::send::{send, Cancel, Outcome, Outgoing, Peer, SendEvent};
    use std::net::IpAddr;
    use std::path::{Path, PathBuf};
    use std::sync::mpsc::{channel, Receiver as Rx, Sender};
    use std::sync::Mutex;

    #[derive(Default)]
    struct Log {
        asked: Vec<String>,
        messages: Vec<String>,
        saved: Vec<PathBuf>,
    }

    /// The user at the other end: accepts or declines at once, or waits for `gate` (to test a cancel while waiting).
    struct FakeUi {
        accept: bool,
        log: Arc<Mutex<Log>>,
        gate: Mutex<Option<Rx<()>>>,
    }
    impl Ui for FakeUi {
        fn ask(&self, from: &DeviceInfo, _ip: IpAddr, _files: &[FileMeta]) -> bool {
            self.log.lock().unwrap().asked.push(from.alias.clone());
            if let Some(g) = self.gate.lock().unwrap().as_ref() {
                let _ = g.recv_timeout(Duration::from_secs(5));
            }
            self.accept
        }
        fn message(&self, _from: &DeviceInfo, text: &str) {
            self.log.lock().unwrap().messages.push(text.to_string());
        }
        fn saved(&self, path: &Path) {
            self.log.lock().unwrap().saved.push(path.to_path_buf());
        }
    }

    struct Setup {
        server: Server,
        server_id: Identity,
        client_id: Identity,
        me: DeviceInfo,
        log: Arc<Mutex<Log>>,
        dir: PathBuf,
        gate: Option<Sender<()>>,
    }

    fn setup(tag: &str, accept: bool, gated: bool) -> Setup {
        let dir = std::env::temp_dir().join(format!("zerocart-send-tls-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("downloads")).unwrap();
        let (server_id, client_id) = (Identity::generate().unwrap(), Identity::generate().unwrap());
        let log = Arc::new(Mutex::new(Log::default()));
        let (tx, rx) = channel();
        let ui = FakeUi { accept, log: log.clone(), gate: Mutex::new(gated.then_some(rx)) };
        let receiver = Arc::new(Receiver::new("Cardputer", &server_id.fingerprint, dir.join("downloads"), ui));
        let server = serve(receiver, &server_id, "127.0.0.1:0".parse().unwrap()).unwrap();
        let me = DeviceInfo::this_device("Sender", &client_id.fingerprint, true);
        Setup { server, server_id, client_id, me, log, dir, gate: gated.then_some(tx) }
    }

    impl Setup {
        fn peer(&self, verify: bool, fingerprint: Option<&str>) -> Peer {
            let mut info = DeviceInfo::this_device("Cardputer", fingerprint.unwrap_or(&self.server_id.fingerprint), true);
            info.port = Some(self.server.addr.port());
            Peer { info, ip: self.server.addr.ip(), verify }
        }
        fn finish(self) {
            let dir = self.dir.clone();
            self.server.stop();
            let _ = std::fs::remove_dir_all(dir);
        }
    }

    fn file(dir: &Path, name: &str, data: &[u8]) -> Outgoing {
        let p = dir.join(name);
        std::fs::write(&p, data).unwrap();
        Outgoing::from_path(&p).unwrap()
    }

    #[test]
    fn a_file_goes_over_real_tls_with_a_client_certificate_and_arrives_checked() {
        let s = setup("file", true, false);
        let data: Vec<u8> = (0..300_000u32).map(|i| (i * 31 + 7) as u8).collect();
        let f = file(&s.dir, "dump.bin", &data);
        assert_eq!(f.size, 300_000);
        assert_eq!(f.sha256.as_ref().unwrap().len(), 64);
        let mut events = Vec::new();
        let out = send(&s.client_id, &s.me, &s.peer(true, None), vec![f], &mut |e| events.push(e), &Cancel::new()).unwrap();
        assert_eq!(out, Outcome::Sent { files: 1 });
        assert_eq!(std::fs::read(s.dir.join("downloads/dump.bin")).unwrap(), data);
        assert_eq!(events.first(), Some(&SendEvent::Waiting));
        assert!(matches!(events.last(), Some(SendEvent::Uploading { done: 300_000, total: 300_000, index: 1, of: 1, .. })));
        assert_eq!(s.log.lock().unwrap().asked, vec!["Sender".to_string()]);
        s.finish();
    }

    #[test]
    fn several_files_arrive_in_order_and_a_text_is_a_message() {
        let s = setup("multi", true, false);
        let (a, b) = (file(&s.dir, "a.sav", b"save one"), file(&s.dir, "b.sav", b"save two"));
        let out = send(&s.client_id, &s.me, &s.peer(true, None), vec![a, b], &mut |_| {}, &Cancel::new()).unwrap();
        assert_eq!(out, Outcome::Sent { files: 2 });
        assert_eq!(std::fs::read(s.dir.join("downloads/a.sav")).unwrap(), b"save one");
        assert_eq!(std::fs::read(s.dir.join("downloads/b.sav")).unwrap(), b"save two");
        let out = send(&s.client_id, &s.me, &s.peer(true, None), vec![Outgoing::text("coucou")], &mut |_| {}, &Cancel::new()).unwrap();
        assert_eq!(out, Outcome::MessageDelivered);
        assert_eq!(s.log.lock().unwrap().messages, vec!["coucou".to_string()]);
        s.finish();
    }

    #[test]
    fn a_decline_is_reported_and_nothing_is_written() {
        let s = setup("decline", false, false);
        let f = file(&s.dir, "x.bin", b"data");
        let out = send(&s.client_id, &s.me, &s.peer(true, None), vec![f], &mut |_| {}, &Cancel::new()).unwrap();
        assert_eq!(out, Outcome::Declined);
        assert_eq!(std::fs::read_dir(s.dir.join("downloads")).unwrap().count(), 0);
        s.finish();
    }

    #[test]
    fn a_certificate_that_is_not_the_announced_one_is_refused_before_anything_is_sent() {
        let s = setup("fp", true, false);
        let f = file(&s.dir, "x.bin", b"data");
        let wrong = "0".repeat(64);
        let err = send(&s.client_id, &s.me, &s.peer(true, Some(&wrong)), vec![f.clone()], &mut |_| {}, &Cancel::new()).unwrap_err();
        assert!(err.contains("empreinte") || err.contains("certificat") || err.contains("fingerprint"), "{err}");
        assert!(s.log.lock().unwrap().asked.is_empty(), "the other user was never asked");
        // a device added by hand (no fingerprint to check) is trusted on first use
        let out = send(&s.client_id, &s.me, &s.peer(false, Some(&wrong)), vec![f], &mut |_| {}, &Cancel::new()).unwrap();
        assert_eq!(out, Outcome::Sent { files: 1 });
        s.finish();
    }

    #[test]
    fn a_cancel_while_the_other_user_is_deciding_stops_the_send() {
        let mut s = setup("cancel", true, true);
        let f = file(&s.dir, "x.bin", b"data");
        let cancel = Arc::new(Cancel::new());
        let c = cancel.clone();
        let t = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(300));
            c.cancel();
        });
        let started = std::time::Instant::now();
        let err = send(&s.client_id, &s.me, &s.peer(true, None), vec![f], &mut |_| {}, &cancel).unwrap_err();
        assert_eq!(err, "annulé");
        assert!(started.elapsed() < Duration::from_secs(4), "the cancel did not wait for the other user");
        t.join().unwrap();
        s.gate.take().unwrap().send(()).unwrap(); // let the server thread finish
        s.finish();
    }

    #[test]
    fn registering_returns_the_other_devices_description() {
        let s = setup("register", true, false);
        let info = crate::send::register(&s.client_id, &s.me, &s.peer(true, None)).unwrap();
        assert_eq!((info.alias.as_str(), info.fingerprint.as_str()), ("Cardputer", s.server_id.fingerprint.as_str()));
        assert!(info.port.is_none(), "the register answer has no port");
        s.finish();
    }

    #[test]
    fn nothing_listening_gives_a_clear_error() {
        let id = Identity::generate().unwrap();
        let me = DeviceInfo::this_device("Sender", &id.fingerprint, true);
        let mut info = DeviceInfo::this_device("Nobody", "AA", true);
        info.port = Some(1); // nothing listens there
        let peer = Peer { info, ip: "127.0.0.1".parse().unwrap(), verify: true };
        let err = send(&id, &me, &peer, vec![Outgoing::text("x")], &mut |_| {}, &Cancel::new()).unwrap_err();
        assert!(err.contains("refusée") || err.contains("réponse"), "{err}");
    }
}
