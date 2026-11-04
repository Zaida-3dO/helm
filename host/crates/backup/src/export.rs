//! Writing one chat out as a Markdown file anyone can read without Helm.

use crate::types::Exported;
use crate::{stamp, stamp_human, BackupError};
use rusqlite::{Connection, OptionalExtension};
use std::path::{Path, PathBuf};

/// The largest artifact read back for an export (the file holds the text and the thinking).
const ARTIFACT_READ_CAP: u64 = 256 * 1024 * 1024;

struct Row {
    role: String,
    tool: Option<String>,
    text: String,
}

/// Render the chat's current line of messages (the branch in use, in order) and write it to a new
/// file in `dest_dir`, named after the chat and the moment. Thinking is left out; rows too big for
/// the database row are read back whole from the artifacts folder.
pub fn export_markdown(
    conn: &Connection,
    artifacts: &Path,
    chat_id: &str,
    dest_dir: &Path,
    now_ms: i64,
) -> Result<Exported, BackupError> {
    let head: Option<(String, Option<String>, i64)> = conn
        .query_row(
            "SELECT title, model, created_at FROM chats WHERE id = ?1",
            [chat_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?;
    let Some((title, model, created_at)) = head else {
        return Err(BackupError::ChatNotFound);
    };

    let mut stmt = conn.prepare(
        "SELECT role, tool_name, text, artifact FROM messages \
         WHERE chat_id = ?1 AND active = 1 ORDER BY timestamp, id",
    )?;
    let rows = stmt.query_map([chat_id], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, Option<String>>(1)?,
            r.get::<_, String>(2)?,
            r.get::<_, Option<String>>(3)?,
        ))
    })?;
    let mut messages = Vec::new();
    for row in rows {
        let (role, tool, mut text, artifact) = row?;
        if let Some(full) = artifact
            .as_deref()
            .and_then(|n| read_artifact(artifacts, n))
        {
            text = full;
        }
        if text.trim().is_empty() {
            continue;
        }
        messages.push(Row { role, tool, text });
    }

    let md = render(&title, model.as_deref(), created_at, now_ms, &messages);
    std::fs::create_dir_all(dest_dir)?;
    let base = format!("{} {}", file_stem(&title), stamp(now_ms));
    let path = write_new(dest_dir, &base, md.as_bytes())?;
    Ok(Exported {
        path: path.to_string_lossy().into_owned(),
        messages: messages.len() as i64,
    })
}

/// The whole text of a row whose content moved to an artifact. The name was made by Helm
/// (`<sha256>.json`); anything else is never opened.
fn read_artifact(dir: &Path, name: &str) -> Option<String> {
    if name.len() >= 100 || !name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'.') {
        return None;
    }
    let raw = helm_core::privfs::read_capped(&dir.join(name), ARTIFACT_READ_CAP).ok()?;
    let v: serde_json::Value = serde_json::from_slice(&raw).ok()?;
    v.get("text")?.as_str().map(str::to_owned)
}

fn render(title: &str, model: Option<&str>, created_at: i64, now_ms: i64, rows: &[Row]) -> String {
    let heading = if title.trim().is_empty() {
        "Untitled chat"
    } else {
        title.trim()
    };
    let mut out = format!("# {}\n\n", heading.replace(['\r', '\n'], " "));
    out.push_str(&format!("Chat started {}", stamp_human(created_at)));
    if let Some(m) = model.filter(|m| !m.is_empty()) {
        out.push_str(&format!(" · model {m}"));
    }
    out.push_str(&format!(" · exported from Helm {}\n", stamp_human(now_ms)));
    for r in rows {
        out.push_str("\n---\n\n");
        match r.role.as_str() {
            "user" => {
                out.push_str("## You\n\n");
                out.push_str(r.text.trim_end());
                out.push('\n');
            }
            "toolResult" => {
                let name = r
                    .tool
                    .as_deref()
                    .filter(|t| !t.is_empty())
                    .unwrap_or("tool");
                out.push_str(&format!("### Tool result: {}\n\n", name.replace('\n', " ")));
                let fence = "`".repeat((longest_run(&r.text, '`') + 1).max(3));
                out.push_str(&format!("{fence}\n{}\n{fence}\n", r.text.trim_end()));
            }
            "context" => {
                for line in r.text.trim_end().lines() {
                    out.push_str(&format!("> {line}\n"));
                }
            }
            _ => {
                out.push_str("## Assistant\n\n");
                out.push_str(r.text.trim_end());
                out.push('\n');
            }
        }
    }
    out
}

fn longest_run(s: &str, c: char) -> usize {
    let (mut best, mut cur) = (0, 0);
    for ch in s.chars() {
        cur = if ch == c { cur + 1 } else { 0 };
        best = best.max(cur);
    }
    best
}

/// A chat title made safe to be a file name on Windows and elsewhere.
fn file_stem(title: &str) -> String {
    let cleaned: String = title
        .chars()
        .map(|c| {
            if c.is_control() || "<>:\"/\\|?*".contains(c) {
                '-'
            } else {
                c
            }
        })
        .collect();
    let mut s: String = cleaned
        .trim()
        .trim_end_matches(['.', ' '])
        .chars()
        .take(80)
        .collect();
    s = s.trim_end_matches(['.', ' ']).to_owned();
    if s.is_empty() {
        s = "chat".to_owned();
    }
    let lower = s.to_lowercase();
    let reserved = ["con", "prn", "aux", "nul"].iter().any(|r| lower == *r)
        || (lower.len() == 4
            && (lower.starts_with("com") || lower.starts_with("lpt"))
            && lower.as_bytes()[3].is_ascii_digit());
    if reserved {
        s = format!("chat {s}");
    }
    s
}

/// Create `<base>.md` (or `<base> 2.md`, ...) without ever overwriting a file.
fn write_new(dir: &Path, base: &str, bytes: &[u8]) -> Result<PathBuf, BackupError> {
    use std::io::Write;
    let mut name = format!("{base}.md");
    for i in 2.. {
        let p = dir.join(&name);
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&p)
        {
            Ok(mut f) => {
                f.write_all(bytes)?;
                return Ok(p);
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                name = format!("{base} {i}.md");
            }
            Err(e) => return Err(e.into()),
        }
    }
    unreachable!("the loop always returns or tries the next name")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_chat_exports_as_readable_markdown_and_never_overwrites() {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "CREATE TABLE chats(id TEXT PRIMARY KEY, title TEXT, model TEXT, created_at INTEGER);
             CREATE TABLE messages(id INTEGER PRIMARY KEY, chat_id TEXT, role TEXT, tool_name TEXT,
                 text TEXT, artifact TEXT, thinking TEXT, active INTEGER, timestamp INTEGER);
             INSERT INTO chats VALUES ('c1', 'Plan: the <big> trip?', 'sonnet', 1760000000000);
             INSERT INTO chats VALUES ('c2', 'con', NULL, 1760000000000);
             INSERT INTO messages VALUES (1, 'c1', 'user', NULL, 'Where to?', NULL, '', 1, 1);
             INSERT INTO messages VALUES (2, 'c1', 'assistant', NULL, 'Lisbon.', NULL, 'secret thoughts', 1, 2);
             INSERT INTO messages VALUES (3, 'c1', 'toolResult', 'Bash', 'ls\n```\nx', NULL, '', 1, 3);
             INSERT INTO messages VALUES (4, 'c1', 'assistant', NULL, 'a branch not taken', NULL, '', 0, 4);
             INSERT INTO messages VALUES (5, 'c1', 'context', NULL, 'Compacted.', NULL, '', 1, 5);
             INSERT INTO messages VALUES (6, 'c1', 'assistant', NULL, '   ', NULL, '', 1, 6);
             INSERT INTO messages VALUES (7, 'c1', 'assistant', NULL, 'cut', 'abc.json', '', 1, 7);",
        )
        .unwrap();
        let arts = tempfile::tempdir().unwrap();
        std::fs::write(
            arts.path().join("abc.json"),
            r#"{"text":"the whole thing"}"#,
        )
        .unwrap();
        let dest = tempfile::tempdir().unwrap();
        let out = dest.path().join("exports");

        let a = export_markdown(&c, arts.path(), "c1", &out, 1_760_000_100_000).unwrap();
        assert_eq!(a.messages, 5); // blank and branch rows are left out
        let text = std::fs::read_to_string(&a.path).unwrap();
        assert!(text.starts_with("# Plan: the <big> trip?\n"));
        assert!(text.contains("model sonnet"));
        assert!(text.contains("## You\n\nWhere to?"));
        assert!(text.contains("## Assistant\n\nLisbon."));
        assert!(text.contains("### Tool result: Bash\n\n````\nls\n```\nx\n````"));
        assert!(text.contains("> Compacted."));
        assert!(text.contains("the whole thing"));
        assert!(!text.contains("secret thoughts"));
        assert!(!text.contains("a branch not taken"));
        // The file name is safe on Windows.
        let name = Path::new(&a.path)
            .file_name()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        assert!(
            name.starts_with("Plan- the -big- trip") && name.ends_with(".md"),
            "{name}"
        );

        // Again in the same second: a second file, never an overwrite.
        let b = export_markdown(&c, arts.path(), "c1", &out, 1_760_000_100_000).unwrap();
        assert_ne!(a.path, b.path);
        // A reserved device name is not used as a bare file name; a missing chat is refused.
        let r = export_markdown(&c, arts.path(), "c2", &out, 1_760_000_100_000).unwrap();
        assert!(Path::new(&r.path)
            .file_name()
            .unwrap()
            .to_string_lossy()
            .starts_with("chat con"));
        assert!(matches!(
            export_markdown(&c, arts.path(), "nope", &out, 1),
            Err(BackupError::ChatNotFound)
        ));
    }
}
