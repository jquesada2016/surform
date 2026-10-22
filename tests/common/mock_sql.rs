//! The mock SurrealDB `/sql` boundary: a tiny hand-rolled HTTP server on a localhost port —
//! `std::net::TcpListener`, one connection per request (`Connection:
//! close`), a fixed request-line/headers/Content-Length parse — that
//! RECORDS every request it receives and answers from a scripted
//! list (the last script repeats). No new test dependency: the HTTP
//! surface it speaks is the one the contract pins (POST /sql,
//! JSON-array responses). The engine behind it is fiction — the
//! responses below are the shapes the docs page and the 3.3.0 probe
//! captured, pinned in the suite that uses this mock.

// The shared-test-home allow (common/mod.rs's own): each suite
// compiles this module by `#[path]` and uses a subset of it — the
// dead-code allow states that structurally, not per-crate.
#![allow(dead_code)]

use std::collections::VecDeque;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

/// One received request, exactly as the wire carried it.
#[derive(Clone, Debug)]
pub struct Recorded {
    pub method: String,
    pub path: String,
    /// Header names lower-cased, values verbatim, in arrival order.
    pub headers: Vec<(String, String)>,
    pub body: String,
}

impl Recorded {
    pub fn header(&self, name: &str) -> Option<&str> {
        let name = name.to_ascii_lowercase();
        self.headers
            .iter()
            .find(|(key, _)| *key == name)
            .map(|(_, value)| value.as_str())
    }
}

/// One scripted answer: the HTTP status line's code and the raw body.
#[derive(Clone, Debug)]
pub struct Scripted {
    pub status: u16,
    pub body: String,
}

/// The scripted `status: "OK"` element (the probe's shape, `type`
/// included).
pub fn ok_element() -> String {
    r#"{"result":null,"status":"OK","time":"1.000ms","type":null}"#.to_string()
}

/// A `status: "ERR"` element with its kind, optional `details.kind`
/// (the nested reason), and the `result` message string.
pub fn err_element(kind: &str, details_kind: Option<&str>, message: &str) -> String {
    let details = match details_kind {
        Some(nested) => format!(r#""details":{{"kind":"{nested}"}},"#),
        None => String::new(),
    };
    format!(
        r#"{{{details}"kind":"{kind}","result":{},"status":"ERR","time":"1.000ms","type":null}}"#,
        json_string(message)
    )
}

/// A `status: "OK"` SELECT element whose result is `rows` (a JSON
/// array's text).
pub fn select_element(rows: &str) -> String {
    format!(r#"{{"result":{rows},"status":"OK","time":"1.000ms","type":null}}"#)
}

/// One ledger row exactly as the probe captured the projected SELECT
/// over the wire — `{hash, name, number}`, `number` a JSON number —
/// no `id` field (the real projection does not return one).
pub fn ledger_row(number: u32, name: &str, hash: &str) -> String {
    format!(
        r#"{{"hash":{},"name":{},"number":{number}}}"#,
        json_string(hash),
        json_string(name)
    )
}

/// One ledger row as the WIDE projection (`SELECT number, name, hash,
/// sdl …` — the revert read) returns it over the wire: the `sdl`
/// column rides as a JSON string, newlines intact (the 3.3.0 probe's
/// shape, captured on the shared instance 2026-10-08).
pub fn ledger_row_sdl(number: u32, name: &str, hash: &str, sdl: &str) -> String {
    format!(
        r#"{{"hash":{},"name":{},"number":{number},"sdl":{}}}"#,
        json_string(hash),
        json_string(name),
        json_string(sdl)
    )
}

/// A missing-table SELECT's ERR element — the fresh-database case
/// (the 3.3.0 probe's exact shape: kind NotFound, details.kind Table).
pub fn missing_table_element(table: &str) -> String {
    format!(
        r#"{{"details":{{"details":{{"name":{}}},"kind":"Table"}},"kind":"NotFound","result":{},"status":"ERR","time":"1.000ms","type":null}}"#,
        json_string(table),
        json_string(&format!("The table '{table}' does not exist"))
    )
}

/// A minimal JSON string literal — RFC 8259 escaping for the bytes
/// test fixtures carry: backslash, quote, and the control characters
/// (the ledger's multiline `sdl` rides `\n`-escaped exactly as the
/// live server spelled it in the 3.3.0 probe).
pub fn json_string(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('"');
    for c in text.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// The running mock. Drop stops it (the accept loop observes the
/// shutdown flag, woken by a self-connect).
pub struct MockSql {
    pub port: u16,
    recorded: Arc<Mutex<Vec<Recorded>>>,
    script: Arc<Mutex<VecDeque<Scripted>>>,
    shutdown: Arc<AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl MockSql {
    /// Starts the mock: `script` answers the first `script.len()`
    /// requests in order; anything beyond repeats `fallback`.
    pub fn start(script: Vec<Scripted>, fallback: Scripted) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("the mock binds");
        let port = listener
            .local_addr()
            .expect("the mock has an address")
            .port();
        let recorded = Arc::new(Mutex::new(Vec::new()));
        let queue = Arc::new(Mutex::new(VecDeque::from(script)));
        let fallback = Arc::new(fallback);
        let shutdown = Arc::new(AtomicBool::new(false));
        let handle = {
            let recorded = Arc::clone(&recorded);
            let queue = Arc::clone(&queue);
            let fallback = Arc::clone(&fallback);
            let shutdown = Arc::clone(&shutdown);
            std::thread::spawn(move || {
                listener.set_nonblocking(false).expect("the mock blocks");
                for stream in listener.incoming() {
                    if shutdown.load(Ordering::SeqCst) {
                        break;
                    }
                    let Ok(mut stream) = stream else { continue };
                    let Some(request) = read_request(&mut stream) else {
                        continue;
                    };
                    recorded.lock().expect("the log locks").push(request);
                    let answer = {
                        let mut queue = queue.lock().expect("the script locks");
                        match queue.pop_front() {
                            Some(scripted) => scripted,
                            None => (*fallback).clone(),
                        }
                    };
                    let reason = match answer.status {
                        200 => "OK",
                        400 => "Bad Request",
                        401 => "Unauthorized",
                        _ => "Error",
                    };
                    let response = format!(
                        "HTTP/1.1 {} {reason}\r\nContent-Type: application/json\r\n\
                         Content-Length: {}\r\nConnection: close\r\n\r\n{}",
                        answer.status,
                        answer.body.len(),
                        answer.body
                    );
                    let _ = stream.write_all(response.as_bytes());
                    let _ = stream.flush();
                }
            })
        };
        Self {
            port,
            recorded,
            script: queue,
            shutdown,
            handle: Some(handle),
        }
    }

    /// The server root a push run points at.
    pub fn endpoint(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }

    /// Every request received so far, in order.
    pub fn requests(&self) -> Vec<Recorded> {
        self.recorded.lock().expect("the log locks").clone()
    }

    /// Appends more scripted answers (the resume legs re-script the
    /// same mock).
    pub fn push_script(&self, scripted: Scripted) {
        self.script
            .lock()
            .expect("the script locks")
            .push_back(scripted);
    }
}

impl Drop for MockSql {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::SeqCst);
        // Wake the blocked accept with a self-connect.
        let _ = std::net::TcpStream::connect(("127.0.0.1", self.port));
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

/// The mock's request parse: the request line, the header block
/// (through the blank line), then exactly Content-Length body bytes.
fn read_request(stream: &mut std::net::TcpStream) -> Option<Recorded> {
    let mut buffer = Vec::new();
    let mut chunk = [0u8; 4096];
    let header_end = loop {
        let read = stream.read(&mut chunk).ok()?;
        if read == 0 {
            return None;
        }
        buffer.extend_from_slice(&chunk[..read]);
        if let Some(at) = find_headers_end(&buffer) {
            break at;
        }
        if buffer.len() > 1 << 20 {
            return None;
        }
    };
    let head = String::from_utf8_lossy(&buffer[..header_end]).into_owned();
    let mut lines = head.split("\r\n");
    let request_line = lines.next()?;
    let mut parts = request_line.split(' ');
    let method = parts.next()?.to_string();
    let path = parts.next()?.to_string();
    let headers: Vec<(String, String)> = lines
        .filter_map(|line| line.split_once(':'))
        .map(|(name, value)| (name.trim().to_ascii_lowercase(), value.trim().to_string()))
        .collect();
    let length: usize = headers
        .iter()
        .find(|(name, _)| name == "content-length")
        .and_then(|(_, value)| value.parse().ok())
        .unwrap_or(0);
    let mut body = buffer[header_end + 4..].to_vec();
    while body.len() < length {
        let read = stream.read(&mut chunk).ok()?;
        if read == 0 {
            break;
        }
        body.extend_from_slice(&chunk[..read]);
    }
    body.truncate(length);
    Some(Recorded {
        method,
        path,
        headers,
        body: String::from_utf8_lossy(&body).into_owned(),
    })
}

fn find_headers_end(buffer: &[u8]) -> Option<usize> {
    buffer.windows(4).position(|window| window == b"\r\n\r\n")
}
