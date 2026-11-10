//! What the phone did not hear of the voice chat's answers, kept for its next message.
//!
//! The phone reports what it played of each answer (`voice_heard`): all of it, cut short at some
//! sentence, or none. Every answer that goes up is noted here first ([`HeardStore::sent`]), so a
//! report can be told apart from one about an answer this PC never made. An answer heard in full
//! is forgotten; one that was cut short or never played stays, and the next message from the
//! phone carries up to [`MAX_NOTES`] lines saying so ([`HeardStore::pending`]), after which they
//! are cleared ([`HeardStore::clear`]). So the chat can say "as I was saying" instead of going on
//! as if everything was heard. A report never wakes the chat.
//!
//! The file (`relay/voice-heard.json`) holds the latest report per answer, at most
//! [`MAX_ENTRIES`] answers, none older than [`MAX_AGE_MS`].

use crate::telegram::write_private;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// The most answers remembered.
pub const MAX_ENTRIES: usize = 50;
/// How long an answer is remembered.
pub const MAX_AGE_MS: i64 = 24 * 60 * 60 * 1000;
/// The most lines one message carries.
pub const MAX_NOTES: usize = 3;
/// How many of an answer's first words name it in a line.
pub const LEAD_WORDS: usize = 8;
/// The most reports taken from one frame.
const MAX_REPORTS: usize = 50;
const FILE_CAP: u64 = 256 * 1024;
const NOTE_START: &str = "[Not heard by the user: ";

/// The file is read, changed and written by the mirror, the report handler and the delivery, one
/// at a time.
static LOCK: Mutex<()> = Mutex::new(());

/// How an answer fell short of being heard.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Missed {
    /// Playing stopped partway.
    #[serde(rename = "partial")]
    Partial,
    /// It was never played.
    #[serde(rename = "none")]
    Unplayed,
}

/// One answer that went up.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Entry {
    /// The answer's reply id.
    pub reply: String,
    /// The conversation it belongs to: a note for another conversation means nothing to the chat.
    pub conversation: String,
    /// Its first words, ready to quote.
    pub lead: String,
    /// How many sentences it went up as.
    pub sentences: u32,
    pub sent_at: i64,
    /// What the phone last said about it; `None` until it has said anything.
    #[serde(default)]
    pub missed: Option<Missed>,
    /// For a partial report: the sentence being spoken when it stopped.
    #[serde(default)]
    pub sentence: u32,
    /// The phone's own time for that report.
    #[serde(default)]
    pub reported_at: i64,
}

#[derive(Default, Serialize, Deserialize)]
struct HeardFile {
    entries: Vec<Entry>,
}

/// The lines to put in front of the next message, and the answers they are about.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Pending {
    /// Bracketed single lines, oldest first.
    pub lines: Vec<String>,
    /// Every answer with something missed at the time, to clear once the message is stored.
    pub ids: Vec<String>,
}

pub fn path(profile_dir: &Path) -> PathBuf {
    profile_dir.join("relay").join("voice-heard.json")
}

/// Up to [`LEAD_WORDS`] words of `text`, made safe to put between quotes inside one bracketed
/// line, with an ellipsis when there were more.
pub fn lead_of(text: &str) -> String {
    let mut words = text.split_whitespace();
    let head: Vec<&str> = words.by_ref().take(LEAD_WORDS).collect();
    let mut out: String = head
        .join(" ")
        .chars()
        .filter(|c| !matches!(c, '[' | ']') && !c.is_control())
        .map(|c| if c == '"' { '\'' } else { c })
        .collect();
    if words.next().is_some() {
        out.push('…');
    }
    out
}

/// The line the chat is given for an answer that fell short.
pub fn note_line(e: &Entry) -> Option<String> {
    match e.missed? {
        Missed::Partial => Some(format!(
            "{NOTE_START}your reply \"{}\" stopped after sentence {} of {}]",
            e.lead, e.sentence, e.sentences
        )),
        Missed::Unplayed => Some(format!(
            "{NOTE_START}your reply \"{}\" was never played]",
            e.lead
        )),
    }
}

/// Forget what is too old, then what is over the limit: answers nothing was said about go first,
/// oldest first, since those cost nothing to lose.
fn prune(entries: &mut Vec<Entry>, now: i64) {
    entries.retain(|e| now - e.sent_at < MAX_AGE_MS);
    while entries.len() > MAX_ENTRIES {
        let at = entries.iter().position(|e| e.missed.is_none()).unwrap_or(0);
        entries.remove(at);
    }
}

#[derive(Clone, Debug)]
pub struct HeardStore {
    path: PathBuf,
}

impl HeardStore {
    pub fn new(path: PathBuf) -> Self {
        HeardStore { path }
    }

    /// The store in a profile folder.
    pub fn at(profile_dir: &Path) -> Self {
        Self::new(path(profile_dir))
    }

    fn load(&self) -> HeardFile {
        helm_core::privfs::read_capped(&self.path, FILE_CAP)
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default()
    }

    /// Read, change and write back, one caller at a time. The file is only written when the
    /// change made a difference.
    fn edit<R>(&self, now: i64, f: impl FnOnce(&mut Vec<Entry>) -> R) -> R {
        let _guard = LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let mut file = self.load();
        let before = file.entries.clone();
        prune(&mut file.entries, now);
        let out = f(&mut file.entries);
        prune(&mut file.entries, now);
        if file.entries != before && write_private(&self.path, &file).is_err() {
            log::warn!("relay: what the phone heard could not be saved");
        }
        out
    }

    /// An answer is going up (or is offered again: the second time changes nothing). Answers
    /// with no sentences were not spoken, so nothing can be said of what was heard.
    pub fn sent(&self, reply: &str, conversation: &str, text: &str, sentences: usize, now: i64) {
        if reply.is_empty() || sentences == 0 {
            return;
        }
        let entry = Entry {
            reply: reply.to_owned(),
            conversation: conversation.to_owned(),
            lead: lead_of(text),
            sentences: u32::try_from(sentences).unwrap_or(u32::MAX),
            sent_at: now,
            missed: None,
            sentence: 0,
            reported_at: 0,
        };
        self.edit(now, |entries| {
            if !entries.iter().any(|e| e.reply == entry.reply) {
                entries.push(entry);
            }
        });
    }

    /// Take in the phone's reports. Returns how many changed what is remembered. A report about an
    /// answer this PC did not make says nothing to the chat and is dropped; so is one older than
    /// the latest already seen for the same answer.
    pub fn report(&self, reports: &[Value], now: i64) -> usize {
        self.edit(now, |entries| {
            let mut changed = 0;
            for r in reports.iter().take(MAX_REPORTS) {
                let Some(id) = r.get("reply").and_then(Value::as_str) else {
                    continue;
                };
                let Some(i) = entries.iter().position(|e| e.reply == id) else {
                    continue;
                };
                let at = r.get("at").and_then(Value::as_i64).unwrap_or(now);
                if at < entries[i].reported_at {
                    continue;
                }
                match r.get("state").and_then(Value::as_str) {
                    Some("all") => {
                        entries.remove(i);
                    }
                    Some("partial") => {
                        let e = &mut entries[i];
                        let last = u64::from(e.sentences.max(1));
                        let said = r.get("sentence").and_then(Value::as_u64).unwrap_or(1);
                        e.missed = Some(Missed::Partial);
                        e.sentence = said.clamp(1, last) as u32;
                        e.reported_at = at;
                    }
                    Some("none") => {
                        let e = &mut entries[i];
                        e.missed = Some(Missed::Unplayed);
                        e.sentence = 0;
                        e.reported_at = at;
                    }
                    _ => continue,
                }
                changed += 1;
            }
            changed
        })
    }

    /// The lines for the next message in `conversation` (`None`: a new conversation is about to
    /// start, so no line applies): the latest [`MAX_NOTES`] answers it fell short on, oldest first.
    /// `ids` also holds the older ones and those of other conversations, which a message
    /// that carries lines clears too: they are no news by then.
    pub fn pending(&self, conversation: Option<&str>, now: i64) -> Pending {
        self.edit(now, |entries| {
            let missed: Vec<&Entry> = entries.iter().filter(|e| e.missed.is_some()).collect();
            let mut mine: Vec<&Entry> = missed
                .iter()
                .copied()
                .filter(|e| conversation == Some(e.conversation.as_str()))
                .collect();
            mine.sort_by_key(|e| e.sent_at);
            let skip = mine.len().saturating_sub(MAX_NOTES);
            Pending {
                lines: mine[skip..].iter().filter_map(|e| note_line(e)).collect(),
                ids: missed.iter().map(|e| e.reply.clone()).collect(),
            }
        })
    }

    /// Forget these answers: their lines went out with a message.
    pub fn clear(&self, ids: &[String], now: i64) {
        if ids.is_empty() {
            return;
        }
        self.edit(now, |entries| entries.retain(|e| !ids.contains(&e.reply)));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const T0: i64 = 1_800_000_000_000;
    const HOUR: i64 = 60 * 60 * 1000;

    #[test]
    fn reports_become_lines_for_the_next_message_once() {
        let dir = tempfile::tempdir().unwrap();
        let s = HeardStore::at(dir.path());
        let text = "Sure thing, the build finished and everything looks fine today.";

        // Nothing is kept for an answer with no sentences (chat mode) or no id.
        s.sent("chat-mode", "c1", "Read on screen.", 0, T0);
        s.sent("", "c1", "x", 3, T0);
        assert!(!path(dir.path()).exists());

        s.sent("r1", "c1", text, 5, T0);
        s.sent("r2", "c1", "Short one.", 2, T0 + 1_000);
        s.sent("r3", "c1", "Heard in full.", 1, T0 + 2_000);
        s.sent("r4", "c2", "Another conversation.", 1, T0 + 3_000);
        s.sent("r1", "c1", "offered again", 9, T0 + 4_000); // a resend changes nothing

        // (the report, what it should make of the answer)
        let rows: Vec<(Value, Option<(Missed, u32)>)> = vec![
            (json!({"reply": "r1", "state": "partial", "sentence": 2, "at": 10}), Some((Missed::Partial, 2))),
            // A sentence number past the end is held to the last one; none given means the first.
            (json!({"reply": "r1", "state": "partial", "sentence": 99, "at": 11}), Some((Missed::Partial, 5))),
            (json!({"reply": "r1", "state": "partial", "at": 12}), Some((Missed::Partial, 1))),
            // A report older than the latest seen is ignored.
            (json!({"reply": "r1", "state": "none", "at": 5}), Some((Missed::Partial, 1))),
            (json!({"reply": "r2", "state": "none", "at": 10}), Some((Missed::Unplayed, 0))),
            // Reports that mean nothing: an unknown answer, an unknown state, no id.
            (json!({"reply": "nobody-made-this", "state": "none", "at": 10}), None),
            (json!({"reply": "r3", "state": "sideways", "at": 10}), None),
            (json!({"state": "none"}), None),
        ];
        for (i, (report, want)) in rows.into_iter().enumerate() {
            let id = report.get("reply").and_then(Value::as_str).unwrap_or("-").to_owned();
            let n = s.report(&[report], T0 + 10_000);
            let got = s
                .load()
                .entries
                .into_iter()
                .find(|e| e.reply == id)
                .and_then(|e| e.missed.map(|m| (m, e.sentence)));
            if id == "r3" || id == "nobody-made-this" || id == "-" {
                assert_eq!((n, got), (0, None), "row {i}");
            } else {
                assert_eq!(got, want, "row {i}");
            }
        }

        // Heard in full: forgotten. Another conversation's miss never reaches this one.
        assert_eq!(s.report(&[json!({"reply": "r3", "state": "all", "at": 20})], T0 + 10_000), 1);
        s.report(&[json!({"reply": "r4", "state": "none", "at": 20})], T0 + 10_000);
        let p = s.pending(Some("c1"), T0 + 20_000);
        assert_eq!(
            p.lines,
            vec![
                "[Not heard by the user: your reply \"Sure thing, the build finished and everything looks…\" stopped after sentence 1 of 5]".to_owned(),
                "[Not heard by the user: your reply \"Short one.\" was never played]".to_owned(),
            ]
        );
        let mut ids = p.ids.clone();
        ids.sort();
        assert_eq!(ids, ["r1", "r2", "r4"]);
        // The lines go out with a message, and are cleared once it is stored.
        s.clear(&p.ids, T0 + 20_000);
        assert_eq!(s.pending(Some("c1"), T0 + 21_000), Pending::default());
        assert_eq!(s.load().entries.len(), 0);

        // A new conversation carries nothing, and clears what it had no use for.
        s.sent("r5", "c1", "Old.", 1, T0 + 30_000);
        s.report(&[json!({"reply": "r5", "state": "none", "at": 31})], T0 + 31_000);
        let p = s.pending(None, T0 + 32_000);
        assert_eq!((p.lines.len(), p.ids.len()), (0, 1));
    }

    #[test]
    fn at_most_three_lines_the_latest_and_the_file_is_bounded_and_ages_out() {
        let dir = tempfile::tempdir().unwrap();
        let s = HeardStore::at(dir.path());
        for i in 0..5 {
            let id = format!("r{i}");
            s.sent(&id, "c1", &format!("Answer number {i}."), 2, T0 + i * 1_000);
            s.report(&[json!({"reply": id, "state": "none", "at": 1})], T0 + 10_000);
        }
        let p = s.pending(Some("c1"), T0 + 20_000);
        let said: Vec<&str> = p.lines.iter().map(|l| l.split('"').nth(1).unwrap()).collect();
        assert_eq!(said, ["Answer number 2.", "Answer number 3.", "Answer number 4."]);
        assert_eq!(p.ids.len(), 5, "older ones are cleared with the rest");
        s.clear(&p.ids, T0 + 20_000);

        // Over the limit, answers nothing was said about go first.
        for i in 0..(MAX_ENTRIES + 10) {
            s.sent(&format!("u{i}"), "c1", "Unreported.", 1, T0 + 100_000 + i as i64);
        }
        s.sent("missed", "c1", "Missed.", 1, T0 + 99_000);
        s.report(&[json!({"reply": "missed", "state": "none", "at": 1})], T0 + 200_000);
        let kept = s.load().entries;
        assert_eq!(kept.len(), MAX_ENTRIES);
        assert!(kept.iter().any(|e| e.reply == "missed"));
        assert!(!kept.iter().any(|e| e.reply == "u0"));
        assert!(kept.iter().any(|e| e.reply == format!("u{}", MAX_ENTRIES + 9)));

        // Nothing is kept past a day, said about or not.
        assert!(s.pending(Some("c1"), T0 + 200_000 + 24 * HOUR).lines.is_empty());
        assert!(s.load().entries.is_empty());
    }

    #[test]
    fn a_lead_is_eight_words_that_fit_in_one_quoted_bracketed_line() {
        let rows = [
            ("One two three four five six seven eight", "One two three four five six seven eight"),
            ("One two three four five six seven eight nine", "One two three four five six seven eight…"),
            ("  spaced \n\n out  ", "spaced out"),
            ("He said \"stop\" [now]", "He said 'stop' now"),
            ("", ""),
        ];
        for (text, want) in rows {
            assert_eq!(lead_of(text), want, "{text:?}");
        }
        // Whatever the answer says, the line stays one bracketed line the transcript can hide.
        let e = Entry {
            reply: "r".into(),
            conversation: "c".into(),
            lead: lead_of("]\nwhere is [this"),
            sentences: 3,
            sent_at: 0,
            missed: Some(Missed::Partial),
            sentence: 2,
            reported_at: 0,
        };
        let line = note_line(&e).unwrap();
        assert!(!line.contains('\n') && line.matches(']').count() == 1, "{line}");
        assert_eq!(note_line(&Entry { missed: None, ..e }), None);
    }
}
