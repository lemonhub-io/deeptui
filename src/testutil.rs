//! Test-only helpers (compiled only under `cargo test`).

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

static COUNTER: AtomicU64 = AtomicU64::new(0);

/// A unique, empty temp directory removed on drop. Stands in for the project
/// root that the file tools operate on.
pub struct TmpDir(PathBuf);

impl TmpDir {
    pub fn new(tag: &str) -> Self {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let p = std::env::temp_dir().join(format!(
            "ragent-test-{tag}-{}-{nanos}-{n}",
            std::process::id()
        ));
        std::fs::create_dir_all(&p).unwrap();
        Self(p)
    }

    pub fn path(&self) -> &Path {
        &self.0
    }

    pub fn join(&self, rel: &str) -> PathBuf {
        self.0.join(rel)
    }

    /// Write `content` to `rel`, creating parent dirs. Returns the full path.
    pub fn write(&self, rel: &str, content: &str) -> PathBuf {
        let p = self.join(rel);
        if let Some(d) = p.parent() {
            std::fs::create_dir_all(d).unwrap();
        }
        std::fs::write(&p, content).unwrap();
        p
    }
}

impl Drop for TmpDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// One SSE body built from `data:` payloads (blank line after each).
pub fn sse(payloads: &[&str]) -> String {
    payloads
        .iter()
        .map(|p| format!("data: {p}\n\n"))
        .collect::<String>()
}

/// Minimal loopback HTTP/1.1 server for tests: every [`Stub::enqueue`]d reply
/// is served to one connection, in FIFO order, and every request is captured
/// verbatim. One dispatcher thread owns the socket, so replies can never be
/// reordered by the scheduler.
pub struct Stub {
    addr: SocketAddr,
    reqs: Arc<Mutex<Vec<String>>>,
    tx: std::sync::mpsc::Sender<(u16, String)>,
}

impl Stub {
    pub fn new() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
        let addr = listener.local_addr().unwrap();
        let reqs: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let (tx, rx) = std::sync::mpsc::channel::<(u16, String)>();
        let served = Arc::clone(&reqs);
        std::thread::spawn(move || {
            while let Ok((status, body)) = rx.recv() {
                let Ok((mut s, _)) = listener.accept() else {
                    return;
                };
                served.lock().unwrap().push(read_request(&mut s));
                let head = format!(
                    "HTTP/1.1 {status} {}\r\nContent-Type: text/event-stream\r\n\
                     Content-Length: {}\r\nConnection: close\r\n\r\n",
                    reason(status),
                    body.len()
                );
                let _ = s.write_all(head.as_bytes());
                let _ = s.write_all(body.as_bytes());
                let _ = s.flush();
            }
        });
        Self { addr, reqs, tx }
    }

    /// Base URL (with `/v1`) to hand to `Config::base_url`.
    pub fn base_url(&self) -> String {
        format!("http://{}/v1", self.addr)
    }

    /// Requests received so far (request line + headers + body).
    pub fn requests(&self) -> Vec<String> {
        self.reqs.lock().unwrap().clone()
    }

    /// Serve `status` + `body` on the next connection, then close it.
    pub fn enqueue(&self, status: u16, body: impl Into<String>) {
        let _ = self.tx.send((status, body.into()));
    }
}

fn reason(status: u16) -> &'static str {
    match status {
        200 => "OK",
        400 => "Bad Request",
        401 => "Unauthorized",
        429 => "Too Many Requests",
        500 => "Internal Server Error",
        _ => "Status",
    }
}

/// Read one whole request: headers, then `Content-Length` bytes of body.
fn read_request(s: &mut TcpStream) -> String {
    let mut buf = Vec::new();
    let mut tmp = [0u8; 4096];
    while let Ok(n) = s.read(&mut tmp) {
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&tmp[..n]);
        if let Some(end) = find(&buf, b"\r\n\r\n") {
            let head = String::from_utf8_lossy(&buf[..end]).to_lowercase();
            let want: usize = head
                .lines()
                .find_map(|l| l.strip_prefix("content-length:"))
                .and_then(|v| v.trim().parse().ok())
                .unwrap_or(0);
            if buf.len() >= end + 4 + want {
                break;
            }
        }
    }
    String::from_utf8_lossy(&buf).into_owned()
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}
