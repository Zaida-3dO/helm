//! A parsed messaging log (Telegram/WhatsApp exports kept as weekly markdown-ish files) to
//! one doc. Every message sits on its own line shaped
//! `**2026-11-10 09:31** — **Pat**: text`; a line that is not of that shape continues the
//! previous message (multi-line pastes).
//!
//! Chunks are runs of consecutive messages up to [`MAX_CHUNK_CHARS`], broken whenever the
//! next message comes more than half an hour after the last one — a gap that size means the
//! conversation moved on, and a searcher wants the run a question sat in, not a week of
//! unrelated chat. Each chunk is timed and dated by its first message. Times on the log lines
//! are read as UTC; whoever exported the file chose its local clock.

use super::{file_stem, finish_doc, fmt_date, parse_date, ChunkOut, DocOut, MAX_CHUNK_CHARS};

/// Half an hour of silence starts a new chunk.
const GAP_MS: i64 = 30 * 60 * 1000;

/// One parsed message line.
struct LogMsg {
    at: i64,
    speaker: String,
    body: String,
}

/// Build a log doc from one weekly file. `key` is the file's name relative to the log
/// folder; the doc is titled by the file stem. Returns `None` when no line parses.
pub fn build_log_doc(key: &str, text: &str) -> Option<DocOut> {
    let msgs = parse_lines(text);
    if msgs.is_empty() {
        return None;
    }
    let title = file_stem(key);
    let mut chunks: Vec<ChunkOut> = Vec::new();
    let mut run: Vec<&LogMsg> = Vec::new();
    let mut run_len = 0usize;
    let mut run_start = 0usize;
    for (i, msg) in msgs.iter().enumerate() {
        let line_len = msg.speaker.chars().count() + msg.body.chars().count() + 4;
        let fresh = !run.is_empty()
            && (msg.at - run[run.len() - 1].at > GAP_MS || run_len + line_len > MAX_CHUNK_CHARS);
        if fresh {
            chunks.push(flush_run(&run, run_start));
            run.clear();
            run_len = 0;
        }
        if run.is_empty() {
            run_start = i;
        }
        run.push(msg);
        run_len += line_len;
    }
    if !run.is_empty() {
        chunks.push(flush_run(&run, run_start));
    }
    for (i, c) in chunks.iter_mut().enumerate() {
        c.ord = i;
    }
    Some(finish_doc(
        key.to_string(),
        title,
        key.to_string(),
        msgs.first()?.at,
        msgs.last()?.at,
        chunks,
    ))
}

fn flush_run(run: &[&LogMsg], start_index: usize) -> ChunkOut {
    let first = run[0];
    let body = run
        .iter()
        .map(|m| format!("{}: {}", m.speaker, m.body))
        .collect::<Vec<_>>()
        .join("\n");
    ChunkOut {
        ord: 0,
        heading: fmt_date(first.at),
        text: body,
        at: first.at,
        // the first message's position in the file, so a hit can point back at a line
        anchor: Some(start_index.to_string()),
    }
}

/// Parse `**YYYY-MM-DD HH:MM** — **Speaker**: body` lines; anything else continues the last.
fn parse_lines(text: &str) -> Vec<LogMsg> {
    let mut msgs: Vec<LogMsg> = Vec::new();
    for line in text.lines() {
        if let Some((at, speaker, body)) = parse_line(line) {
            msgs.push(LogMsg { at, speaker, body });
            continue;
        }
        if let Some(last) = msgs.last_mut() {
            if !line.trim().is_empty() {
                last.body.push('\n');
                last.body.push_str(line.trim_end());
            }
        }
    }
    msgs
}

fn parse_line(line: &str) -> Option<(i64, String, String)> {
    let rest = line.strip_prefix("**")?;
    let (stamp, rest) = rest.split_once("**")?;
    let stamp = stamp.trim();
    if stamp.len() < 16 {
        return None;
    }
    let at_day = parse_date(&stamp[0..10])?;
    let hh: u32 = stamp[11..13].parse().ok()?;
    let mm: u32 = stamp[14..16].parse().ok()?;
    if hh > 23 || mm > 59 {
        return None;
    }
    let at = at_day + hh as i64 * 3_600_000 + mm as i64 * 60_000;
    let rest = rest.trim_start();
    // an em dash or a plain hyphen between stamp and speaker
    let rest = rest
        .strip_prefix('—')
        .or_else(|| rest.strip_prefix('-'))?
        .trim_start();
    let rest = rest.strip_prefix("**")?;
    let (speaker, rest) = rest.split_once("**")?;
    let speaker = speaker.trim().to_string();
    let body = rest.trim().strip_prefix(':')?.trim_start().to_string();
    if speaker.is_empty() {
        return None;
    }
    Some((at, speaker, body))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(y: i32, mo: u32, d: u32, hh: u32, mi: u32) -> i64 {
        super::super::days_from_civil(y, mo, d) * 86_400_000
            + hh as i64 * 3_600_000
            + mi as i64 * 60_000
    }

    #[test]
    fn one_run_one_chunk() {
        let text = "**2026-11-10 09:00** — **Pat**: buying milk\n**2026-11-10 09:04** — **Bo**: get bread too\n";
        let d = build_log_doc("logs/2026-w45.md", text).unwrap();
        assert_eq!(d.title, "2026-w45");
        assert_eq!(d.chunks.len(), 1);
        assert_eq!(d.chunks[0].heading, "2026-11-10");
        assert_eq!(d.chunks[0].at, at(2026, 11, 10, 9, 0));
        assert_eq!(d.chunks[0].text, "Pat: buying milk\nBo: get bread too");
        assert_eq!(d.occurred_from, at(2026, 11, 10, 9, 0));
        assert_eq!(d.occurred_to, at(2026, 11, 10, 9, 4));
    }

    #[test]
    fn tables() {
        // a gap over half an hour breaks the run; exactly half an hour does not
        let text = "**2026-11-10 09:00** — **Pat**: a\n**2026-11-10 09:30** — **Bo**: b";
        let d = build_log_doc("w.md", text).unwrap();
        assert_eq!(d.chunks.len(), 1);
        let text = "**2026-11-10 09:00** — **Pat**: a\n**2026-11-10 09:31** — **Bo**: b";
        let d = build_log_doc("w.md", text).unwrap();
        assert_eq!(d.chunks.len(), 2);
        assert_eq!(d.chunks[1].anchor.as_deref(), Some("1"));

        // a long run breaks on size too
        let long = "x".repeat(1990);
        let text =
            format!("**2026-11-10 09:00** — **Pat**: {long}\n**2026-11-10 09:01** — **Bo**: tail");
        let d = build_log_doc("w.md", &text).unwrap();
        assert_eq!(d.chunks.len(), 2);
        for c in &d.chunks {
            assert!(c.text.chars().count() <= MAX_CHUNK_CHARS);
        }

        // continuation lines join the previous message
        let text = "**2026-11-10 09:00** — **Pat**: line one\nstill the same message\n**2026-11-10 09:02** — **Bo**: ok";
        let d = build_log_doc("w.md", text).unwrap();
        assert_eq!(
            d.chunks[0].text,
            "Pat: line one\nstill the same message\nBo: ok"
        );

        // a hyphen dash is accepted too, spaces around it are not required
        let text = "**2026-11-10 09:00**-**Pat**: hi";
        let d = build_log_doc("w.md", text).unwrap();
        assert_eq!(d.chunks[0].text, "Pat: hi");

        // nothing parses: no doc
        assert!(build_log_doc("w.md", "hello world\njust prose").is_none());
        // a bad stamp is a continuation, not a message
        let d = build_log_doc(
            "w.md",
            "**2026-13-99 99:99** — **Pat**: bad\n**2026-11-10 09:00** — **Bo**: real",
        )
        .unwrap();
        assert_eq!(
            d.chunks[0].text, "Bo: real",
            "the first line had no earlier message to join"
        );

        // hash stability
        let a = build_log_doc("w.md", "**2026-11-10 09:00** — **Pat**: a").unwrap();
        let b = build_log_doc("w.md", "**2026-11-10 09:00** — **Pat**: a").unwrap();
        assert_eq!(a.hash, b.hash);
        let c = build_log_doc(
            "w.md",
            "**2026-11-10 09:00** — **Pat**: a\n**2026-11-10 10:00** — **Bo**: b",
        )
        .unwrap();
        assert_ne!(a.hash, c.hash);
    }
}
