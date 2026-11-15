//! A chat's message rows to one doc: one chunk per turn — the ask and the final answer that
//! followed it, the way a person remembers an exchange.
//!
//! The rows come in as the chat's own user and assistant messages in time order; everything
//! in between (a running plan, a note mid-task, tool traffic) stays out here — it belongs to
//! the same chat's drill-down, which answers from the local database. A reply longer than
//! [`MAX_CHUNK_CHARS`] is split, and every piece repeats the first [`ASK_PREFIX_CHARS`]
//! characters of the ask so each piece can stand on its own. The doc opens with a header
//! chunk — title, project, date range, turn count, the first ask — which is what makes a
//! whole session findable without any summarising.

use super::{
    finish_doc, fmt_date, wrap_capped, ChunkOut, DocOut, ASK_PREFIX_CHARS, MAX_CHUNK_CHARS,
};

/// Which side of the exchange a row is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MsgRole {
    User,
    Assistant,
}

/// One message as the chat database has it: id, side, instant, text.
pub struct MsgRow {
    pub id: i64,
    pub role: MsgRole,
    pub at_ms: i64,
    pub text: String,
}

/// Build a chat doc. `key` is the chat's id (kept as the locator too); the first ask's id is
/// every turn's anchor. Returns `None` when there is no user message at all: an assistant
/// row with nothing asked is nothing to search.
pub fn build_chat_doc(key: &str, title: &str, project: &str, rows: &[MsgRow]) -> Option<DocOut> {
    let turns = turns_of(rows);
    if turns.is_empty() {
        return None;
    }
    let first_at = rows.iter().map(|r| r.at_ms).min().unwrap_or(0);
    let last_at = rows.iter().map(|r| r.at_ms).max().unwrap_or(0);
    let first_ask = turns[0].ask.trim();
    let header = format!(
        "{title}\n{project}\n{n} turns, {from} to {to}\nFirst ask: {ask}",
        n = turns.len(),
        from = fmt_date(first_at),
        to = fmt_date(last_at),
        ask = clip(first_ask, ASK_PREFIX_CHARS),
    );
    let mut chunks = vec![ChunkOut {
        ord: 0,
        heading: String::new(),
        text: header,
        at: first_at,
        anchor: None,
    }];

    for turn in &turns {
        let ask = clip(turn.ask.trim(), 1500);
        let reply = turn.reply.trim();
        let anchor = Some(turn.ask_id.to_string());
        if reply.is_empty() {
            chunks.push(ChunkOut {
                ord: 0,
                heading: String::new(),
                text: ask,
                at: turn.at,
                anchor,
            });
            continue;
        }
        let whole = format!("{ask}\n\n{reply}");
        if whole.chars().count() <= MAX_CHUNK_CHARS {
            chunks.push(ChunkOut {
                ord: 0,
                heading: String::new(),
                text: whole,
                at: turn.at,
                anchor,
            });
            continue;
        }
        let prefix = format!("{}\n\n", clip(turn.ask.trim(), ASK_PREFIX_CHARS));
        let cap = (MAX_CHUNK_CHARS - prefix.chars().count()).max(1);
        for piece in wrap_capped(reply, cap) {
            chunks.push(ChunkOut {
                ord: 0,
                heading: String::new(),
                text: format!("{prefix}{piece}"),
                at: turn.at,
                anchor: anchor.clone(),
            });
        }
    }
    Some(finish_doc(
        key.to_string(),
        title.to_string(),
        key.to_string(),
        first_at,
        last_at,
        chunks,
    ))
}

/// One turn: the ask, the final assistant row before the next ask, and the ask's instant.
struct Turn<'a> {
    ask_id: i64,
    at: i64,
    ask: &'a str,
    reply: &'a str,
}

fn turns_of(rows: &[MsgRow]) -> Vec<Turn<'_>> {
    let mut turns: Vec<Turn<'_>> = Vec::new();
    for row in rows {
        match row.role {
            MsgRole::User => turns.push(Turn {
                ask_id: row.id,
                at: row.at_ms,
                ask: &row.text,
                reply: "",
            }),
            MsgRole::Assistant => {
                // The last non-empty assistant row before the next ask is the answer; each
                // later one replaces the one before it.
                if let Some(turn) = turns.last_mut() {
                    if !row.text.trim().is_empty() {
                        turn.reply = &row.text;
                    }
                }
            }
        }
    }
    turns
}

fn clip(s: &str, chars: usize) -> String {
    if s.chars().count() <= chars {
        return s.to_string();
    }
    let mut out: String = s.chars().take(chars).collect();
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(id: i64, role: MsgRole, at_ms: i64, text: &str) -> MsgRow {
        MsgRow {
            id,
            role,
            at_ms,
            text: text.to_string(),
        }
    }

    #[test]
    fn one_turn_per_ask_with_the_final_reply() {
        let rows = [
            row(1, MsgRole::User, 100, "how do we retry?"),
            row(2, MsgRole::Assistant, 150, "let me check the code."),
            row(3, MsgRole::Assistant, 200, "with a backoff timer."),
            row(4, MsgRole::User, 300, "and the hub?"),
            row(5, MsgRole::Assistant, 350, "same over the line."),
        ];
        let doc = build_chat_doc("c1", "Retry chat", "Notebook", &rows).unwrap();
        // header + two turns
        assert_eq!(doc.chunks.len(), 3);
        assert!(doc.chunks[0].text.contains("2 turns"));
        assert!(doc.chunks[0].text.contains("First ask: how do we retry?"));
        assert!(doc.chunks[0].text.contains("Notebook"));
        // intermediate narration stays out; the final answer is in
        assert_eq!(
            doc.chunks[1].text,
            "how do we retry?\n\nwith a backoff timer."
        );
        assert_eq!(doc.chunks[1].anchor.as_deref(), Some("1"));
        assert_eq!(doc.chunks[1].at, 100);
        assert_eq!(doc.chunks[2].text, "and the hub?\n\nsame over the line.");
        assert_eq!(doc.chunks[2].anchor.as_deref(), Some("4"));
        assert_eq!(doc.occurred_from, 100);
        assert_eq!(doc.occurred_to, 350);
    }

    #[test]
    fn tables() {
        // an ask with no reply still gets its chunk
        let rows = [row(1, MsgRole::User, 5, "asking alone")];
        let d = build_chat_doc("c", "t", "p", &rows).unwrap();
        assert_eq!(d.chunks.len(), 2);
        assert_eq!(d.chunks[1].text, "asking alone");

        // assistant-only chat: nothing to search
        let rows = [row(1, MsgRole::Assistant, 5, "unsolicited")];
        assert!(build_chat_doc("c", "t", "p", &rows).is_none());

        // empty assistant rows do not become the reply
        let rows = [
            row(1, MsgRole::User, 1, "q"),
            row(2, MsgRole::Assistant, 2, "   "),
            row(3, MsgRole::Assistant, 3, "real"),
        ];
        let d = build_chat_doc("c", "t", "p", &rows).unwrap();
        assert_eq!(d.chunks[1].text, "q\n\nreal");

        // a long reply splits, every piece repeats the ask prefix, all pieces stay under the cap
        let reply = "long answer word ".repeat(200); // 3200 chars
        let rows = [
            row(1, MsgRole::User, 1, "please explain everything"),
            row(2, MsgRole::Assistant, 2, &reply),
        ];
        let d = build_chat_doc("c", "t", "p", &rows).unwrap();
        assert!(
            d.chunks.len() >= 3,
            "split expected, got {}",
            d.chunks.len()
        );
        for c in &d.chunks[1..] {
            assert!(c.text.chars().count() <= MAX_CHUNK_CHARS);
            assert!(c.text.starts_with("please explain everything\n\n"));
            assert_eq!(c.anchor.as_deref(), Some("1"));
        }

        // the prefix itself is clipped at ASK_PREFIX_CHARS
        let ask = "x".repeat(500);
        let rows = [
            row(1, MsgRole::User, 1, &ask),
            row(2, MsgRole::Assistant, 2, &"y".repeat(3000)),
        ];
        let d = build_chat_doc("c", "t", "p", &rows).unwrap();
        for c in &d.chunks[2..] {
            let prefix: String = c.text.chars().take(ASK_PREFIX_CHARS + 2).collect();
            assert!(prefix.contains('…'), "prefix must be clipped: {prefix}");
        }

        // hash follows turns
        let rows = [
            row(1, MsgRole::User, 1, "one"),
            row(2, MsgRole::Assistant, 2, "two"),
        ];
        let a = build_chat_doc("c", "t", "p", &rows).unwrap();
        let b = build_chat_doc("c", "t", "p", &rows).unwrap();
        assert_eq!(a.hash, b.hash);
    }
}
