// SPDX-License-Identifier: GPL-3.0-or-later
//! The sending side of LocalSend: `register` with a device, `prepare-upload` (the other user accepts or declines on
//! their screen), then `upload` of every accepted file. Each request opens its own connection, like LocalSend does.
use crate::http::{read_response, write_request, Body, MAX_JSON};
use crate::identity::Identity;
use crate::protocol::{DeviceInfo, FileMeta, PrepareUploadRequest, PrepareUploadResponse, API, PORT};
use crate::tls::{client_config, wrap_client, Stream};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::io::{self, BufReader, Read, Write};
use std::net::{IpAddr, Shutdown, SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::Duration;

/// How long the other user has to accept (the answer to `prepare-upload` comes when they do).
const ACCEPT_TIMEOUT: Duration = Duration::from_secs(600);

/// A device that can be sent to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Peer {
    pub info: DeviceInfo,
    pub ip: IpAddr,
    /// Check the certificate against `info.fingerprint` (false for a device added by hand: trusted on first use).
    pub verify: bool,
}

impl Peer {
    pub fn port(&self) -> u16 {
        self.info.port.unwrap_or(PORT)
    }
}

/// Lets another thread stop a send, also while it waits for the other user to accept.
#[derive(Default)]
pub struct Cancel {
    flag: AtomicBool,
    socket: Mutex<Option<TcpStream>>,
}

impl Cancel {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn cancel(&self) {
        self.flag.store(true, Ordering::SeqCst);
        if let Some(s) = self.socket.lock().unwrap().as_ref() {
            let _ = s.shutdown(Shutdown::Both);
        }
    }

    pub fn is_cancelled(&self) -> bool {
        self.flag.load(Ordering::SeqCst)
    }

    fn attach(&self, tcp: &TcpStream) {
        *self.socket.lock().unwrap() = tcp.try_clone().ok();
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    Path(PathBuf),
    Bytes(Vec<u8>),
}

/// A file (or a text) to send.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outgoing {
    pub name: String,
    pub size: u64,
    pub file_type: String,
    pub sha256: Option<String>,
    /// For a text message: the text.
    pub preview: Option<String>,
    pub source: Source,
}

fn mime_of(name: &str) -> &'static str {
    match name.rsplit('.').next().map(str::to_ascii_lowercase).as_deref() {
        Some("txt" | "log" | "md") => "text/plain",
        Some("json") => "application/json",
        Some("png") => "image/png",
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("pdf") => "application/pdf",
        _ => "application/octet-stream",
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn random_hex(n: usize) -> String {
    let mut b = vec![0u8; n];
    getrandom::getrandom(&mut b).expect("the system random generator");
    hex(&b)
}

impl Outgoing {
    /// A file on disk: its size and SHA-256 are read now (the receiver checks the hash).
    pub fn from_path(path: &Path) -> io::Result<Self> {
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).ok_or_else(|| io::Error::other("no file name"))?;
        let mut f = std::fs::File::open(path)?;
        let mut hasher = Sha256::new();
        let mut size = 0u64;
        let mut buf = vec![0u8; 64 * 1024];
        loop {
            let n = f.read(&mut buf)?;
            if n == 0 {
                break;
            }
            hasher.update(&buf[..n]);
            size += n as u64;
        }
        Ok(Self { file_type: mime_of(&name).to_string(), name, size, sha256: Some(hex(&hasher.finalize())), preview: None, source: Source::Path(path.to_path_buf()) })
    }

    /// A text message.
    pub fn text(text: &str) -> Self {
        let data = text.as_bytes().to_vec();
        Self { name: format!("{}.txt", random_hex(8)), size: data.len() as u64, file_type: "text/plain".into(), sha256: None, preview: Some(text.to_string()), source: Source::Bytes(data) }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// Every file was received.
    Sent { files: usize },
    /// A text message was shown on the other screen.
    MessageDelivered,
    Declined,
    /// The other device wants a PIN, which is not supported.
    PinRequired,
    /// The other device is busy with another transfer.
    Busy,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SendEvent {
    /// Waiting for the other user to accept.
    Waiting,
    Uploading { file: String, index: usize, of: usize, done: u64, total: u64 },
}

fn connect(id: &Identity, peer: &Peer, timeout: Duration, cancel: &Cancel) -> Result<Box<dyn Stream>, String> {
    let addr = SocketAddr::new(peer.ip, peer.port());
    let tcp = TcpStream::connect_timeout(&addr, Duration::from_secs(8)).map_err(|e| short_error(&e))?;
    tcp.set_read_timeout(Some(timeout)).map_err(|e| e.to_string())?;
    tcp.set_write_timeout(Some(Duration::from_secs(60))).map_err(|e| e.to_string())?;
    cancel.attach(&tcp);
    if cancel.is_cancelled() {
        return Err("annulé".into());
    }
    if peer.info.uses_https() {
        let config = client_config(id, peer.verify.then_some(peer.info.fingerprint.as_str()))?;
        wrap_client(config, tcp).map_err(|e| short_error(&e))
    } else {
        Ok(Box::new(tcp))
    }
}

fn short_error(e: &io::Error) -> String {
    match e.kind() {
        io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock => "pas de réponse de l'appareil".into(),
        io::ErrorKind::ConnectionRefused => "connexion refusée : LocalSend est-il ouvert ?".into(),
        _ => {
            let s = e.to_string();
            if s.contains("fingerprint") { "le certificat ne correspond pas à l'empreinte de l'appareil".into() } else { s.chars().take(100).collect() }
        }
    }
}

fn url_encode(s: &str) -> String {
    s.bytes().map(|b| if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) { (b as char).to_string() } else { format!("%{b:02X}") }).collect()
}

/// POSTs JSON and returns the status and the JSON answer, if any.
fn request_json(id: &Identity, peer: &Peer, name: &str, query: &[(&str, &str)], body: Option<&Value>, timeout: Duration, cancel: &Cancel) -> Result<(u16, Option<Value>), String> {
    let mut conn = connect(id, peer, timeout, cancel)?;
    let data = body.map(|v| v.to_string().into_bytes()).unwrap_or_default();
    let q: Vec<String> = query.iter().map(|(k, v)| format!("{k}={}", url_encode(v))).collect();
    let target = format!("{API}{name}{}", if q.is_empty() { String::new() } else { format!("?{}", q.join("&")) });
    let host = format!("{}:{}", peer.ip, peer.port());
    let io_err = |e: io::Error| if cancel.is_cancelled() { "annulé".to_string() } else { short_error(&e) };
    write_request(&mut conn, "POST", &target, &host, &[("Content-Type", "application/json".into()), ("Content-Length", data.len().to_string())]).map_err(io_err)?;
    conn.write_all(&data).and_then(|_| conn.flush()).map_err(io_err)?;
    let mut reader = BufReader::new(conn);
    let head = read_response(&mut reader).map_err(io_err)?;
    let mut raw = Vec::new();
    Body::with_headers(&mut reader, &head.headers).map_err(io_err)?.take(MAX_JSON).read_to_end(&mut raw).map_err(io_err)?;
    let parsed = if raw.is_empty() { None } else { serde_json::from_slice(&raw).ok() };
    Ok((head.status, parsed))
}

/// Introduces this device to `peer` (`register`); returns what the peer says about itself.
pub fn register(id: &Identity, me: &DeviceInfo, peer: &Peer) -> Result<DeviceInfo, String> {
    let body = serde_json::to_value(me).map_err(|e| e.to_string())?;
    let (status, answer) = request_json(id, peer, "register", &[], Some(&body), Duration::from_secs(4), &Cancel::new())?;
    match (status, answer) {
        (200, Some(v)) => serde_json::from_value(v).map_err(|e| e.to_string()),
        (s, _) => Err(format!("refusé (HTTP {s})")),
    }
}

/// Sends `files` to `peer`. `me` is this device with its port and protocol.
pub fn send(
    id: &Identity,
    me: &DeviceInfo,
    peer: &Peer,
    files: Vec<Outgoing>,
    progress: &mut dyn FnMut(SendEvent),
    cancel: &Cancel,
) -> Result<Outcome, String> {
    if files.is_empty() {
        return Err("rien à envoyer".into());
    }
    let ids: Vec<String> = files.iter().map(|_| random_hex(16)).collect();
    let request = PrepareUploadRequest {
        info: me.clone(),
        files: files
            .iter()
            .zip(&ids)
            .map(|(f, i)| {
                (i.clone(), FileMeta { id: i.clone(), file_name: f.name.clone(), size: f.size, file_type: f.file_type.clone(), sha256: f.sha256.clone(), preview: f.preview.clone() })
            })
            .collect(),
    };
    progress(SendEvent::Waiting);
    let body = serde_json::to_value(&request).map_err(|e| e.to_string())?;
    let (status, answer) = request_json(id, peer, "prepare-upload", &[], Some(&body), ACCEPT_TIMEOUT, cancel)?;
    if cancel.is_cancelled() {
        return Err("annulé".into());
    }
    match status {
        204 => return Ok(Outcome::MessageDelivered),
        401 => return Ok(Outcome::PinRequired),
        403 => return Ok(Outcome::Declined),
        409 => return Ok(Outcome::Busy),
        200 => {}
        s => return Err(format!("refusé (HTTP {s})")),
    }
    let answer: PrepareUploadResponse = serde_json::from_value(answer.ok_or("réponse vide")?).map_err(|e| format!("réponse invalide : {e}"))?;
    let accepted: Vec<(&Outgoing, &String)> = files.iter().zip(&ids).filter(|(_, i)| answer.files.contains_key(*i)).collect();
    let total_files = accepted.len();
    for (index, (file, fid)) in accepted.iter().enumerate() {
        let token = &answer.files[*fid];
        if let Err(e) = upload(id, peer, &answer.session_id, fid, token, file, index + 1, total_files, progress, cancel) {
            if cancel.is_cancelled() {
                let _ = request_json(id, peer, "cancel", &[("sessionId", &answer.session_id)], None, Duration::from_secs(4), &Cancel::new());
                return Err("annulé".into());
            }
            return Err(e);
        }
    }
    Ok(Outcome::Sent { files: total_files })
}

#[allow(clippy::too_many_arguments)]
fn upload(id: &Identity, peer: &Peer, session: &str, file_id: &str, token: &str, f: &Outgoing, index: usize, of: usize, progress: &mut dyn FnMut(SendEvent), cancel: &Cancel) -> Result<(), String> {
    let mut conn = connect(id, peer, Duration::from_secs(60), cancel)?;
    let target = format!("{API}upload?sessionId={}&fileId={}&token={}", url_encode(session), url_encode(file_id), url_encode(token));
    let host = format!("{}:{}", peer.ip, peer.port());
    let io_err = |e: io::Error| if cancel.is_cancelled() { "annulé".to_string() } else { short_error(&e) };
    write_request(&mut conn, "POST", &target, &host, &[("Content-Type", "application/octet-stream".into()), ("Content-Length", f.size.to_string())]).map_err(io_err)?;
    let mut done = 0u64;
    let report = |done: u64, progress: &mut dyn FnMut(SendEvent)| progress(SendEvent::Uploading { file: f.name.clone(), index, of, done, total: f.size });
    match &f.source {
        Source::Bytes(data) => {
            conn.write_all(data).map_err(io_err)?;
            done = data.len() as u64;
            report(done, progress);
        }
        Source::Path(p) => {
            let mut src = std::fs::File::open(p).map_err(|e| e.to_string())?;
            let mut buf = vec![0u8; 64 * 1024];
            while done < f.size {
                if cancel.is_cancelled() {
                    return Err("annulé".into());
                }
                let n = src.read(&mut buf).map_err(|e| e.to_string())?;
                if n == 0 {
                    return Err("le fichier est plus court que prévu".into());
                }
                conn.write_all(&buf[..n]).map_err(io_err)?;
                done += n as u64;
                report(done, progress);
            }
        }
    }
    conn.flush().map_err(io_err)?;
    let mut reader = BufReader::new(conn);
    let head = read_response(&mut reader).map_err(io_err)?;
    let mut rest = Vec::new();
    let _ = Body::with_headers(&mut reader, &head.headers).and_then(|b| b.take(MAX_JSON).read_to_end(&mut rest));
    if head.status != 200 {
        return Err(format!("{} : refusé (HTTP {})", f.name, head.status));
    }
    Ok(())
}
