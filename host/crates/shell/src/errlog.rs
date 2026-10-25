//! The renderer error log.
//!
//! The page reports an uncaught error, an unhandled promise rejection or an error a pane caught
//! (`ui_error_report`). Each report becomes one line in `<diagnostics>/renderer-errors.log`:
//! `unix_ms kind message | stack | source`, with line breaks inside a field written as `\n`.
//!
//! - Rate limit: at most [`PER_WINDOW`] lines per [`WINDOW_MS`]; the rest are dropped and counted,
//!   and the next line written says how many were dropped. The same kind and message again within
//!   [`REPEAT_MS`] is dropped too, so a render loop cannot fill the disk.
//! - Rotation: when the file reaches [`ROTATE_BYTES`] it becomes `renderer-errors.log.1` (replacing
//!   the previous one) and a new file starts, so the log never takes more than twice that.
//! - Fields are cut to fixed lengths and stripped of control characters; nothing else from the page
//!   is written.

use std::io::Write;
use std::path::Path;
use std::sync::Mutex;

pub const WINDOW_MS: i64 = 60_000;
pub const PER_WINDOW: usize = 20;
pub const REPEAT_MS: i64 = 10_000;
pub const ROTATE_BYTES: u64 = 2 * 1024 * 1024;
const MESSAGE_CHARS: usize = 1_000;
const STACK_CHARS: usize = 4_000;
const SOURCE_CHARS: usize = 300;
const KINDS: &[&str] = &["error", "rejection", "pane"];

/// What the rate limiter remembers between reports.
#[derive(Default)]
pub struct ErrorLog {
    state: Mutex<State>,
}

#[derive(Default)]
struct State {
    window_start: i64,
    in_window: usize,
    dropped: usize,
    last: Option<(String, i64)>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    Written,
    Dropped,
}

fn clean(s: &str, max: usize) -> String {
    s.chars()
        .take(max)
        .map(|c| match c {
            '\n' | '\r' => ' ',
            c if c.is_control() => '?',
            c => c,
        })
        .collect()
}

/// A stack keeps its line breaks as a visible `\n` so the entry stays on one line.
fn clean_stack(s: &str, max: usize) -> String {
    s.replace("\r\n", "\n")
        .chars()
        .take(max)
        .map(|c| match c {
            '\n' => "\\n".to_owned(),
            c if c.is_control() => "?".to_owned(),
            c => c.to_string(),
        })
        .collect()
}

impl ErrorLog {
    pub fn new() -> Self {
        Self::default()
    }

    /// Log one report into `dir`, unless the rate limit or the repeat check drops it.
    pub fn record(
        &self,
        dir: &Path,
        now: i64,
        kind: &str,
        message: &str,
        stack: &str,
        source: &str,
    ) -> std::io::Result<Outcome> {
        let kind = if KINDS.contains(&kind) { kind } else { "error" };
        let message = clean(message, MESSAGE_CHARS);
        let dropped_before = {
            let mut s = self.state.lock().unwrap_or_else(|p| p.into_inner());
            if now - s.window_start >= WINDOW_MS || now < s.window_start {
                s.window_start = now;
                s.in_window = 0;
            }
            let key = format!("{kind} {message}");
            let repeat = matches!(&s.last, Some((k, at)) if *k == key && now - at < REPEAT_MS);
            if repeat {
                return Ok(Outcome::Dropped);
            }
            if s.in_window >= PER_WINDOW {
                s.dropped += 1;
                return Ok(Outcome::Dropped);
            }
            s.in_window += 1;
            s.last = Some((key, now));
            std::mem::take(&mut s.dropped)
        };
        helm_core::privfs::create_private_dir_all(dir)?;
        let path = dir.join("renderer-errors.log");
        if std::fs::metadata(&path).is_ok_and(|m| m.len() >= ROTATE_BYTES) {
            // A failed rename must not stop logging; the file then just keeps growing this once.
            let _ = std::fs::rename(&path, dir.join("renderer-errors.log.1"));
        }
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)?;
        if dropped_before > 0 {
            writeln!(
                f,
                "{now} note {dropped_before} reports were dropped by the rate limit"
            )?;
        }
        writeln!(
            f,
            "{now} {kind} {message} | {} | {}",
            clean_stack(stack, STACK_CHARS),
            clean(source, SOURCE_CHARS)
        )?;
        Ok(Outcome::Written)
    }
}

static GLOBAL: std::sync::OnceLock<ErrorLog> = std::sync::OnceLock::new();

/// The process-wide log the `ui_error_report` op writes through.
pub fn global() -> &'static ErrorLog {
    GLOBAL.get_or_init(ErrorLog::new)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(dir: &Path) -> Vec<String> {
        std::fs::read_to_string(dir.join("renderer-errors.log"))
            .unwrap_or_default()
            .lines()
            .map(str::to_owned)
            .collect()
    }

    #[test]
    fn lines_are_cleaned_repeats_and_floods_are_dropped_and_the_file_rotates() {
        let dir = tempfile::tempdir().unwrap();
        let d = dir.path();
        let log = ErrorLog::new();
        let w = |t, kind, msg: &str| {
            log.record(d, t, kind, msg, "at a\r\nat b", "app.js:1:2")
                .unwrap()
        };

        // One line per report, line breaks kept visible, an unknown kind read as error.
        assert_eq!(
            w(1_000, "bogus", "boom\nsecond line\u{7}"),
            Outcome::Written
        );
        assert_eq!(
            lines(d),
            ["1000 error boom second line? | at a\\nat b | app.js:1:2"]
        );

        // The same message again within 10 s is dropped; a different one, or the same later, is not.
        assert_eq!(w(2_000, "error", "boom second line?"), Outcome::Dropped);
        assert_eq!(w(2_000, "rejection", "other"), Outcome::Written);
        assert_eq!(w(12_000, "rejection", "other"), Outcome::Written);

        // 20 per minute, then the rest are dropped and the next written line says how many.
        for i in 0..40 {
            w(20_000 + i, "pane", &format!("flood {i}"));
        }
        assert_eq!(
            lines(d).len(),
            3 + 17,
            "20 in the window, 3 already written before the flood"
        );
        assert_eq!(
            w(20_000 + WINDOW_MS, "pane", "after the minute"),
            Outcome::Written
        );
        let all = lines(d);
        assert!(
            all[all.len() - 2].contains("23 reports were dropped"),
            "{:?}",
            &all[all.len() - 2..]
        );

        // Past 2 MiB the file becomes .1 and a new one starts.
        std::fs::write(
            d.join("renderer-errors.log"),
            vec![b'x'; ROTATE_BYTES as usize],
        )
        .unwrap();
        assert_eq!(w(500_000, "error", "fresh"), Outcome::Written);
        assert_eq!(lines(d).len(), 1);
        assert_eq!(
            std::fs::metadata(d.join("renderer-errors.log.1"))
                .unwrap()
                .len(),
            ROTATE_BYTES
        );
    }
}
