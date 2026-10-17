//! Output plumbing: UTF-8-safe chunking, the replay ring and flow control.

use std::collections::VecDeque;
use std::sync::{Condvar, Mutex};
use std::time::Duration;

/// Largest chunk sent in one event.
pub const CHUNK_MAX: usize = 64 * 1024;
/// Recent output kept for a reattach.
pub const REPLAY_MAX: usize = 512 * 1024;
/// Output pauses once this many UTF-16 units are unacknowledged.
pub const UNACKED_MAX: u64 = 1024 * 1024;

/// Turns raw PTY bytes into complete UTF-8 strings, holding back a split code point for the next
/// read. Invalid bytes become U+FFFD rather than stalling the stream.
#[derive(Default)]
pub struct Utf8Carry {
    pending: Vec<u8>,
}

impl Utf8Carry {
    pub fn push(&mut self, bytes: &[u8]) -> String {
        self.pending.extend_from_slice(bytes);
        let valid_up_to = match std::str::from_utf8(&self.pending) {
            Ok(_) => self.pending.len(),
            Err(e) => match e.error_len() {
                // An incomplete sequence at the end: keep it for the next read.
                None => e.valid_up_to(),
                // A truly invalid byte: decode lossily, keeping a trailing partial sequence.
                Some(_) => {
                    let tail = incomplete_tail(&self.pending);
                    self.pending.len() - tail
                }
            },
        };
        let rest = self.pending.split_off(valid_up_to);
        let out = String::from_utf8_lossy(&self.pending).into_owned();
        self.pending = rest;
        out
    }
}

/// Bytes at the end that begin a multi-byte sequence not yet complete (0-3).
fn incomplete_tail(b: &[u8]) -> usize {
    for back in 1..=3.min(b.len()) {
        let c = b[b.len() - back];
        if c & 0b1100_0000 == 0b1000_0000 {
            continue; // continuation byte
        }
        let need = if c >= 0xF0 {
            4
        } else if c >= 0xE0 {
            3
        } else if c >= 0xC0 {
            2
        } else {
            1
        };
        return if need > back { back } else { 0 };
    }
    0
}

/// Split a string into pieces of at most `max` bytes on character boundaries.
pub fn split_chunks(s: &str, max: usize) -> Vec<&str> {
    let mut out = Vec::new();
    let mut rest = s;
    while rest.len() > max {
        let mut cut = max;
        while !rest.is_char_boundary(cut) {
            cut -= 1;
        }
        let (a, b) = rest.split_at(cut);
        out.push(a);
        rest = b;
    }
    if !rest.is_empty() {
        out.push(rest);
    }
    out
}

/// The most recent output, at most [`REPLAY_MAX`] bytes, trimmed on character boundaries.
#[derive(Default)]
pub struct Replay {
    parts: VecDeque<String>,
    bytes: usize,
}

impl Replay {
    pub fn push(&mut self, s: &str) {
        self.parts.push_back(s.to_owned());
        self.bytes += s.len();
        while self.bytes > REPLAY_MAX {
            let Some(front) = self.parts.front_mut() else {
                break;
            };
            let over = self.bytes - REPLAY_MAX;
            if front.len() <= over {
                self.bytes -= front.len();
                self.parts.pop_front();
            } else {
                let mut cut = over;
                while !front.is_char_boundary(cut) {
                    cut += 1;
                }
                self.bytes -= cut;
                front.drain(..cut);
            }
        }
    }

    pub fn text(&self) -> String {
        self.parts.iter().map(String::as_str).collect()
    }

    pub fn len(&self) -> usize {
        self.bytes
    }

    pub fn is_empty(&self) -> bool {
        self.bytes == 0
    }
}

/// Flow control: the reader adds what it sent and waits while too much is unacknowledged; the
/// renderer's `terminal_ack` subtracts. `close` releases a waiting reader for good.
#[derive(Default)]
pub struct FlowGate {
    state: Mutex<(u64, bool)>,
    cv: Condvar,
}

impl FlowGate {
    pub fn sent(&self, units: u64) {
        self.state.lock().unwrap().0 += units;
    }

    pub fn ack(&self, units: u64) {
        let mut st = self.state.lock().unwrap();
        st.0 = st.0.saturating_sub(units);
        self.cv.notify_all();
    }

    pub fn unacked(&self) -> u64 {
        self.state.lock().unwrap().0
    }

    /// Block while over the limit (re-checking every `poll`), until acked or closed.
    pub fn wait_for_room(&self, poll: Duration) {
        let mut st = self.state.lock().unwrap();
        while st.0 > UNACKED_MAX && !st.1 {
            st = self.cv.wait_timeout(st, poll).unwrap().0;
        }
    }

    pub fn close(&self) {
        self.state.lock().unwrap().1 = true;
        self.cv.notify_all();
    }
}

/// UTF-16 units in a string (what the renderer counts when it acks).
pub fn utf16_units(s: &str) -> u64 {
    s.encode_utf16().count() as u64
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn a_split_code_point_waits_for_its_tail() {
        let mut c = Utf8Carry::default();
        let euro = "€".as_bytes(); // 3 bytes
        assert_eq!(c.push(&[b'a', euro[0]]), "a");
        assert_eq!(c.push(&euro[1..2]), "");
        assert_eq!(c.push(&[euro[2], b'b']), "€b");
        assert_eq!(
            c.push(&[0xff, b'x']),
            "\u{fffd}x",
            "invalid bytes do not stall"
        );
    }

    #[test]
    fn chunks_respect_the_cap_and_char_boundaries() {
        let s = "é".repeat(10); // 20 bytes
        let parts = split_chunks(&s, 5);
        assert!(parts.iter().all(|p| p.len() <= 5));
        assert_eq!(parts.concat(), s);
        assert_eq!(split_chunks("", 5).len(), 0);
    }

    #[test]
    fn replay_keeps_the_newest_512_kib() {
        let mut r = Replay::default();
        r.push(&"a".repeat(REPLAY_MAX));
        r.push("tail");
        assert_eq!(r.len(), REPLAY_MAX);
        assert!(r.text().ends_with("tail"));
        assert!(r.text().starts_with('a'));
        let mut r = Replay::default();
        r.push(&"é".repeat(REPLAY_MAX / 2));
        r.push("x");
        assert!(r.len() <= REPLAY_MAX);
        assert!(r.text().ends_with('x'));
    }

    #[test]
    fn the_reader_pauses_past_one_mib_until_acked() {
        let g = Arc::new(FlowGate::default());
        g.sent(UNACKED_MAX + 10);
        let g2 = g.clone();
        let t = std::thread::spawn(move || {
            g2.wait_for_room(Duration::from_millis(5));
            g2.unacked()
        });
        std::thread::sleep(Duration::from_millis(50));
        assert!(!t.is_finished(), "still paused");
        g.ack(20);
        assert!(t.join().unwrap() <= UNACKED_MAX);
        g.sent(UNACKED_MAX * 2);
        g.close();
        g.wait_for_room(Duration::from_millis(5)); // returns at once once closed
    }

    #[test]
    fn utf16_units_count_surrogate_pairs_twice() {
        assert_eq!(utf16_units("a€😀"), 4);
    }
}
