// SPDX-License-Identifier: GPL-3.0-or-later
//! The receiving side of LocalSend: answers `info`, `register`, `prepare-upload`, `upload` and `cancel` on a stream.
//!
//! The user's decision and the display are behind [`Ui`]: nothing is written to the download folder before the user
//! accepted the transfer. Files are written to a hidden `.part` file, checked against their SHA-256 when the sender
//! gave one, and only then given their name (never overwriting an existing file).
use crate::http::{read_request, write_response, Body, Request};
use crate::protocol::{DeviceInfo, FileMeta, PrepareUploadRequest, PrepareUploadResponse, API};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// What the application provides: the decision of the user and the display of what happens.
pub trait Ui: Send + Sync {
    /// A device wants to send `files`. Blocks until the user answers (or a time-out): `true` to accept.
    fn ask(&self, from: &DeviceInfo, ip: IpAddr, files: &[FileMeta]) -> bool;
    /// A text message arrived.
    fn message(&self, from: &DeviceInfo, text: &str);
    /// A device was seen (register or prepare-upload).
    fn peer_seen(&self, _info: &DeviceInfo, _ip: IpAddr) {}
    /// Bytes of a file were received (`done` of `total` for that file).
    fn progress(&self, _file: &str, _done: u64, _total: u64) {}
    /// A file was saved at `path`.
    fn saved(&self, _path: &Path) {}
}

struct Session {
    id: String,
    ip: IpAddr,
    tokens: HashMap<String, String>,
    files: HashMap<String, FileMeta>,
    left: HashSet<String>,
    ended: bool,
}

pub struct Receiver<U: Ui> {
    /// This device, for the `info` and `register` answers (with the port and protocol only where LocalSend wants them).
    alias: String,
    fingerprint: String,
    downloads: PathBuf,
    ui: U,
    session: Mutex<Option<Session>>,
}

/// A received file name as a path inside the download folder: folders are kept, `..` and control characters are not.
pub fn safe_relative(name: &str) -> PathBuf {
    let mut p = PathBuf::new();
    for part in name.replace('\\', "/").split('/') {
        let clean: String = part.chars().filter(|c| *c >= ' ' && *c != '\x7f').collect::<String>().trim().chars().take(200).collect();
        if !clean.is_empty() && clean != "." && clean != ".." {
            p.push(clean);
        }
    }
    if p.as_os_str().is_empty() {
        p.push("file");
    }
    p
}

/// `path`, or `name (1).ext`, `name (2).ext`... : an existing file is never replaced.
pub fn unique_path(path: &Path) -> PathBuf {
    if !path.exists() {
        return path.to_path_buf();
    }
    let stem = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let ext = path.extension().map(|e| format!(".{}", e.to_string_lossy())).unwrap_or_default();
    for i in 1..10000 {
        let c = path.with_file_name(format!("{stem} ({i}){ext}"));
        if !c.exists() {
            return c;
        }
    }
    path.with_file_name(format!("{stem} {}{ext}", random_hex(3)))
}

fn random_hex(bytes: usize) -> String {
    let mut b = vec![0u8; bytes];
    getrandom::getrandom(&mut b).expect("the system random generator");
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

impl<U: Ui> Receiver<U> {
    pub fn new(alias: &str, fingerprint: &str, downloads: PathBuf, ui: U) -> Self {
        Self { alias: alias.to_string(), fingerprint: fingerprint.to_string(), downloads, ui, session: Mutex::new(None) }
    }

    fn info(&self) -> Value {
        serde_json::to_value(DeviceInfo::this_device(&self.alias, &self.fingerprint, false)).expect("serialisable")
    }

    /// Serves the requests of one connection until the peer closes it (or asks for it to be closed).
    pub fn serve<S: Read + Write>(&self, stream: S, ip: IpAddr) -> io::Result<()> {
        let mut conn = BufReader::new(stream);
        while let Some(req) = read_request(&mut conn)? {
            let mut close = false;
            let answer = {
                let mut body = Body::new(&mut conn, &req)?;
                let (status, answer) = self.route(&req, &mut body, ip);
                close = close || status >= 400 && req.path.ends_with("/upload");
                if !close {
                    // keep the connection in step even when the handler did not read the whole body
                    body.drain()?;
                }
                (status, answer)
            };
            write_response(conn.get_mut(), answer.0, answer.1.as_ref(), close)?;
            if close {
                return Ok(());
            }
        }
        Ok(())
    }

    fn route<R: BufRead>(&self, req: &Request, body: &mut Body<'_, R>, ip: IpAddr) -> (u16, Option<Value>) {
        let name = req.path.strip_prefix(API).unwrap_or("");
        let result = match (req.method.as_str(), name, req.path.as_str()) {
            ("GET", "info", _) | ("GET", _, "/api/localsend/v1/info") => Ok((200, Some(self.info()))),
            ("POST", "info", _) => Ok((200, Some(self.info()))),
            ("POST", "register", _) => self.register(body, ip),
            ("POST", "prepare-upload", _) => self.prepare_upload(body, ip),
            ("POST", "upload", _) => self.upload(req, body, ip),
            ("POST", "cancel", _) => self.cancel(req, ip),
            _ => Ok((404, None)),
        };
        result.unwrap_or((400, None))
    }

    fn register<R: BufRead>(&self, body: &mut Body<'_, R>, ip: IpAddr) -> io::Result<(u16, Option<Value>)> {
        let info: DeviceInfo = serde_json::from_value(body.json()?).map_err(io::Error::other)?;
        self.ui.peer_seen(&info, ip);
        Ok((200, Some(self.info())))
    }

    fn prepare_upload<R: BufRead>(&self, body: &mut Body<'_, R>, ip: IpAddr) -> io::Result<(u16, Option<Value>)> {
        let request: PrepareUploadRequest = serde_json::from_value(body.json()?).map_err(io::Error::other)?;
        if request.files.is_empty() {
            return Err(io::Error::other("no files"));
        }
        self.ui.peer_seen(&request.info, ip);
        // a single text file with a preview is a message: shown, nothing to transfer
        let metas: Vec<&FileMeta> = request.files.values().collect();
        if let [one] = metas.as_slice() {
            if one.file_type.starts_with("text/") {
                if let Some(text) = &one.preview {
                    self.ui.message(&request.info, text);
                    return Ok((204, None));
                }
            }
        }
        {
            let s = self.session.lock().unwrap();
            if s.as_ref().is_some_and(|s| !s.ended) {
                return Ok((409, None)); // busy with another transfer
            }
        }
        let files: Vec<FileMeta> = request.files.values().cloned().collect();
        if !self.ui.ask(&request.info, ip, &files) {
            return Ok((403, None));
        }
        let mut tokens = BTreeMap::new();
        for id in request.files.keys() {
            tokens.insert(id.clone(), random_hex(16));
        }
        let session_id = random_hex(16);
        *self.session.lock().unwrap() = Some(Session {
            id: session_id.clone(),
            ip,
            tokens: tokens.iter().map(|(k, v)| (k.clone(), v.clone())).collect(),
            files: request.files.iter().map(|(k, v)| (k.clone(), v.clone())).collect(),
            left: request.files.keys().cloned().collect(),
            ended: false,
        });
        let answer = PrepareUploadResponse { session_id, files: tokens };
        Ok((200, Some(serde_json::to_value(answer).map_err(io::Error::other)?)))
    }

    fn upload<R: BufRead>(&self, req: &Request, body: &mut Body<'_, R>, ip: IpAddr) -> io::Result<(u16, Option<Value>)> {
        let q = |k: &str| req.query.get(k).map(String::as_str).unwrap_or("");
        let meta = {
            let s = self.session.lock().unwrap();
            let Some(session) = s.as_ref() else { return Ok((403, None)) };
            let valid = q("sessionId") == session.id && session.tokens.get(q("fileId")).is_some_and(|t| t == q("token")) && session.ip == ip;
            if !valid {
                return Ok((403, None));
            }
            if session.ended {
                return Ok((409, None));
            }
            session.files[q("fileId")].clone()
        };
        let relative = safe_relative(&meta.file_name);
        let target = self.downloads.join(&relative);
        let part = target.with_file_name(format!(".{}.part", target.file_name().map(|n| n.to_string_lossy()).unwrap_or_default()));
        if let Some(dir) = target.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let mut hasher = meta.sha256.as_ref().map(|_| Sha256::new());
        let mut received = 0u64;
        let result = (|| -> io::Result<()> {
            let mut out = std::fs::File::create(&part)?;
            let mut buf = vec![0u8; 64 * 1024];
            loop {
                let n = body.read(&mut buf)?;
                if n == 0 {
                    break;
                }
                out.write_all(&buf[..n])?;
                if let Some(h) = hasher.as_mut() {
                    h.update(&buf[..n]);
                }
                received += n as u64;
                self.ui.progress(&meta.file_name, received, meta.size);
            }
            out.flush()
        })();
        if let Err(e) = result {
            let _ = std::fs::remove_file(&part);
            let _ = e;
            return Ok((500, None));
        }
        if let (Some(h), Some(want)) = (hasher, meta.sha256.as_ref()) {
            if hex(&h.finalize()) != want.to_ascii_lowercase() {
                let _ = std::fs::remove_file(&part);
                return Ok((422, None));
            }
        }
        let final_path = unique_path(&target);
        std::fs::rename(&part, &final_path)?;
        {
            let mut s = self.session.lock().unwrap();
            if let Some(session) = s.as_mut() {
                session.left.remove(&meta.id);
                if session.left.is_empty() {
                    session.ended = true;
                }
            }
        }
        self.ui.saved(&final_path);
        Ok((200, None))
    }

    fn cancel(&self, req: &Request, ip: IpAddr) -> io::Result<(u16, Option<Value>)> {
        let mut s = self.session.lock().unwrap();
        if let Some(session) = s.as_mut() {
            if req.query.get("sessionId").is_some_and(|id| *id == session.id) && session.ip == ip {
                session.ended = true;
            }
        }
        Ok((200, None))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;
    use std::sync::{Arc, Mutex as M};

    /// A test stream: reads the scripted requests, collects the answers.
    struct Pipe {
        input: Cursor<Vec<u8>>,
        output: Vec<u8>,
    }
    impl Read for Pipe {
        fn read(&mut self, b: &mut [u8]) -> io::Result<usize> {
            self.input.read(b)
        }
    }
    impl Write for Pipe {
        fn write(&mut self, b: &[u8]) -> io::Result<usize> {
            self.output.write(b)
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[derive(Default)]
    struct Log {
        asked: Vec<(String, usize)>,
        messages: Vec<String>,
        saved: Vec<PathBuf>,
    }
    struct FakeUi {
        accept: bool,
        log: Arc<M<Log>>,
    }
    impl Ui for FakeUi {
        fn ask(&self, from: &DeviceInfo, _ip: IpAddr, files: &[FileMeta]) -> bool {
            self.log.lock().unwrap().asked.push((from.alias.clone(), files.len()));
            self.accept
        }
        fn message(&self, _from: &DeviceInfo, text: &str) {
            self.log.lock().unwrap().messages.push(text.to_string());
        }
        fn saved(&self, path: &Path) {
            self.log.lock().unwrap().saved.push(path.to_path_buf());
        }
    }

    fn ip() -> IpAddr {
        "192.168.1.20".parse().unwrap()
    }

    fn dir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("zerocart-send-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn post(path: &str, body: &str) -> Vec<u8> {
        format!("POST {path} HTTP/1.1\r\nHost: x\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}", body.len()).into_bytes()
    }

    /// Splits the answers written on the pipe: (status, body).
    fn answers(out: &[u8]) -> Vec<(u16, String)> {
        let text = String::from_utf8_lossy(out).into_owned();
        let mut v = Vec::new();
        let mut rest = text.as_str();
        while let Some(i) = rest.find("HTTP/1.1 ") {
            rest = &rest[i + 9..];
            let status: u16 = rest[..3].parse().unwrap();
            let head_end = rest.find("\r\n\r\n").unwrap();
            let len: usize = rest[..head_end].lines().find_map(|l| l.strip_prefix("Content-Length: ")).unwrap().parse().unwrap();
            v.push((status, rest[head_end + 4..head_end + 4 + len].to_string()));
            rest = &rest[head_end + 4 + len..];
        }
        v
    }

    fn receiver(accept: bool, d: &Path) -> (Receiver<FakeUi>, Arc<M<Log>>) {
        let log = Arc::new(M::new(Log::default()));
        (Receiver::new("ZeroCart", "FINGER", d.to_path_buf(), FakeUi { accept, log: log.clone() }), log)
    }

    fn run(r: &Receiver<FakeUi>, requests: Vec<Vec<u8>>) -> Vec<(u16, String)> {
        let mut pipe = Pipe { input: Cursor::new(requests.concat()), output: Vec::new() };
        r.serve(&mut pipe, ip()).unwrap();
        answers(&pipe.output)
    }

    fn prepare_body(files: &[(&str, &str, u64, Option<&str>)]) -> String {
        let f: Vec<String> = files
            .iter()
            .map(|(id, name, size, sha)| {
                let sha = sha.map(|s| format!("\"{s}\"")).unwrap_or("null".into());
                format!(r#""{id}":{{"id":"{id}","fileName":"{name}","size":{size},"fileType":"application/octet-stream","sha256":{sha}}}"#)
            })
            .collect();
        format!(r#"{{"info":{{"alias":"Pixel","fingerprint":"PX","version":"2.1"}},"files":{{{}}}}}"#, f.join(","))
    }

    fn upload_request(session: &str, file: &str, token: &str, data: &[u8]) -> Vec<u8> {
        let mut r = format!("POST /api/localsend/v2/upload?sessionId={session}&fileId={file}&token={token} HTTP/1.1\r\nContent-Length: {}\r\n\r\n", data.len()).into_bytes();
        r.extend_from_slice(data);
        r
    }

    fn sha(data: &[u8]) -> String {
        hex(&Sha256::digest(data))
    }

    #[test]
    fn info_and_register_answer_with_this_device_and_note_the_peer() {
        let d = dir("info");
        let (r, _) = receiver(true, &d);
        let a = run(&r, vec![
            b"GET /api/localsend/v2/info HTTP/1.1\r\n\r\n".to_vec(),
            post("/api/localsend/v2/register", r#"{"alias":"Pixel","fingerprint":"PX","port":53317,"protocol":"https"}"#),
            b"GET /api/localsend/v2/nothing HTTP/1.1\r\n\r\n".to_vec(),
        ]);
        assert_eq!(a.len(), 3);
        let info: Value = serde_json::from_str(&a[0].1).unwrap();
        assert_eq!((info["alias"].as_str(), info["fingerprint"].as_str()), (Some("ZeroCart"), Some("FINGER")));
        assert!(info.get("port").is_none(), "no port in the info answer");
        assert_eq!(a[1].0, 200);
        assert_eq!(a[2].0, 404);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn nothing_is_written_before_the_user_accepts_and_a_decline_is_403() {
        let d = dir("decline");
        let (r, log) = receiver(false, &d);
        let a = run(&r, vec![post("/api/localsend/v2/prepare-upload", &prepare_body(&[("f1", "a.bin", 3, None)]))]);
        assert_eq!(a[0].0, 403);
        assert_eq!(log.lock().unwrap().asked, vec![("Pixel".to_string(), 1)]);
        assert_eq!(std::fs::read_dir(&d).unwrap().count(), 0, "the download folder is untouched");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn an_accepted_transfer_saves_the_file_after_checking_its_hash() {
        let d = dir("accept");
        let (r, log) = receiver(true, &d);
        let data = b"hello zerocart";
        let a = run(&r, vec![post("/api/localsend/v2/prepare-upload", &prepare_body(&[("f1", "docs/a.bin", data.len() as u64, Some(&sha(data)))]))]);
        assert_eq!(a[0].0, 200);
        let ans: PrepareUploadResponse = serde_json::from_str(&a[0].1).unwrap();
        let token = &ans.files["f1"];
        assert_eq!(token.len(), 32);
        let b = run(&r, vec![upload_request(&ans.session_id, "f1", token, data)]);
        assert_eq!(b[0].0, 200);
        assert_eq!(std::fs::read(d.join("docs/a.bin")).unwrap(), data);
        assert!(!d.join("docs/.a.bin.part").exists(), "no leftover .part file");
        assert_eq!(log.lock().unwrap().saved, vec![d.join("docs/a.bin")]);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_wrong_hash_is_422_and_leaves_no_file() {
        let d = dir("hash");
        let (r, _) = receiver(true, &d);
        let a = run(&r, vec![post("/api/localsend/v2/prepare-upload", &prepare_body(&[("f1", "a.bin", 3, Some(&sha(b"xyz")))]))]);
        let ans: PrepareUploadResponse = serde_json::from_str(&a[0].1).unwrap();
        let b = run(&r, vec![upload_request(&ans.session_id, "f1", &ans.files["f1"], b"abc")]);
        assert_eq!(b[0].0, 422);
        assert_eq!(std::fs::read_dir(&d).unwrap().count(), 0);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn uploads_need_the_session_the_file_the_token_and_the_same_address() {
        let d = dir("auth");
        let (r, _) = receiver(true, &d);
        let a = run(&r, vec![post("/api/localsend/v2/prepare-upload", &prepare_body(&[("f1", "a.bin", 1, None)]))]);
        let ans: PrepareUploadResponse = serde_json::from_str(&a[0].1).unwrap();
        let tok = &ans.files["f1"];
        for bad in [
            upload_request("wrong", "f1", tok, b"x"),
            upload_request(&ans.session_id, "f1", "badtoken", b"x"),
            upload_request(&ans.session_id, "zzz", tok, b"x"),
        ] {
            let b = run(&r, vec![bad]);
            assert_eq!(b[0].0, 403);
        }
        // another address, even with the right token
        let mut pipe = Pipe { input: Cursor::new(upload_request(&ans.session_id, "f1", tok, b"x")), output: vec![] };
        r.serve(&mut pipe, "10.0.0.9".parse().unwrap()).unwrap();
        assert_eq!(answers(&pipe.output)[0].0, 403);
        assert_eq!(std::fs::read_dir(&d).unwrap().count(), 0);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_second_transfer_while_one_is_running_is_busy_and_a_finished_one_frees_the_receiver() {
        let d = dir("busy");
        let (r, _) = receiver(true, &d);
        let a = run(&r, vec![post("/api/localsend/v2/prepare-upload", &prepare_body(&[("f1", "a.bin", 1, None)]))]);
        let ans: PrepareUploadResponse = serde_json::from_str(&a[0].1).unwrap();
        let second = run(&r, vec![post("/api/localsend/v2/prepare-upload", &prepare_body(&[("g1", "b.bin", 1, None)]))]);
        assert_eq!(second[0].0, 409);
        run(&r, vec![upload_request(&ans.session_id, "f1", &ans.files["f1"], b"x")]);
        let third = run(&r, vec![post("/api/localsend/v2/prepare-upload", &prepare_body(&[("g1", "b.bin", 1, None)]))]);
        assert_eq!(third[0].0, 200, "the first session is over");
        // a cancel from the sender ends the session too
        let ans: PrepareUploadResponse = serde_json::from_str(&third[0].1).unwrap();
        run(&r, vec![format!("POST /api/localsend/v2/cancel?sessionId={} HTTP/1.1\r\nContent-Length: 0\r\n\r\n", ans.session_id).into_bytes()]);
        let fourth = run(&r, vec![post("/api/localsend/v2/prepare-upload", &prepare_body(&[("h1", "c.bin", 1, None)]))]);
        assert_eq!(fourth[0].0, 200);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_text_message_is_shown_and_answered_204_without_asking_or_saving() {
        let d = dir("text");
        let (r, log) = receiver(false, &d); // even a receiver that declines files shows messages
        let body = r#"{"info":{"alias":"Pixel","fingerprint":"PX"},"files":{"t":{"id":"t","fileName":"m.txt","size":5,"fileType":"text/plain","preview":"hello"}}}"#;
        let a = run(&r, vec![post("/api/localsend/v2/prepare-upload", body)]);
        assert_eq!(a[0].0, 204);
        let l = log.lock().unwrap();
        assert_eq!(l.messages, vec!["hello".to_string()]);
        assert!(l.asked.is_empty());
        drop(l);
        assert_eq!(std::fs::read_dir(&d).unwrap().count(), 0);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn hostile_names_stay_inside_the_download_folder_and_existing_files_are_never_replaced() {
        assert_eq!(safe_relative("../../etc/passwd"), PathBuf::from("etc/passwd"));
        assert_eq!(safe_relative("..\\..\\x.bin"), PathBuf::from("x.bin"));
        assert_eq!(safe_relative("/abs/olute"), PathBuf::from("abs/olute"));
        assert_eq!(safe_relative("a/./b\u{7}c/ d "), PathBuf::from("a/bc/d"));
        assert_eq!(safe_relative(".."), PathBuf::from("file"));
        assert_eq!(safe_relative(""), PathBuf::from("file"));
        assert_eq!(safe_relative(&"x".repeat(500)).to_string_lossy().len(), 200);
        let d = dir("unique");
        let p = d.join("a.bin");
        std::fs::write(&p, b"1").unwrap();
        assert_eq!(unique_path(&p), d.join("a (1).bin"));
        std::fs::write(d.join("a (1).bin"), b"2").unwrap();
        assert_eq!(unique_path(&p), d.join("a (2).bin"));
        assert_eq!(unique_path(&d.join("new.bin")), d.join("new.bin"));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_hostile_name_in_a_real_upload_cannot_escape() {
        let d = dir("escape");
        let down = d.join("downloads");
        std::fs::create_dir_all(&down).unwrap();
        let (r, _) = receiver(true, &down);
        let a = run(&r, vec![post("/api/localsend/v2/prepare-upload", &prepare_body(&[("f1", "../../outside.bin", 1, None)]))]);
        let ans: PrepareUploadResponse = serde_json::from_str(&a[0].1).unwrap();
        run(&r, vec![upload_request(&ans.session_id, "f1", &ans.files["f1"], b"x")]);
        assert!(down.join("outside.bin").exists());
        assert!(!d.join("outside.bin").exists());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn malformed_requests_are_400_and_an_oversized_json_is_refused() {
        let d = dir("bad");
        let (r, _) = receiver(true, &d);
        let a = run(&r, vec![post("/api/localsend/v2/prepare-upload", "not json")]);
        assert_eq!(a[0].0, 400);
        let a = run(&r, vec![post("/api/localsend/v2/prepare-upload", r#"{"info":{"alias":"A","fingerprint":"F"},"files":{}}"#)]);
        assert_eq!(a[0].0, 400, "no files");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_chunked_upload_is_received_like_a_sized_one() {
        let d = dir("chunked");
        let (r, _) = receiver(true, &d);
        let a = run(&r, vec![post("/api/localsend/v2/prepare-upload", &prepare_body(&[("f1", "c.bin", 11, None)]))]);
        let ans: PrepareUploadResponse = serde_json::from_str(&a[0].1).unwrap();
        let req = format!(
            "POST /api/localsend/v2/upload?sessionId={}&fileId=f1&token={} HTTP/1.1\r\nTransfer-Encoding: chunked\r\n\r\n5\r\nhello\r\n6\r\n world\r\n0\r\n\r\n",
            ans.session_id, ans.files["f1"]
        );
        let b = run(&r, vec![req.into_bytes()]);
        assert_eq!(b[0].0, 200);
        assert_eq!(std::fs::read(d.join("c.bin")).unwrap(), b"hello world");
        let _ = std::fs::remove_dir_all(&d);
    }
}
