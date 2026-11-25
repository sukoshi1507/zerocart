// SPDX-License-Identifier: GPL-3.0-or-later
//! The small part of HTTP/1.1 that LocalSend needs: a request head, a body read as `Content-Length` bytes or as
//! chunks, and a response. It works on any `BufRead`/`Write`, so TLS or a test buffer plugs in the same way.
use std::collections::HashMap;
use std::io::{self, BufRead, Read, Write};

const MAX_LINE: usize = 8192;
const MAX_HEADERS: usize = 100;
/// Largest JSON body accepted (a `prepare-upload` with many files).
pub const MAX_JSON: u64 = 4 << 20;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request {
    pub method: String,
    pub path: String,
    pub query: HashMap<String, String>,
    /// Header names in lowercase.
    pub headers: HashMap<String, String>,
}

impl Request {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.get(name).map(String::as_str)
    }

}

fn is_chunked(headers: &HashMap<String, String>) -> bool {
    headers.get("transfer-encoding").is_some_and(|v| v.eq_ignore_ascii_case("chunked"))
}

fn content_length(headers: &HashMap<String, String>) -> io::Result<u64> {
    match headers.get("content-length") {
        None => Ok(0),
        Some(v) => v.trim().parse().map_err(|_| bad("invalid Content-Length")),
    }
}

/// The head of a response read by the client.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Response {
    pub status: u16,
    pub headers: HashMap<String, String>,
}

fn read_headers<R: BufRead>(r: &mut R) -> io::Result<HashMap<String, String>> {
    let mut headers = HashMap::new();
    loop {
        let Some(h) = read_line(r)? else { return Err(bad("unexpected end of stream")) };
        if h.is_empty() {
            return Ok(headers);
        }
        if headers.len() >= MAX_HEADERS {
            return Err(bad("too many headers"));
        }
        let (k, v) = h.split_once(':').ok_or_else(|| bad("malformed header"))?;
        headers.insert(k.trim().to_ascii_lowercase(), v.trim().to_string());
    }
}

/// Reads the status line and headers of a response.
pub fn read_response<R: BufRead>(r: &mut R) -> io::Result<Response> {
    let line = read_line(r)?.ok_or_else(|| bad("the connection closed without an answer"))?;
    let mut parts = line.split_whitespace();
    let (Some(version), Some(status)) = (parts.next(), parts.next()) else { return Err(bad("malformed status line")) };
    if !version.starts_with("HTTP/1.") {
        return Err(bad("not HTTP/1.x"));
    }
    let status = status.parse().map_err(|_| bad("malformed status code"))?;
    Ok(Response { status, headers: read_headers(r)? })
}

/// Writes a request head (the body, if any, is written by the caller after it).
pub fn write_request<W: Write>(w: &mut W, method: &str, target: &str, host: &str, headers: &[(&str, String)]) -> io::Result<()> {
    let mut head = format!("{method} {target} HTTP/1.1\r\nHost: {host}\r\nUser-Agent: ZeroCart\r\n");
    for (k, v) in headers {
        head.push_str(&format!("{k}: {v}\r\n"));
    }
    head.push_str("\r\n");
    w.write_all(head.as_bytes())
}

fn bad(msg: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg.to_string())
}

/// One line without its line ending; `None` at the end of the stream. A line longer than [`MAX_LINE`] is an error.
fn read_line<R: BufRead>(r: &mut R) -> io::Result<Option<String>> {
    let mut buf = Vec::new();
    loop {
        let available = r.fill_buf()?;
        if available.is_empty() {
            return if buf.is_empty() { Ok(None) } else { Err(bad("unexpected end of stream")) };
        }
        match available.iter().position(|&b| b == b'\n') {
            Some(i) => {
                buf.extend_from_slice(&available[..i]);
                r.consume(i + 1);
                break;
            }
            None => {
                let n = available.len();
                buf.extend_from_slice(available);
                r.consume(n);
            }
        }
        if buf.len() > MAX_LINE {
            return Err(bad("line too long"));
        }
    }
    if buf.len() > MAX_LINE {
        return Err(bad("line too long"));
    }
    if buf.last() == Some(&b'\r') {
        buf.pop();
    }
    String::from_utf8(buf).map(Some).map_err(|_| bad("line is not UTF-8"))
}

pub fn url_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'+' => out.push(b' '),
            b'%' if i + 2 < b.len() => match std::str::from_utf8(&b[i + 1..i + 3]).ok().and_then(|h| u8::from_str_radix(h, 16).ok()) {
                Some(v) => {
                    out.push(v);
                    i += 2;
                }
                None => out.push(b'%'),
            },
            c => out.push(c),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Reads a request head. `None`: the peer closed the connection between two requests.
pub fn read_request<R: BufRead>(r: &mut R) -> io::Result<Option<Request>> {
    let line = loop {
        match read_line(r)? {
            None => return Ok(None),
            Some(l) if l.is_empty() => continue, // stray blank lines between requests
            Some(l) => break l,
        }
    };
    let mut parts = line.split_whitespace();
    let (Some(method), Some(target), Some(version)) = (parts.next(), parts.next(), parts.next()) else {
        return Err(bad("malformed request line"));
    };
    if !version.starts_with("HTTP/1.") {
        return Err(bad("not HTTP/1.x"));
    }
    let (path, query_text) = target.split_once('?').unwrap_or((target, ""));
    let query = query_text
        .split('&')
        .filter(|p| !p.is_empty())
        .map(|p| {
            let (k, v) = p.split_once('=').unwrap_or((p, ""));
            (url_decode(k), url_decode(v))
        })
        .collect();
    let headers = read_headers(r)?;
    Ok(Some(Request { method: method.to_ascii_uppercase(), path: url_decode(path), query, headers }))
}

enum Mode {
    Length(u64),
    /// Bytes left in the current chunk, and whether the last chunk was read.
    Chunked(u64, bool),
}

/// The body of a request, read as it arrives (a file is never held in memory).
pub struct Body<'a, R: BufRead> {
    r: &'a mut R,
    mode: Mode,
}

impl<'a, R: BufRead> Body<'a, R> {
    pub fn new(r: &'a mut R, req: &Request) -> io::Result<Self> {
        Self::with_headers(r, &req.headers)
    }

    /// The body that follows these headers (of a request or of a response).
    pub fn with_headers(r: &'a mut R, headers: &HashMap<String, String>) -> io::Result<Self> {
        let mode = if is_chunked(headers) { Mode::Chunked(0, false) } else { Mode::Length(content_length(headers)?) };
        Ok(Self { r, mode })
    }

    /// The whole body as JSON, refusing more than [`MAX_JSON`] bytes.
    pub fn json(&mut self) -> io::Result<serde_json::Value> {
        let mut data = Vec::new();
        self.by_ref().take(MAX_JSON + 1).read_to_end(&mut data)?;
        if data.len() as u64 > MAX_JSON {
            return Err(bad("body too large"));
        }
        serde_json::from_slice(&data).map_err(|e| bad(&format!("invalid JSON: {e}")))
    }

    /// Reads and drops what is left, to be ready for the next request on the connection.
    pub fn drain(&mut self) -> io::Result<()> {
        io::copy(self, &mut io::sink()).map(|_| ())
    }
}

impl<R: BufRead> Read for Body<'_, R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match &mut self.mode {
            Mode::Length(left) => {
                if *left == 0 || buf.is_empty() {
                    return Ok(0);
                }
                let n = buf.len().min(*left as usize);
                let got = self.r.read(&mut buf[..n])?;
                if got == 0 {
                    return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "connection closed during the body"));
                }
                *left -= got as u64;
                Ok(got)
            }
            Mode::Chunked(left, done) => {
                if *done || buf.is_empty() {
                    return Ok(0);
                }
                if *left == 0 {
                    let line = read_line(self.r)?.ok_or_else(|| bad("unexpected end of stream"))?;
                    let size = u64::from_str_radix(line.split(';').next().unwrap_or("").trim(), 16).map_err(|_| bad("invalid chunk size"))?;
                    if size == 0 {
                        // trailers, then the blank line that ends the body
                        while !read_line(self.r)?.ok_or_else(|| bad("unexpected end of stream"))?.is_empty() {}
                        *done = true;
                        return Ok(0);
                    }
                    *left = size;
                }
                let n = buf.len().min(*left as usize);
                let got = self.r.read(&mut buf[..n])?;
                if got == 0 {
                    return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "connection closed during a chunk"));
                }
                *left -= got as u64;
                if *left == 0 {
                    read_line(self.r)?; // the CRLF after the chunk data
                }
                Ok(got)
            }
        }
    }
}

fn reason(status: u16) -> &'static str {
    match status {
        200 => "OK",
        204 => "No Content",
        400 => "Bad Request",
        403 => "Forbidden",
        404 => "Not Found",
        409 => "Conflict",
        413 => "Payload Too Large",
        422 => "Unprocessable Entity",
        500 => "Internal Server Error",
        _ => "Status",
    }
}

/// Writes a response. A JSON body is sent with its length; `close` asks the peer to drop the connection after it.
pub fn write_response<W: Write>(w: &mut W, status: u16, json: Option<&serde_json::Value>, close: bool) -> io::Result<()> {
    let body = json.map(|v| v.to_string().into_bytes()).unwrap_or_default();
    let mut head = format!("HTTP/1.1 {status} {}\r\nServer: ZeroCart\r\nContent-Length: {}\r\n", reason(status), body.len());
    if !body.is_empty() {
        head.push_str("Content-Type: application/json\r\n");
    }
    head.push_str(if close { "Connection: close\r\n\r\n" } else { "Connection: keep-alive\r\n\r\n" });
    w.write_all(head.as_bytes())?;
    w.write_all(&body)?;
    w.flush()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn a_request_head_is_parsed_with_its_query_and_lowercase_headers() {
        let raw = b"POST /api/localsend/v2/upload?sessionId=a%20b&fileId=f1&token=t HTTP/1.1\r\nHost: x\r\nContent-Length: 5\r\nContent-Type: application/octet-stream\r\n\r\nhello";
        let mut r = Cursor::new(&raw[..]);
        let req = read_request(&mut r).unwrap().unwrap();
        assert_eq!((req.method.as_str(), req.path.as_str()), ("POST", "/api/localsend/v2/upload"));
        assert_eq!(req.query["sessionId"], "a b");
        assert_eq!((req.query["fileId"].as_str(), req.query["token"].as_str()), ("f1", "t"));
        assert_eq!(req.header("content-length"), Some("5"));
        let mut body = Vec::new();
        Body::new(&mut r, &req).unwrap().read_to_end(&mut body).unwrap();
        assert_eq!(body, b"hello");
    }

    #[test]
    fn the_end_of_the_stream_between_requests_is_not_an_error_but_garbage_is() {
        assert_eq!(read_request(&mut Cursor::new(&b""[..])).unwrap(), None);
        assert_eq!(read_request(&mut Cursor::new(&b"\r\n\r\n"[..])).unwrap(), None);
        assert!(read_request(&mut Cursor::new(&b"GARBAGE\r\n\r\n"[..])).is_err());
        assert!(read_request(&mut Cursor::new(&b"GET / SMTP/1.0\r\n\r\n"[..])).is_err());
        assert!(read_request(&mut Cursor::new(&b"GET / HTTP/1.1\r\nno colon here\r\n\r\n"[..])).is_err());
        let long = format!("GET /{} HTTP/1.1\r\n\r\n", "a".repeat(MAX_LINE + 10));
        assert!(read_request(&mut Cursor::new(long.into_bytes())).is_err(), "a huge line is refused");
    }

    #[test]
    fn a_chunked_body_is_decoded_across_chunks_and_trailers() {
        let raw = b"POST /x HTTP/1.1\r\nTransfer-Encoding: chunked\r\n\r\n5\r\nhello\r\n6;ext=1\r\n world\r\n0\r\nX-Trailer: 1\r\n\r\nGET /next HTTP/1.1\r\n\r\n";
        let mut r = Cursor::new(&raw[..]);
        let req = read_request(&mut r).unwrap().unwrap();
        let mut body = String::new();
        Body::new(&mut r, &req).unwrap().read_to_string(&mut body).unwrap();
        assert_eq!(body, "hello world");
        // the connection is positioned on the next request
        assert_eq!(read_request(&mut r).unwrap().unwrap().path, "/next");
    }

    #[test]
    fn a_body_cut_short_is_an_error_and_drain_keeps_the_connection_in_step() {
        let raw = b"POST /x HTTP/1.1\r\nContent-Length: 10\r\n\r\nabc";
        let mut r = Cursor::new(&raw[..]);
        let req = read_request(&mut r).unwrap().unwrap();
        let mut sink = Vec::new();
        assert!(Body::new(&mut r, &req).unwrap().read_to_end(&mut sink).is_err());
        let raw = b"POST /x HTTP/1.1\r\nContent-Length: 4\r\n\r\nabcdGET /y HTTP/1.1\r\n\r\n";
        let mut r = Cursor::new(&raw[..]);
        let req = read_request(&mut r).unwrap().unwrap();
        Body::new(&mut r, &req).unwrap().drain().unwrap();
        assert_eq!(read_request(&mut r).unwrap().unwrap().path, "/y");
    }

    #[test]
    fn json_bodies_are_bounded_and_validated() {
        let req = |body: &str| {
            let raw = format!("POST /x HTTP/1.1\r\nContent-Length: {}\r\n\r\n{body}", body.len());
            raw.into_bytes()
        };
        let raw = req(r#"{"a":1}"#);
        let mut r = Cursor::new(raw);
        let q = read_request(&mut r).unwrap().unwrap();
        assert_eq!(Body::new(&mut r, &q).unwrap().json().unwrap()["a"], 1);
        let raw = req("not json");
        let mut r = Cursor::new(raw);
        let q = read_request(&mut r).unwrap().unwrap();
        assert!(Body::new(&mut r, &q).unwrap().json().is_err());
        let bad = b"POST /x HTTP/1.1\r\nContent-Length: abc\r\n\r\n".to_vec();
        let mut r = Cursor::new(bad);
        let q = read_request(&mut r).unwrap().unwrap();
        assert!(Body::new(&mut r, &q).is_err());
    }

    #[test]
    fn responses_carry_their_length_and_content_type() {
        let mut out = Vec::new();
        write_response(&mut out, 200, Some(&serde_json::json!({"a": 1})), false).unwrap();
        let text = String::from_utf8(out).unwrap();
        assert!(text.starts_with("HTTP/1.1 200 OK\r\n") && text.contains("Content-Length: 7\r\n") && text.contains("Content-Type: application/json"));
        assert!(text.ends_with("\r\n\r\n{\"a\":1}") && text.contains("keep-alive"));
        let mut out = Vec::new();
        write_response(&mut out, 403, None, true).unwrap();
        let text = String::from_utf8(out).unwrap();
        assert!(text.starts_with("HTTP/1.1 403 Forbidden") && text.contains("Content-Length: 0") && !text.contains("Content-Type") && text.contains("close"));
    }

    #[test]
    fn a_response_and_a_request_head_are_written_and_read_back() {
        let mut out = Vec::new();
        write_request(&mut out, "POST", "/api/localsend/v2/upload?a=b", "10.0.0.2:53317", &[("Content-Length", "3".into())]).unwrap();
        out.extend_from_slice(b"abc");
        let mut r = Cursor::new(out);
        let req = read_request(&mut r).unwrap().unwrap();
        assert_eq!((req.method.as_str(), req.path.as_str(), req.query["a"].as_str(), req.header("host")), ("POST", "/api/localsend/v2/upload", "b", Some("10.0.0.2:53317")));
        let mut body = String::new();
        Body::new(&mut r, &req).unwrap().read_to_string(&mut body).unwrap();
        assert_eq!(body, "abc");
        let mut resp = Vec::new();
        write_response(&mut resp, 409, Some(&serde_json::json!({"x": 1})), false).unwrap();
        let mut r = Cursor::new(resp);
        let head = read_response(&mut r).unwrap();
        assert_eq!(head.status, 409);
        let mut b = String::new();
        Body::with_headers(&mut r, &head.headers).unwrap().read_to_string(&mut b).unwrap();
        assert_eq!(b, "{\"x\":1}");
        assert!(read_response(&mut Cursor::new(&b""[..])).is_err());
        assert!(read_response(&mut Cursor::new(&b"SMTP 250 ok\r\n\r\n"[..])).is_err());
    }

    #[test]
    fn url_decoding_handles_escapes_plus_and_bad_input() {
        assert_eq!(url_decode("a%20b+c%2Fd"), "a b c/d");
        assert_eq!(url_decode("100%"), "100%");
        assert_eq!(url_decode("%zz"), "%zz");
        assert_eq!(url_decode("%e2%82%ac"), "€");
    }
}
