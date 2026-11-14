//! The sign-in return address: a one-shot listener on this computer.
//!
//! The browser is sent to the server's sign-in page with a redirect back to
//! `http://127.0.0.1:<ephemeral port>/callback`. The listener accepts that one callback, with
//! the matching `state`, answers a small "you can close this tab" page and closes. Other paths
//! and a wrong `state` are refused with a plain page, and the listener keeps waiting until it
//! times out. Nothing is logged, and the code is handed to the caller only.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::Arc;
use std::time::{Duration, Instant};

const MAX_HEAD: usize = 16 * 1024;

/// Why a callback could not be used. The words are for the person watching; they never carry a
/// value from the address the browser came back on.
#[derive(Debug)]
pub enum CallbackError {
    TimedOut,
    Closed,
}

pub struct Loopback {
    port: u16,
    rx: mpsc::Receiver<Result<String, CallbackError>>,
    stop: Arc<AtomicBool>,
}

impl Drop for Loopback {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
    }
}

impl Loopback {
    /// Binds `127.0.0.1` on an ephemeral port and starts waiting for the callback whose `state`
    /// matches, giving up after `wait` from now.
    pub fn bind(state: &str, wait: Duration) -> std::io::Result<Self> {
        let listener = TcpListener::bind("127.0.0.1:0")?;
        listener.set_nonblocking(true)?;
        let port = listener.local_addr()?.port();
        let (tx, rx) = mpsc::channel();
        let stop = Arc::new(AtomicBool::new(false));
        let thread_stop = stop.clone();
        let state = state.to_owned();
        std::thread::spawn(move || {
            let deadline = Instant::now() + wait;
            let mut sent = false;
            while !thread_stop.load(Ordering::SeqCst) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        if let Some(code) = handle(&mut stream, &state) {
                            // The code is sent at most once; the first matching callback wins.
                            sent = tx.send(Ok(code)).is_ok();
                            break;
                        }
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(50));
                    }
                    Err(_) => break,
                }
                if Instant::now() >= deadline {
                    break;
                }
            }
            if !sent {
                let _ = tx.send(Err(CallbackError::TimedOut));
            }
        });
        Ok(Self { port, rx, stop })
    }

    pub fn redirect_uri(&self) -> String {
        format!("http://127.0.0.1:{}/callback", self.port)
    }

    /// Waits up to `budget` for the callback.
    pub fn wait(&self, budget: Duration) -> Result<String, CallbackError> {
        match self.rx.recv_timeout(budget) {
            Ok(r) => r,
            Err(mpsc::RecvTimeoutError::Timeout) => Err(CallbackError::TimedOut),
            Err(mpsc::RecvTimeoutError::Disconnected) => Err(CallbackError::Closed),
        }
    }
}

/// Answers one request. Returns the authorization code when the path was `/callback` and the
/// `state` matched; anything else gets a plain page and `None` (keep waiting).
fn handle(stream: &mut TcpStream, expected_state: &str) -> Option<String> {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
    let _ = stream.set_write_timeout(Some(Duration::from_secs(5)));
    let mut head = Vec::new();
    let mut chunk = [0u8; 1024];
    loop {
        let n = match stream.read(&mut chunk) {
            Ok(0) | Err(_) => return None,
            Ok(n) => n,
        };
        head.extend_from_slice(&chunk[..n]);
        if head.windows(4).any(|w| w == b"\r\n\r\n") || head.len() >= MAX_HEAD {
            break;
        }
    }
    let text = String::from_utf8_lossy(&head);
    let Some(line) = text.lines().next() else {
        page(
            stream,
            "400 Bad Request",
            "This page is finished.",
            "Nothing is on it.",
        );
        return None;
    };
    let mut words = line.split_whitespace();
    let method = words.next().unwrap_or("");
    let target = words.next().unwrap_or("");
    if method != "GET" {
        page(
            stream,
            "405 Method Not Allowed",
            "This page is finished.",
            "Nothing is on it.",
        );
        return None;
    }
    let (path, query) = match target.split_once('?') {
        Some((p, q)) => (p, q),
        None => (target, ""),
    };
    if path != "/callback" {
        page(
            stream,
            "404 Not Found",
            "This page is finished.",
            "Nothing is on it.",
        );
        return None;
    }
    let params = parse_query(query);
    let code = params.get("code").filter(|c| !c.is_empty());
    match (code, params.get("state").map(String::as_str)) {
        (Some(code), Some(state)) if state == expected_state => {
            page(
                stream,
                "200 OK",
                "You can close this tab.",
                "Helm received the sign-in. Go back to Helm.",
            );
            Some(code.to_owned())
        }
        _ => {
            page(
                stream,
                "400 Bad Request",
                "The sign-in did not finish.",
                "Go back to Helm and try again.",
            );
            None
        }
    }
}

fn page(stream: &mut TcpStream, status: &str, heading: &str, note: &str) {
    let body = format!(
        "<!doctype html><html><head><title>Helm</title></head><body>\
         <h1>{heading}</h1><p>{note}</p></body></html>"
    );
    let _ = write!(
        stream,
        "HTTP/1.1 {status}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = stream.flush();
    let _ = stream.shutdown(std::net::Shutdown::Write);
}

fn parse_query(query: &str) -> HashMap<String, String> {
    let mut out = HashMap::new();
    for pair in query.split('&').filter(|p| !p.is_empty()) {
        let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
        if let Some(k) = percent_decode(k) {
            if let Some(v) = percent_decode(v) {
                out.insert(k, v);
            }
        }
    }
    out
}

/// Percent-decoding, lenient: a bad escape keeps the text as it was written.
pub(crate) fn percent_decode(s: &str) -> Option<String> {
    let bytes = s.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' => {
                let hex = |b: u8| match b {
                    b'0'..=b'9' => Some(b - b'0'),
                    b'a'..=b'f' => Some(b - b'a' + 10),
                    b'A'..=b'F' => Some(b - b'A' + 10),
                    _ => None,
                };
                if i + 2 < bytes.len() {
                    if let (Some(h), Some(l)) = (hex(bytes[i + 1]), hex(bytes[i + 2])) {
                        out.push(h * 16 + l);
                        i += 3;
                        continue;
                    }
                }
                out.push(b'%');
                i += 1;
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8(out).ok()
}

/// Form-encoding a value: everything the query syntax would take literally passes through, the
/// rest becomes `%XX`, and a space becomes `+`.
pub(crate) fn form_encode(pairs: &[(&str, &str)]) -> String {
    const KEEP: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-._~";
    let enc = |s: &str| -> String {
        let mut out = String::new();
        for b in s.as_bytes() {
            if KEEP.contains(b) {
                out.push(*b as char);
            } else if *b == b' ' {
                out.push('+');
            } else {
                out.push_str(&format!("%{b:02X}"));
            }
        }
        out
    };
    pairs
        .iter()
        .map(|(k, v)| format!("{}={}", enc(k), enc(v)))
        .collect::<Vec<_>>()
        .join("&")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpStream;

    fn get(addr: &str, path: &str) -> String {
        let mut s = TcpStream::connect(addr).unwrap();
        write!(s, "GET {path} HTTP/1.1\r\nHost: {addr}\r\n\r\n").unwrap();
        let mut out = Vec::new();
        let _ = s.set_read_timeout(Some(Duration::from_secs(5)));
        let _ = s.read_to_end(&mut out);
        String::from_utf8_lossy(&out).into_owned()
    }

    #[test]
    fn the_callback_is_answered_once_and_other_visits_are_refused() {
        let lb = Loopback::bind("good-state", Duration::from_secs(10)).unwrap();
        let addr = format!("127.0.0.1:{}", lb.port);
        // A wrong path and a wrong state are refused, and the listener keeps waiting.
        assert!(get(&addr, "/other").contains("404"));
        assert!(get(&addr, "/callback?code=c1&state=wrong").contains("400"));
        let ok = get(&addr, "/callback?code=c%201&state=good-state");
        assert!(ok.contains("200 OK") && ok.contains("close this tab"));
        assert_eq!(lb.wait(Duration::from_secs(2)).unwrap(), "c 1");
        // The listener is gone: nothing more is accepted.
        assert!(lb.wait(Duration::from_millis(300)).is_err());
    }

    #[test]
    fn waiting_without_a_callback_times_out() {
        let lb = Loopback::bind("s", Duration::from_millis(300)).unwrap();
        assert!(matches!(
            lb.wait(Duration::from_secs(2)),
            Err(CallbackError::TimedOut)
        ));
    }

    #[test]
    fn encoding_round_trips() {
        let body = form_encode(&[("code", "a b+c%&"), ("state", "s1")]);
        assert_eq!(body, "code=a+b%2Bc%25%26&state=s1");
        let p = parse_query(&body);
        assert_eq!(p.get("code").map(String::as_str), Some("a b+c%&"));
        assert_eq!(p.get("state").map(String::as_str), Some("s1"));
        assert!(percent_decode("%ZZ").as_deref() == Some("%ZZ"));
    }
}
