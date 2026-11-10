//! The household's knowledge files: the rules, skills and agents every Helm keeps in folders,
//! stored here so each person's layer reaches every device.
//!
//! A *layer* is `everyone` or `person:<personId>`. A file is a relative path under `rules/`,
//! `skills/<name>/` or `agents/` in a layer, up to 2 MiB; a layer holds up to 64 MiB and
//! [`MAX_LAYER_FILES`] files. Every change gets the next number of one hub-wide counter (`seq`),
//! so a Helm asks for "everything after N" and applies the answer in order. A deleted file stays
//! as a row without a body (a tombstone), so a deletion reaches devices that were off.
//!
//! Frames on the relay line (all need a token with the `peer` scope):
//!
//! | Helm → hub | hub → Helm |
//! |---|---|
//! | `kn_pull {id, since, limit}` | `kn_files {id, files, cursor, more}`: the files with `seq > since`, oldest first, at most `limit` (200 at most) and about 1 MiB of frame; one bigger file is sent alone. `cursor` is the last `seq` sent; ask again from it while `more` is true. A file is `{layer, path, seq, sha256, size, deleted, body}`; `body` is base64 and left out when `deleted` |
//! | `kn_put {id, layer, path, baseSeq, sha256, deleted, body}` | `kn_put_result {id, ok: true, seq}`, or `{id, ok: false, conflict: {seq, sha256, deleted}}` when the file changed since `baseSeq`, or `{id, ok: false, error}` (a plain sentence) |
//! | | `kn_changed {seq}` to every `peer` line after a change |
//!
//! `baseSeq` is the `seq` the writer last saw for the path, or 0 for a file it believes is new. A
//! put whose content equals what is stored is accepted as it is (no new `seq`, no `kn_changed`),
//! so two devices writing the same bytes never conflict. A path that was deleted may be written
//! again with `baseSeq` 0. The hub keeps the stored copy on a conflict; the writer decides what to
//! do with its own.
//!
//! Paths are refused when they are empty or over 200 bytes, have an empty segment, a segment that
//! starts with a dot or ends with a dot or space, hold `\ : * ? " < > |` or a control character,
//! name a Windows device (`con`, `nul`, `aux`, `prn`, `com1`..`com9`, `lpt1`..`lpt9`), or differ
//! from an existing path only in letter case: the files end up on Windows disks.

use crate::lines::Lines;
use base64::Engine as _;
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

pub const MAX_FILE: usize = 2 * 1024 * 1024;
pub const MAX_LAYER_BYTES: i64 = 64 * 1024 * 1024;
pub const MAX_LAYER_FILES: i64 = 5000;
pub const MAX_PATH: usize = 200;
pub const MAX_PULL_FILES: usize = 200;
/// A `kn_files` frame stops adding files at about this size.
pub const MAX_REPLY: usize = 1024 * 1024;
const MAX_DEPTH: usize = 8;

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS knowledge_files (
    layer      TEXT NOT NULL,
    path       TEXT NOT NULL,
    seq        INTEGER NOT NULL,
    sha256     TEXT NOT NULL,
    size       INTEGER NOT NULL,
    body       BLOB,
    deleted    INTEGER NOT NULL DEFAULT 0,
    device_uid TEXT,
    updated_at TEXT NOT NULL,
    PRIMARY KEY (layer, path)
);
CREATE UNIQUE INDEX IF NOT EXISTS knowledge_seq ON knowledge_files(seq);
";

const NOW: &str = "strftime('%Y-%m-%dT%H:%M:%fZ','now')";

/// Make the table. Called by the store when it opens.
pub fn init(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(SCHEMA)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Layer {
    Everyone,
    Person(i64),
}

/// `everyone` or `person:<id>` (an id written the plain way: no sign, no leading zeros).
pub fn parse_layer(s: &str) -> Option<Layer> {
    if s == "everyone" {
        return Some(Layer::Everyone);
    }
    let digits = s.strip_prefix("person:")?;
    let id: i64 = digits.parse().ok()?;
    (id > 0 && id.to_string() == digits).then_some(Layer::Person(id))
}

fn reserved_name(segment: &str) -> bool {
    let stem = segment.split('.').next().unwrap_or("").to_ascii_lowercase();
    matches!(stem.as_str(), "con" | "prn" | "aux" | "nul")
        || ["com", "lpt"].iter().any(|p| {
            stem.strip_prefix(p)
                .is_some_and(|n| matches!(n.as_bytes(), [b'1'..=b'9']))
        })
}

/// Whether `path` may be stored: see the module notes.
pub fn check_path(path: &str) -> Result<(), &'static str> {
    if path.is_empty() || path.len() > MAX_PATH {
        return Err("A path is 1 to 200 bytes.");
    }
    let segments: Vec<&str> = path.split('/').collect();
    if segments.len() > MAX_DEPTH {
        return Err("A path is at most 8 folders deep.");
    }
    for s in &segments {
        let bad_char = |c: char| c.is_control() || "\\:*?\"<>|".contains(c);
        if s.is_empty()
            || s.starts_with('.')
            || s.ends_with('.')
            || s.ends_with(' ')
            || s.chars().any(bad_char)
            || reserved_name(s)
        {
            return Err("That path has a name that cannot be stored as a file.");
        }
    }
    let shallowest = match segments[0] {
        "rules" | "agents" => 2,
        "skills" => 3,
        _ => return Err("Files live under rules/, skills/<name>/ or agents/."),
    };
    if segments.len() < shallowest {
        return Err("Files live under rules/, skills/<name>/ or agents/.");
    }
    Ok(())
}

/// A file to store, as a `kn_put` frame carries it.
pub struct Put<'a> {
    pub layer: &'a str,
    pub path: &'a str,
    pub base_seq: i64,
    pub sha256: &'a str,
    pub deleted: bool,
    /// Base64.
    pub body: Option<&'a str>,
    /// The device that wrote it.
    pub device_uid: Option<&'a str>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum PutOutcome {
    /// Stored (or already stored) as `seq`; `changed` is false when nothing was written.
    Saved {
        seq: i64,
        changed: bool,
    },
    /// The stored copy moved on since `baseSeq`.
    Conflict {
        seq: i64,
        sha256: String,
        deleted: bool,
    },
    Refused(String),
}

fn hex_sha256(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

/// Store one file, or say why not. Everything happens in one transaction.
pub fn put(conn: &mut Connection, p: &Put<'_>) -> rusqlite::Result<PutOutcome> {
    let refuse = |why: &str| Ok(PutOutcome::Refused(why.to_owned()));
    if parse_layer(p.layer).is_none() {
        return refuse("That is not a knowledge layer.");
    }
    if let Err(why) = check_path(p.path) {
        return refuse(why);
    }
    let (sha, bytes) = if p.deleted {
        (String::new(), Vec::new())
    } else {
        let Some(body) = p.body else {
            return refuse("A file needs a body.");
        };
        if body.len() > MAX_FILE / 3 * 4 + 4 {
            return refuse("A file is at most 2 MiB.");
        }
        let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(body) else {
            return refuse("The body is not base64.");
        };
        if bytes.len() > MAX_FILE {
            return refuse("A file is at most 2 MiB.");
        }
        let sha = hex_sha256(&bytes);
        if !p.sha256.is_empty() && !p.sha256.eq_ignore_ascii_case(&sha) {
            return refuse("The sha256 does not match the body.");
        }
        (sha, bytes)
    };

    let tx = conn.transaction()?;
    let existing: Option<(i64, String, bool)> = tx
        .query_row(
            "SELECT seq, sha256, deleted FROM knowledge_files WHERE layer = ?1 AND path = ?2",
            params![p.layer, p.path],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?;
    if let Some((seq, stored_sha, stored_deleted)) = &existing {
        if *stored_deleted == p.deleted && *stored_sha == sha {
            return Ok(PutOutcome::Saved {
                seq: *seq,
                changed: false,
            });
        }
        let known = p.base_seq == *seq || (*stored_deleted && p.base_seq == 0);
        if !known {
            return Ok(PutOutcome::Conflict {
                seq: *seq,
                sha256: stored_sha.clone(),
                deleted: *stored_deleted,
            });
        }
    }
    if !p.deleted {
        let case_clash = tx
            .prepare(
                "SELECT 1 FROM knowledge_files
                 WHERE layer = ?1 AND lower(path) = lower(?2) AND path <> ?2 AND deleted = 0",
            )?
            .exists(params![p.layer, p.path])?;
        if case_clash {
            return refuse("A file with that name, differing only in letter case, exists.");
        }
        let (used, files): (i64, i64) = tx.query_row(
            "SELECT COALESCE(SUM(size), 0), COUNT(*) FROM knowledge_files
             WHERE layer = ?1 AND deleted = 0 AND path <> ?2",
            params![p.layer, p.path],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        if used + bytes.len() as i64 > MAX_LAYER_BYTES || files + 1 > MAX_LAYER_FILES {
            return refuse("This layer is full.");
        }
    }
    let seq: i64 = tx.query_row(
        "SELECT COALESCE(MAX(seq), 0) + 1 FROM knowledge_files",
        [],
        |r| r.get(0),
    )?;
    let body: Option<&[u8]> = (!p.deleted).then_some(bytes.as_slice());
    tx.execute(
        &format!(
            "INSERT INTO knowledge_files
                 (layer, path, seq, sha256, size, body, deleted, device_uid, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, {NOW})
             ON CONFLICT(layer, path) DO UPDATE SET
                 seq = excluded.seq, sha256 = excluded.sha256, size = excluded.size,
                 body = excluded.body, deleted = excluded.deleted,
                 device_uid = excluded.device_uid, updated_at = excluded.updated_at"
        ),
        params![
            p.layer,
            p.path,
            seq,
            sha,
            bytes.len() as i64,
            body,
            p.deleted,
            p.device_uid
        ],
    )?;
    tx.commit()?;
    Ok(PutOutcome::Saved { seq, changed: true })
}

/// The files with `seq > since`, oldest first: the frame's `files`, its `cursor` and whether
/// `more` are left. At most `limit` files and about [`MAX_REPLY`] bytes; the first file is always
/// sent, however big.
pub fn pull(
    conn: &Connection,
    since: i64,
    limit: usize,
) -> rusqlite::Result<(Vec<Value>, i64, bool)> {
    let limit = limit.clamp(1, MAX_PULL_FILES);
    struct Meta {
        layer: String,
        path: String,
        seq: i64,
        sha256: String,
        size: i64,
        deleted: bool,
    }
    let metas: Vec<Meta> = conn
        .prepare(
            "SELECT layer, path, seq, sha256, size, deleted FROM knowledge_files
             WHERE seq > ?1 ORDER BY seq LIMIT ?2",
        )?
        .query_map(params![since, limit as i64 + 1], |r| {
            Ok(Meta {
                layer: r.get(0)?,
                path: r.get(1)?,
                seq: r.get(2)?,
                sha256: r.get(3)?,
                size: r.get(4)?,
                deleted: r.get(5)?,
            })
        })?
        .collect::<rusqlite::Result<_>>()?;
    let mut more = metas.len() > limit;
    let mut files = Vec::new();
    let mut cursor = since;
    let mut used = 0usize;
    for m in metas.into_iter().take(limit) {
        let estimate = 200
            + m.layer.len()
            + m.path.len()
            + if m.deleted {
                0
            } else {
                (m.size as usize).div_ceil(3) * 4
            };
        if !files.is_empty() && used + estimate > MAX_REPLY {
            more = true;
            break;
        }
        used += estimate;
        let mut file = json!({
            "layer": m.layer,
            "path": m.path,
            "seq": m.seq,
            "sha256": m.sha256,
            "size": m.size,
            "deleted": m.deleted,
        });
        if !m.deleted {
            let body: Vec<u8> = conn.query_row(
                "SELECT body FROM knowledge_files WHERE layer = ?1 AND path = ?2",
                params![m.layer, m.path],
                |r| r.get(0),
            )?;
            file["body"] = Value::String(base64::engine::general_purpose::STANDARD.encode(body));
        }
        cursor = m.seq;
        files.push(file);
    }
    Ok((files, cursor, more))
}

/// A `kn_put` frame's fields.
pub struct PutFrame<'a> {
    pub layer: &'a str,
    pub path: &'a str,
    pub base_seq: i64,
    pub sha256: &'a str,
    pub deleted: bool,
    pub body: Option<&'a str>,
}

const NEEDS_PEER: &str = "This machine's token cannot use the household.";
const NO_STORE: &str = "The hub's knowledge store is unavailable.";

/// The `kn_files` frame answering a `kn_pull` from the client `client_id`.
pub fn pull_frame(lines: &Lines, client_id: i64, id: &Value, since: i64, limit: usize) -> Value {
    let failed = |why: &str| json!({ "type": "kn_files", "id": id, "error": why });
    if !lines.is_peer(client_id) {
        return failed(NEEDS_PEER);
    }
    let Some(state) = lines.app_state() else {
        return failed(NO_STORE);
    };
    let pulled = match state.store.lock() {
        Ok(store) => pull(store.conn(), since, limit),
        Err(_) => return failed(NO_STORE),
    };
    match pulled {
        Ok((files, cursor, more)) => {
            json!({ "type": "kn_files", "id": id, "files": files, "cursor": cursor, "more": more })
        }
        Err(e) => failed(&format!("The knowledge store failed: {e}")),
    }
}

/// The `kn_put_result` frame answering a `kn_put`, and the new `seq` when the store changed (the
/// caller tells the other lines).
pub fn put_frame(
    lines: &Lines,
    client_id: i64,
    id: &Value,
    f: &PutFrame<'_>,
) -> (Value, Option<i64>) {
    let failed = |why: &str| {
        (
            json!({ "type": "kn_put_result", "id": id, "ok": false, "error": why }),
            None,
        )
    };
    if !lines.is_peer(client_id) {
        return failed(NEEDS_PEER);
    }
    let Some(state) = lines.app_state() else {
        return failed(NO_STORE);
    };
    let Some(layer) = parse_layer(f.layer) else {
        return failed("That is not a knowledge layer.");
    };
    let device = {
        let Ok(r) = lines.registry().lock() else {
            return failed(NO_STORE);
        };
        if let Layer::Person(person) = layer {
            if r.person_by_id(person).ok().flatten().is_none() {
                return failed("There is no such person.");
            }
        }
        r.client_by_id(client_id)
            .ok()
            .flatten()
            .and_then(|c| c.device_uid)
    };
    let stored = match state.store.lock() {
        Ok(mut store) => put(
            store.conn_mut(),
            &Put {
                layer: f.layer,
                path: f.path,
                base_seq: f.base_seq,
                sha256: f.sha256,
                deleted: f.deleted,
                body: f.body,
                device_uid: device.as_deref(),
            },
        ),
        Err(_) => return failed(NO_STORE),
    };
    match stored {
        Ok(PutOutcome::Saved { seq, changed }) => (
            json!({ "type": "kn_put_result", "id": id, "ok": true, "seq": seq }),
            changed.then_some(seq),
        ),
        Ok(PutOutcome::Conflict {
            seq,
            sha256,
            deleted,
        }) => (
            json!({
                "type": "kn_put_result",
                "id": id,
                "ok": false,
                "conflict": { "seq": seq, "sha256": sha256, "deleted": deleted },
            }),
            None,
        ),
        Ok(PutOutcome::Refused(why)) => failed(&why),
        Err(e) => failed(&format!("The knowledge store failed: {e}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn b64(s: &str) -> String {
        base64::engine::general_purpose::STANDARD.encode(s)
    }

    fn save(conn: &mut Connection, path: &str, base_seq: i64, body: Option<&str>) -> PutOutcome {
        put(
            conn,
            &Put {
                layer: "everyone",
                path,
                base_seq,
                sha256: "",
                deleted: body.is_none(),
                body: body.map(b64).as_deref(),
                device_uid: Some("device-0001"),
            },
        )
        .unwrap()
    }

    #[test]
    fn layers_and_paths_are_checked_and_puts_keep_the_stored_copy_on_a_conflict() {
        for (layer, ok) in [
            ("everyone", true),
            ("person:7", true),
            ("person:07", false),
            ("person:0", false),
            ("person:-1", false),
            ("person:", false),
            ("people:7", false),
            ("Everyone", false),
        ] {
            assert_eq!(parse_layer(layer).is_some(), ok, "{layer}");
        }
        for (path, ok) in [
            ("rules/style.md", true),
            ("agents/helper.md", true),
            ("skills/review/SKILL.md", true),
            ("skills/review/notes/deep.md", true),
            ("rules", false),
            ("skills/review", false),
            ("skills/SKILL.md", false),
            ("notes/a.md", false),
            ("rules/../a.md", false),
            ("rules/.hidden", false),
            ("rules//a.md", false),
            ("/rules/a.md", false),
            ("rules/a.md/", false),
            ("rules/a\\b.md", false),
            ("rules/a:b.md", false),
            ("rules/nul.md", false),
            ("rules/COM3", false),
            ("rules/com10.md", true),
            ("rules/trailing.", false),
            ("rules/a\u{7}b.md", false),
        ] {
            assert_eq!(check_path(path).is_ok(), ok, "{path}");
        }
        assert!(check_path(&format!("rules/{}", "a".repeat(200))).is_err());

        let mut conn = Connection::open_in_memory().unwrap();
        init(&conn).unwrap();
        // New file, then an update from the seq it was read at.
        let one = save(&mut conn, "rules/a.md", 0, Some("one"));
        assert_eq!(
            one,
            PutOutcome::Saved {
                seq: 1,
                changed: true
            }
        );
        // The same bytes again are accepted without a new seq, whatever baseSeq says.
        assert_eq!(
            save(&mut conn, "rules/a.md", 0, Some("one")),
            PutOutcome::Saved {
                seq: 1,
                changed: false
            }
        );
        assert_eq!(
            save(&mut conn, "rules/a.md", 1, Some("two")),
            PutOutcome::Saved {
                seq: 2,
                changed: true
            }
        );
        // A stale base conflicts and reports the stored copy; nothing is written.
        let PutOutcome::Conflict {
            seq,
            sha256,
            deleted,
        } = save(&mut conn, "rules/a.md", 1, Some("mine"))
        else {
            panic!("expected a conflict");
        };
        assert_eq!((seq, sha256, deleted), (2, hex_sha256(b"two"), false));
        // A new file is a conflict when the path exists, and a delete needs the current seq.
        assert!(matches!(
            save(&mut conn, "rules/a.md", 0, Some("fresh")),
            PutOutcome::Conflict { .. }
        ));
        assert!(matches!(
            save(&mut conn, "rules/a.md", 1, None),
            PutOutcome::Conflict { .. }
        ));
        assert_eq!(
            save(&mut conn, "rules/a.md", 2, None),
            PutOutcome::Saved {
                seq: 3,
                changed: true
            }
        );
        // A deleted path is written again as new; letter case may not split a name.
        assert_eq!(
            save(&mut conn, "rules/a.md", 0, Some("back")),
            PutOutcome::Saved {
                seq: 4,
                changed: true
            }
        );
        assert!(matches!(
            save(&mut conn, "rules/A.md", 0, Some("x")),
            PutOutcome::Refused(_)
        ));
        // A wrong checksum, a bad body and a missing body are refused.
        for (sha, body) in [
            ("0".repeat(64), Some(b64("x"))),
            (String::new(), Some("!!".into())),
            (String::new(), None),
        ] {
            let r = put(
                &mut conn,
                &Put {
                    layer: "everyone",
                    path: "rules/b.md",
                    base_seq: 0,
                    sha256: &sha,
                    deleted: false,
                    body: body.as_deref(),
                    device_uid: None,
                },
            )
            .unwrap();
            assert!(matches!(r, PutOutcome::Refused(_)), "{sha} {body:?}");
        }

        // Pulling returns each path's latest state in seq order, tombstones without a body, and
        // pages by the cursor.
        save(&mut conn, "rules/c.md", 0, Some("cee"));
        save(&mut conn, "agents/d.md", 0, Some("dee"));
        let (files, cursor, more) = pull(&conn, 0, 200).unwrap();
        let paths: Vec<&str> = files.iter().map(|f| f["path"].as_str().unwrap()).collect();
        assert_eq!(paths, ["rules/a.md", "rules/c.md", "agents/d.md"]);
        assert_eq!((cursor, more), (6, false));
        assert_eq!(files[0]["body"], b64("back"));
        let (page, cursor, more) = pull(&conn, 0, 2).unwrap();
        assert_eq!((page.len(), cursor, more), (2, 5, true));
        let (rest, cursor, more) = pull(&conn, cursor, 2).unwrap();
        assert_eq!((rest.len(), cursor, more), (1, 6, false));
        save(&mut conn, "rules/c.md", 5, None);
        let (gone, cursor, _) = pull(&conn, 6, 10).unwrap();
        assert_eq!(
            (gone[0]["deleted"].as_bool(), gone[0].get("body")),
            (Some(true), None)
        );
        assert_eq!(cursor, 7);
        // Nothing new: the cursor stays where it was asked from.
        assert_eq!(pull(&conn, 7, 10).unwrap(), (vec![], 7, false));
    }
}
