//! The attention summary.
//!
//! One host-side summary feeds every consumer: the taskbar badge, keep-awake and the tray menu,
//! the sidebar's row marks, and the in-app toasts' withdraw-on-read (a toast is withdrawn once
//! its chat leaves `unread`). Nobody computes it twice.
//!
//! Rules:
//! - Only unarchived chats that are not unpromoted side chats count.
//! - The badge counts **chats**, each once, that have any of: a manual unread mark; an unread
//!   completion while idle with no error; a pending blocking request (approval, question or plan,
//!   from the waiting tracker: the live interactions). Stopped or failed chats count only when
//!   manually marked. Nothing is muted yet.
//! - `busy` counts starting, running and stopping chats.
//! - Each group lists at most [`MAX_ROWS`], newest activity first.
//!
//! [`publish`] recomputes the summary and emits an `attention` event when it changed. The actor
//! calls it on every status change and every change to its interactions; ops that change marks,
//! completions or the archived flag call it too.

use crate::runtime::Shared;
use crate::types::*;
use rusqlite::{params, Transaction};
use std::collections::{HashMap, HashSet};

pub const MAX_ROWS: usize = 8;

/// The waiting tracker: chat id → the kind of its first pending blocking request.
pub type Needs = HashMap<String, AttentionKind>;

/// The blocking requests the live actors hold right now.
pub(crate) fn live_needs(sh: &Shared) -> Needs {
    let map = sh.actors.lock().unwrap();
    let mut out = Needs::new();
    for (id, h) in map.iter() {
        let live = h.live.lock().unwrap();
        if let Some(kind) = live
            .thread
            .interactions
            .iter()
            .find_map(|i| match i.attention {
                Some(InteractionAttention::Approval) | Some(InteractionAttention::Trust) => {
                    Some(AttentionKind::Approval)
                }
                Some(InteractionAttention::Question) => Some(AttentionKind::Question),
                Some(InteractionAttention::Plan) => Some(AttentionKind::Plan),
                None => None,
            })
        {
            out.insert(id.clone(), kind);
        }
    }
    out
}

struct Cand {
    id: String,
    title: String,
    project: String,
    status: ChatStatus,
    error: bool,
    marked: bool,
    completion: bool,
    muted: bool,
    stalled: bool,
    /// Marked addressed or snoozed, with no activity since.
    quiet: bool,
}

/// The plain-language reason a row is listed.
pub fn reason(kind: AttentionKind, marked: bool) -> &'static str {
    match kind {
        AttentionKind::Approval => "Waiting for your approval",
        AttentionKind::Question => "Has a question for you",
        AttentionKind::Plan => "Has a plan for you to review",
        AttentionKind::Running => "Working",
        AttentionKind::Unread if marked => "Marked unread",
        AttentionKind::Unread => "Finished; not read yet",
        AttentionKind::Stalled => "No output for a while; it may be stuck",
    }
}

fn title_of(t: &str) -> String {
    if t.trim().is_empty() {
        "Untitled".into()
    } else {
        t.to_owned()
    }
}

/// Compute the summary from the database plus the waiting tracker.
pub fn compute(
    tx: &Transaction<'_>,
    needs: &Needs,
    revision: i64,
) -> rusqlite::Result<AttentionSummary> {
    let now = helm_core::now_ms();
    // `quiet`: addressed after the last activity, or snoozed into the future with no activity
    // since the snooze (?1 is now).
    const COLS: &str = "c.id, c.title, COALESCE(p.name, ''), c.status, c.error IS NOT NULL, \
         EXISTS(SELECT 1 FROM unread_marks u WHERE u.chat_id = c.id), \
         EXISTS(SELECT 1 FROM chat_completions k WHERE k.chat_id = c.id AND k.read = 0), \
         EXISTS(SELECT 1 FROM chat_mutes m WHERE m.chat_id = c.id), \
         COALESCE((SELECT a.stalled FROM chat_activity a WHERE a.chat_id = c.id), 0), \
         EXISTS(SELECT 1 FROM chat_attention t WHERE t.chat_id = c.id AND ( \
             t.addressed_at >= MAX(c.updated_at, COALESCE((SELECT a.last_activity_at FROM chat_activity a WHERE a.chat_id = c.id), 0)) \
             OR (t.snoozed_until > ?1 AND t.snoozed_at >= MAX(c.updated_at, COALESCE((SELECT a.last_activity_at FROM chat_activity a WHERE a.chat_id = c.id), 0)))))";
    let read = |r: &rusqlite::Row<'_>| -> rusqlite::Result<Cand> {
        Ok(Cand {
            id: r.get(0)?,
            title: title_of(&r.get::<_, String>(1)?),
            project: r.get(2)?,
            status: ChatStatus::parse(&r.get::<_, String>(3)?),
            error: r.get(4)?,
            marked: r.get(5)?,
            completion: r.get(6)?,
            muted: r.get(7)?,
            stalled: r.get(8)?,
            quiet: r.get(9)?,
        })
    };
    let mut cands: Vec<Cand> = {
        let mut st = tx.prepare(&format!(
            "SELECT {COLS} FROM chats c JOIN projects p ON p.id = c.project_id
             WHERE c.archived = 0 AND c.side_source_id IS NULL
               AND (c.status IN ('starting', 'running', 'stopping')
                    OR EXISTS(SELECT 1 FROM unread_marks u WHERE u.chat_id = c.id)
                    OR EXISTS(SELECT 1 FROM chat_completions k WHERE k.chat_id = c.id AND k.read = 0))
             ORDER BY c.updated_at DESC, c.id"
        ))?;
        let rows = st.query_map([now], read)?;
        rows.collect::<rusqlite::Result<_>>()?
    };
    let seen: HashSet<String> = cands.iter().map(|c| c.id.clone()).collect();
    for id in needs.keys().filter(|id| !seen.contains(*id)) {
        let mut st = tx.prepare_cached(&format!(
            "SELECT {COLS} FROM chats c JOIN projects p ON p.id = c.project_id
             WHERE c.id = ?2 AND c.archived = 0 AND c.side_source_id IS NULL"
        ))?;
        let mut rows = st.query_map(params![now, id], read)?;
        if let Some(c) = rows.next() {
            cands.push(c?);
        }
    }
    let row = |c: &Cand, kind: AttentionKind| AttentionRow {
        chat_id: c.id.clone(),
        title: c.title.clone(),
        project: c.project.clone(),
        kind,
        reason: reason(kind, c.marked).to_owned(),
    };
    let mut s = AttentionSummary {
        badge: 0,
        busy: 0,
        needs_you: vec![],
        running: vec![],
        unread: vec![],
        stalled: vec![],
        revision,
    };
    for c in &cands {
        if c.status.is_busy() {
            // Keep-awake counts every busy chat, muted or not.
            s.busy += 1;
            if !c.muted && s.running.len() < MAX_ROWS {
                s.running.push(row(c, AttentionKind::Running));
            }
        }
        // A muted chat is out of the badge and every group but Running's busy count; a
        // chat marked addressed or snoozed is out until something happens in it.
        if c.muted || c.quiet {
            continue;
        }
        let need = needs.get(&c.id).copied();
        let unread = c.marked || (c.completion && c.status == ChatStatus::Idle && !c.error);
        let stalled = c.stalled && c.status.is_busy();
        if need.is_some() || unread || stalled {
            s.badge += 1;
        }
        if stalled && s.stalled.len() < MAX_ROWS {
            s.stalled.push(row(c, AttentionKind::Stalled));
        }
        if let Some(k) = need {
            if s.needs_you.len() < MAX_ROWS {
                s.needs_you.push(row(c, k));
            }
        }
        if unread && s.unread.len() < MAX_ROWS {
            s.unread.push(row(c, AttentionKind::Unread));
        }
    }
    Ok(s)
}

fn same(a: &AttentionSummary, b: &AttentionSummary) -> bool {
    AttentionSummary {
        revision: 0,
        ..a.clone()
    } == AttentionSummary {
        revision: 0,
        ..b.clone()
    }
}

/// Recompute and emit when it changed. Serialised by the `attention` lock, so events leave in
/// revision order.
pub(crate) fn publish(sh: &Shared) {
    // The lock is taken before the waiting tracker is read, so a publish holding older live state
    // can never be the one that gets the newer revision. Lock order: attention, actors, live.
    let mut last = sh.attention.lock().unwrap();
    let needs = live_needs(sh);
    let Ok(mut s) = sh.db.snapshot(|tx| compute(tx, &needs, 0)) else {
        return;
    };
    if last.as_ref().is_some_and(|l| same(l, &s)) {
        return;
    }
    s.revision = sh.clock.next();
    *last = Some(s.clone());
    drop(last);
    sh.events
        .emit(ChatEvent::Attention { summary: s }.to_app_event());
}

/// The current summary, for `attention_get`.
pub(crate) fn current(sh: &Shared) -> rusqlite::Result<AttentionSummary> {
    let mut last = sh.attention.lock().unwrap();
    let needs = live_needs(sh);
    let mut s = sh.db.snapshot(|tx| compute(tx, &needs, 0))?;
    match last.as_ref() {
        Some(l) if same(l, &s) => s.revision = l.revision,
        _ => {
            s.revision = sh.clock.next();
            *last = Some(s.clone());
        }
    }
    Ok(s)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::tests::temp_db;
    use crate::store;
    use helm_core::db::Database;

    fn chat(db: &Database, title: &str) -> String {
        let id = db
            .write(|tx| store::create_chat(tx, "C:/w/proj", None, None, 1))
            .unwrap();
        db.write(|tx| tx.execute("UPDATE chats SET title = ?1 WHERE id = ?2", [title, &id]))
            .unwrap();
        id
    }

    fn exec(db: &Database, sql: &str, p: &[&str]) {
        db.write(|tx| tx.execute(sql, rusqlite::params_from_iter(p.iter())))
            .unwrap();
    }

    fn summary(db: &Database, needs: &Needs) -> AttentionSummary {
        db.snapshot(|tx| compute(tx, needs, 1)).unwrap()
    }

    #[test]
    fn the_badge_counts_each_chat_once() {
        let (_d, db) = temp_db();
        let a = chat(&db, "a");
        // A mark, an unread completion and a pending approval on one chat: one badge.
        exec(
            &db,
            "INSERT INTO unread_marks(chat_id, token, marked_at) VALUES (?1, 't', 1)",
            &[&a],
        );
        exec(
            &db,
            "INSERT INTO chat_completions(chat_id, id, completed_at) VALUES (?1, 'k1', 1)",
            &[&a],
        );
        let mut needs = Needs::new();
        needs.insert(a.clone(), AttentionKind::Approval);
        let s = summary(&db, &needs);
        assert_eq!(s.badge, 1);
        assert_eq!(s.needs_you.len(), 1);
        assert_eq!(s.needs_you[0].kind, AttentionKind::Approval);
        assert_eq!(s.unread.len(), 1);
        assert_eq!(s.unread[0].project, "proj");
    }

    #[test]
    fn archived_chats_and_side_chats_never_count() {
        let (_d, db) = temp_db();
        let a = chat(&db, "a");
        let b = chat(&db, "b");
        exec(
            &db,
            "INSERT INTO unread_marks(chat_id, token, marked_at) VALUES (?1, 't', 1)",
            &[&a],
        );
        exec(
            &db,
            "INSERT INTO unread_marks(chat_id, token, marked_at) VALUES (?1, 't', 1)",
            &[&b],
        );
        assert_eq!(summary(&db, &Needs::new()).badge, 2);
        exec(&db, "UPDATE chats SET archived = 1 WHERE id = ?1", &[&a]);
        exec(
            &db,
            "UPDATE chats SET side_source_id = 'x' WHERE id = ?1",
            &[&b],
        );
        let mut needs = Needs::new();
        needs.insert(a.clone(), AttentionKind::Question);
        let s = summary(&db, &needs);
        assert_eq!(s.badge, 0, "archived keeps its mark but is not counted");
        assert!(s.needs_you.is_empty() && s.unread.is_empty());
    }

    #[test]
    fn completions_count_only_on_idle_error_free_chats() {
        let (_d, db) = temp_db();
        let a = chat(&db, "");
        exec(
            &db,
            "INSERT INTO chat_completions(chat_id, id, completed_at) VALUES (?1, 'k1', 1)",
            &[&a],
        );
        let s = summary(&db, &Needs::new());
        assert_eq!((s.badge, s.unread[0].title.as_str()), (1, "Untitled"));
        exec(
            &db,
            "UPDATE chats SET status = 'stopped' WHERE id = ?1",
            &[&a],
        );
        assert_eq!(
            summary(&db, &Needs::new()).badge,
            0,
            "stopped counts only when marked"
        );
        exec(
            &db,
            "UPDATE chats SET status = 'idle', error = 'boom' WHERE id = ?1",
            &[&a],
        );
        assert_eq!(
            summary(&db, &Needs::new()).badge,
            0,
            "an error keeps it out"
        );
        exec(&db, "UPDATE chats SET error = NULL WHERE id = ?1", &[&a]);
        exec(
            &db,
            "UPDATE chat_completions SET read = 1 WHERE chat_id = ?1",
            &[&a],
        );
        assert_eq!(
            summary(&db, &Needs::new()).badge,
            0,
            "a read completion is not unread"
        );
        exec(
            &db,
            "UPDATE chats SET status = 'stopped' WHERE id = ?1",
            &[&a],
        );
        exec(
            &db,
            "INSERT INTO unread_marks(chat_id, token, marked_at) VALUES (?1, 't', 1)",
            &[&a],
        );
        assert_eq!(
            summary(&db, &Needs::new()).badge,
            1,
            "a manual mark counts whatever the status"
        );
    }

    #[test]
    fn running_chats_are_busy_and_groups_hold_eight() {
        let (_d, db) = temp_db();
        for i in 0..10 {
            let c = chat(&db, &format!("c{i}"));
            exec(
                &db,
                "UPDATE chats SET status = 'running' WHERE id = ?1",
                &[&c],
            );
        }
        let s = summary(&db, &Needs::new());
        assert_eq!(s.busy, 10);
        assert_eq!(s.running.len(), MAX_ROWS);
        assert_eq!(s.badge, 0, "running alone is not a badge");
    }
}
