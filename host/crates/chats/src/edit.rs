//! Message edit and resend.
//!
//! - `message_edit_open {chatId, messageId}` answers the message's text and a revision. It is
//!   refused while the chat works, for archived chats, and for anything but an active, complete
//!   user message whose entry is in the session log.
//! - `message_resend {chatId, messageId, revision, submissionId, text}` is two-phase and safe
//!   against a crash between the phases:
//!   - **Stage**, in one transaction with the new submission (saved, not sent): a `pending_edits`
//!     row naming the chat, the submission, the edited entry and the session log's length.
//!   - **Apply**, once the chat's worker has closed ([`apply`]): append an `edit` marker entry
//!     whose parent is the edited message's parent (a new branch just before it); mark the edited
//!     message and everything after it inactive (the old branch stays in the log and the
//!     database, out of the transcript and search); mark submissions whose rows went inactive as
//!     superseded and take them out of the queue; drop the queue slot and the completion; and
//!     drop the Claude session binding, since Claude's own session cannot be cut there. The
//!     resent prompt then carries the history before the edit point once, like a message fork.
//!   - **Recovery:** a `pending_edits` row left by a crash is applied at startup
//!     ([`recover_all`]); its submission is left `rejected`, so the queue offers Recover rather
//!     than sending it unseen.
//! - **Images.** The resent message keeps the edited message's images: the new submission gets its
//!   own copy of each (see [`crate::message_images`]), so the new bubble shows them and they ride
//!   the prompt again. The old message keeps its own.
//! - The draft is never touched, and project files are not rolled back.

use crate::archive::Refusal;
use crate::session_log::{self, ContextEntry, Entry, Header, SessionLog};
use crate::types::*;
use helm_core::db::Database;
use rusqlite::{params, OptionalExtension, Transaction};
use serde_json::Value;
use std::path::Path;

/// An edit's revision is the message row's `revision`; a changed row refuses a stale resend.
pub struct EditOpen {
    pub message_id: i64,
    pub revision: i64,
    pub text: String,
}

fn refuse(m: &str) -> Refusal {
    Refusal::new("refused", m)
}

/// The editable user message `message_id`: (source key, revision, text).
fn editable(
    tx: &Transaction<'_>,
    chat_id: &str,
    message_id: i64,
) -> rusqlite::Result<Result<(String, i64, String), Refusal>> {
    let chat: Option<(String, bool)> = tx
        .query_row(
            "SELECT status, archived FROM chats WHERE id = ?1",
            [chat_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    let Some((status, archived)) = chat else {
        return Ok(Err(Refusal::new("not_found", "That chat does not exist.")));
    };
    if archived {
        return Ok(Err(refuse("Restore this chat before editing a message.")));
    }
    if ChatStatus::parse(&status).is_busy() {
        return Ok(Err(refuse(
            "Wait for the turn to finish, or stop it, before editing.",
        )));
    }
    let row: Option<(String, String, String, bool, i64, String)> = tx
        .query_row(
            "SELECT source_key, role, status, active, revision, text FROM messages WHERE chat_id = ?1 AND id = ?2",
            params![chat_id, message_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?)),
        )
        .optional()?;
    let Some((key, role, mstatus, active, revision, text)) = row else {
        return Ok(Err(refuse("Only your own sent messages can be edited.")));
    };
    if role != "user" || mstatus != "complete" || !active {
        return Ok(Err(refuse("Only your own sent messages can be edited.")));
    }
    let in_log: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM entries WHERE chat_id = ?1 AND entry_id = ?2)",
        params![chat_id, key],
        |r| r.get(0),
    )?;
    if !in_log {
        return Ok(Err(refuse(
            "This message is still being saved; try again in a moment.",
        )));
    }
    if text.len() > crate::store::MAX_SUBMISSION_BYTES {
        return Ok(Err(refuse(
            "This message is larger than 2 MB and cannot be edited.",
        )));
    }
    Ok(Ok((key, revision, text)))
}

pub fn open(db: &Database, chat_id: &str, message_id: i64) -> Result<EditOpen, Refusal> {
    let r = db
        .snapshot(|tx| editable(tx, chat_id, message_id))
        .map_err(|e| Refusal::new("internal", e.to_string()))??;
    Ok(EditOpen {
        message_id,
        revision: r.1,
        text: r.2,
    })
}

/// Give the new submission its own copy of the edited message's images.
fn carry_images(
    tx: &Transaction<'_>,
    attachments: &Path,
    chat_id: &str,
    message_id: i64,
    submission_id: &str,
) -> rusqlite::Result<()> {
    let details: Option<String> = tx
        .query_row(
            "SELECT details FROM messages WHERE chat_id = ?1 AND id = ?2",
            params![chat_id, message_id],
            |r| r.get(0),
        )
        .optional()?;
    let details: Value = details
        .and_then(|d| serde_json::from_str(&d).ok())
        .unwrap_or(Value::Null);
    for old in crate::message_images::ids_in(&details) {
        let new = uuid::Uuid::new_v4().to_string();
        if let Err(e) = crate::message_images::copy_file(attachments, &old, &new) {
            log::warn!("edit: image {old} could not be carried over: {e}");
            continue;
        }
        if !crate::message_images::insert_copy(tx, &old, chat_id, &new, chat_id, submission_id)? {
            let _ = std::fs::remove_file(attachments.join(&new));
        }
    }
    Ok(())
}

/// Stage the edit with its submission. Returns the submission.
pub fn stage(
    db: &Database,
    sessions: &Path,
    attachments: &Path,
    a: &MessageResendArgs,
) -> Result<Submission, Refusal> {
    if a.text.trim().is_empty() {
        return Err(Refusal::new("bad_args", "There is nothing to send."));
    }
    if a.text.len() > crate::store::MAX_SUBMISSION_BYTES {
        return Err(Refusal::new(
            "too_large",
            "The message is larger than 2 MB.",
        ));
    }
    let sid = uuid::Uuid::parse_str(&a.submission_id)
        .map_err(|_| Refusal::new("bad_args", "submissionId must be a UUID."))?
        .to_string();
    let log_len = std::fs::metadata(session_log::log_path(sessions, &a.chat_id))
        .map(|m| m.len() as i64)
        .unwrap_or(0);
    let now = helm_core::now_ms();
    db.write(|tx| {
        if let Some(s) = crate::store::get_submission(tx, &sid)? {
            return Ok(Ok(s));
        }
        let (key, revision, _) = match editable(tx, &a.chat_id, a.message_id)? {
            Ok(v) => v,
            Err(r) => return Ok(Err(r)),
        };
        if revision != a.revision {
            return Ok(Err(refuse(
                "This message changed while you were editing; copy your edit and open it again.",
            )));
        }
        let pending: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM pending_edits WHERE chat_id = ?1)",
            [&a.chat_id],
            |r| r.get(0),
        )?;
        if pending {
            return Ok(Err(refuse("An edit is already being applied to this chat.")));
        }
        crate::submit::insert_submission(
            tx,
            &crate::submit::NewSubmission {
                id: &sid,
                chat_id: &a.chat_id,
                text: &a.text,
                mode: SubmissionMode::Prompt,
                state: SubmissionState::Saved,
                origin: SubmissionOrigin::Typed,
                priority: None,
                admission: crate::submit::AdmissionClass::Exempt,
                attachments: &[],
                now,
            },
        )?;
        carry_images(tx, attachments, &a.chat_id, a.message_id, &sid)?;
        tx.execute(
            "INSERT INTO pending_edits(chat_id, submission_id, entry_id, log_len, marker_id, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![a.chat_id, sid, key, log_len, format!("c:edit:{sid}"), now],
        )?;
        Ok(Ok(crate::store::get_submission(tx, &sid)?.expect("just inserted")))
    })
    .map_err(|e: rusqlite::Error| Refusal::new("internal", e.to_string()))?
}

/// Apply the chat's staged edit, if any (the worker must be closed). Returns its submission id.
pub fn apply(
    db: &Database,
    profile_dir: &Path,
    chat_id: &str,
    revision: i64,
) -> Result<Option<String>, Refusal> {
    let internal = |e: String| Refusal::new("internal", e);
    let row: Option<(String, String, String, String)> = db
        .snapshot(|tx| {
            tx.query_row(
                "SELECT p.submission_id, p.entry_id, p.marker_id, COALESCE(pr.path, '')
                 FROM pending_edits p JOIN chats c ON c.id = p.chat_id
                 JOIN projects pr ON pr.id = c.project_id WHERE p.chat_id = ?1",
                [chat_id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .optional()
        })
        .map_err(|e| internal(e.to_string()))?;
    let Some((sid, entry_id, marker_id, cwd)) = row else {
        return Ok(None);
    };
    let sessions = profile_dir.join("sessions");
    let artifacts = profile_dir.join("artifacts");
    let header = Header {
        version: session_log::FORMAT_VERSION.into(),
        chat_id: chat_id.to_owned(),
        harness: "claude".into(),
        cwd: cwd.clone(),
        created_at: helm_core::now_ms(),
    };
    let (mut log, read) =
        SessionLog::open(&sessions, &header).map_err(|e| internal(e.to_string()))?;
    let Some(at) = read
        .entries
        .iter()
        .position(|e| e.get("id").and_then(Value::as_str) == Some(&entry_id))
    else {
        return Err(internal(
            "the edited message is not in the session log".into(),
        ));
    };
    let parent = read.entries[at]
        .get("parentId")
        .and_then(Value::as_str)
        .map(str::to_owned);
    let before: Vec<_> = read.entries[..at].to_vec();
    // The marker: a new branch just before the edited message (idempotent on its id).
    let mut marker = Entry::Context(ContextEntry {
        id: marker_id.clone(),
        parent_id: None,
        timestamp: helm_core::now_ms(),
        kind: ContextKind::Edit,
        text: "Message edited and sent again; the earlier version is kept in the history.".into(),
    });
    log.append_with_parent(&mut marker, parent)
        .map_err(|e| internal(e.to_string()))?;
    let carry = crate::fork::carry_text(&before);
    let raw = serde_json::to_value(&marker).unwrap_or(Value::Null);
    db.write(|tx| {
        let (ts, mid): (i64, i64) = tx.query_row(
            "SELECT timestamp, id FROM messages WHERE chat_id = ?1 AND source_key = ?2",
            params![chat_id, entry_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        // The old branch: the edited message and everything after it.
        tx.execute(
            "UPDATE messages SET active = 0, revision = ?4 WHERE chat_id = ?1 AND (timestamp, id) >= (?2, ?3)",
            params![chat_id, ts, mid, revision],
        )?;
        if let Some(obj) = raw.as_object() {
            session_log::project(tx, &artifacts, chat_id, obj, revision)?;
        }
        // Submissions whose rows went inactive are superseded.
        tx.execute(
            "UPDATE submissions SET state = 'superseded' WHERE chat_id = ?1 AND id != ?2
               AND ('u:' || id) IN (SELECT source_key FROM messages WHERE chat_id = ?1 AND active = 0)",
            params![chat_id, sid],
        )?;
        tx.execute("DELETE FROM queued_submissions WHERE chat_id = ?1", [chat_id])?;
        tx.execute(
            "UPDATE submissions SET state = 'superseded' WHERE chat_id = ?1 AND state = 'queued'",
            [chat_id],
        )?;
        tx.execute("DELETE FROM draft_queue_slots WHERE chat_id = ?1", [chat_id])?;
        crate::manage::drop_completion(tx, chat_id)?;
        tx.execute("DELETE FROM fork_carry WHERE chat_id = ?1", [chat_id])?;
        if let Some(c) = &carry {
            tx.execute(
                "INSERT INTO fork_carry(chat_id, text) VALUES (?1, ?2)",
                params![chat_id, c],
            )?;
        }
        tx.execute("DELETE FROM pending_edits WHERE chat_id = ?1", [chat_id])?;
        Ok::<_, rusqlite::Error>(())
    })
    .map_err(|e| internal(e.to_string()))?;
    // Claude's own session cannot be cut at the edit point: the next launch starts fresh.
    let _ = std::fs::remove_file(session_log::binding_path(&sessions, chat_id));
    Ok(Some(sid))
}

/// Startup: apply every edit a crash left staged; their submissions wait for Recover.
pub fn recover_all(db: &Database, profile_dir: &Path, revision: i64) {
    let ids: Vec<String> = db
        .snapshot(|tx| {
            let mut st = tx.prepare("SELECT chat_id FROM pending_edits")?;
            let rows = st.query_map([], |r| r.get(0))?;
            rows.collect::<rusqlite::Result<Vec<String>>>()
        })
        .unwrap_or_default();
    for id in ids {
        match apply(db, profile_dir, &id, revision) {
            Ok(Some(sid)) => {
                let _ = db.write(|tx| {
                    tx.execute(
                        "UPDATE submissions SET state = 'rejected' WHERE id = ?1 AND state = 'saved'",
                        [&sid],
                    )
                });
                log::info!("chat {id}: a staged edit was applied after a restart");
            }
            Ok(None) => {}
            Err(e) => log::error!("chat {id}: staged edit not applied: {}", e.message),
        }
    }
}
