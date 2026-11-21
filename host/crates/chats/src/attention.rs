//! The attention summary.
//!
//! One host-side summary feeds every consumer: the taskbar badge, keep-awake and the tray menu,
//! the sidebar's row marks, and the in-app toasts' withdraw-on-read (a toast is withdrawn once
//! its chat leaves both `unread` and `finished`). Nobody computes it twice.
//!
//! Rules:
//! - Only unarchived chats that are not unpromoted side chats count.
//! - The badge counts **chats**, each once, that have any of: a manual unread mark; a pending
//!   blocking request (approval, question or plan, from the waiting tracker: the live
//!   interactions). A chat that merely finished a reply the user has not read does not count:
//!   it sits in `finished` instead, so the finished notices (a toast's withdraw-on-read, the
//!   away alerts) keep the chat while the reply stays unread and drop it once it is read.
//!   Stopped or failed chats count only when manually marked. A muted chat counts for nothing
//!   and is in no group but `busy`; what it holds (a live request or its agent's flag) is
//!   listed in `muted` so the user can still find it.
//!   `triage` carries the idle check's priority and kind for the chats it listed, and the chats it
//!   gave up on; it is never counted. A check's decision is a flagged row; its stall is a stalled
//!   row; neither shows while the chat has an open wait or a working helper chat.
//! - `busy` counts starting, running and stopping chats.
//! - The running, stalled, unread and finished groups list at most [`MAX_ROWS`] **per profile**,
//!   newest activity first: every consumer shows one profile at a time, so one profile's busy
//!   chats must not push another profile's rows off the list. The needs-you and flagged groups
//!   are never cut short: each row there is a
//!   flag drawn on a sidebar row, and a flag that is counted in the badge must have a row to
//!   sit on. A chat can be in both of them (the consumers show it once).
//!
//! [`publish`] recomputes the summary and emits an `attention` event when it changed. The actor
//! calls it on every status change and every change to its interactions; ops that change marks,
//! completions or the archived flag call it too.

use crate::runtime::Shared;
use crate::types::*;
use rusqlite::{params, Transaction};
use std::collections::{HashMap, HashSet};

/// The most rows each of the capped groups (running, stalled, unread, finished) lists per profile.
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
            // A card the question gate is still holding stays quiet: the badge, tray, toasts
            // and alerts wake only if the hold ends with the card still waiting.
            .filter(|i| {
                !i.gate
                    .as_ref()
                    .is_some_and(|g| g.state == crate::types::GateStatus::Checking)
            })
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
    profile: String,
    status: ChatStatus,
    error: bool,
    marked: bool,
    completion: bool,
    muted: bool,
    stalled: bool,
    /// Marked addressed or snoozed, with no activity since.
    quiet: bool,
    /// The idle check's reason, while its verdict is current and triage is on.
    flagged: Option<String>,
    /// What sort of verdict that is: a stall is listed with the stuck chats, not flagged.
    flagged_kind: Option<crate::triage::Kind>,
    /// The question its own agent flagged for the user ([`crate::flags`]).
    asked: Option<(String, String)>,
}

/// One profile's share of the four capped groups.
#[derive(Default)]
struct Caps {
    running: usize,
    stalled: usize,
    unread: usize,
    finished: usize,
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
        AttentionKind::Triage => "May still need you",
    }
}

/// What a muted chat's live request is, for the Muted group.
fn muted_kind(k: AttentionKind) -> MutedKind {
    match k {
        AttentionKind::Approval => MutedKind::Approval,
        AttentionKind::Plan => MutedKind::Plan,
        _ => MutedKind::Question,
    }
}

fn title_of(t: &str) -> String {
    if t.trim().is_empty() {
        "Untitled".into()
    } else {
        t.to_owned()
    }
}

/// Chats with an Allow request waiting for the user. The consent tables belong to another crate;
/// where they do not exist (a build or test without them) nobody is waiting.
fn allow_waiting(tx: &Transaction<'_>, now: i64) -> Vec<String> {
    let Ok(mut st) = tx.prepare_cached(
        "SELECT DISTINCT chat_id FROM consent_requests WHERE state = 'pending' AND expires_at > ?1",
    ) else {
        return vec![];
    };
    st.query_map([now], |r| r.get::<_, String>(0))
        .map(|rows| rows.filter_map(Result::ok).collect())
        .unwrap_or_default()
}

/// Compute the summary from the database plus the waiting tracker. A pending Allow request counts
/// as a request waiting for approval, so it raises the badge and the "needs you" row even when
/// the chat is not open.
pub fn compute(
    tx: &Transaction<'_>,
    needs: &Needs,
    revision: i64,
) -> rusqlite::Result<AttentionSummary> {
    let now = helm_core::now_ms();
    let mut with_allows = needs.clone();
    for chat in allow_waiting(tx, now) {
        with_allows.entry(chat).or_insert(AttentionKind::Approval);
    }
    let needs = &with_allows;
    let triage_on = crate::triage::settings(tx).enabled;
    // `quiet`: addressed after the last activity, or snoozed into the future with no activity
    // since the snooze (?1 is now).
    // The idle check's verdict counts only while it was made for the chat's current activity, and
    // only while the chat is not waiting on helpers of its own: it clears the moment one appears.
    const COLS_HEAD: &str = concat!(
        "c.id, c.title, COALESCE(p.name, ''), c.status, c.error IS NOT NULL, \
         EXISTS(SELECT 1 FROM unread_marks u WHERE u.chat_id = c.id), \
         EXISTS(SELECT 1 FROM chat_completions k WHERE k.chat_id = c.id AND k.read = 0), \
         EXISTS(SELECT 1 FROM chat_mutes m WHERE m.chat_id = c.id), \
         COALESCE((SELECT a.stalled FROM chat_activity a WHERE a.chat_id = c.id), 0), ",
        crate::quiet_sql!(),
    );
    let current = format!(
        "g.chat_id = c.id AND g.needs = 1 AND g.failed = 0 AND NOT {} \
         AND g.stamp = MAX(c.updated_at, COALESCE((SELECT a.last_activity_at FROM chat_activity a WHERE a.chat_id = c.id), 0))",
        crate::triage::helpers_working_sql(tx)?
    );
    let cols = format!(
        "{COLS_HEAD}, (SELECT g.reason FROM chat_triage g WHERE {current}), \
         (SELECT f.question FROM chat_flags f WHERE f.chat_id = c.id), \
         (SELECT f.reason FROM chat_flags f WHERE f.chat_id = c.id), \
         (SELECT COALESCE(g.kind, 'decision') FROM chat_triage g WHERE {current}), \
         COALESCE(c.profile_id, '')"
    );
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
            flagged: if triage_on { r.get(10)? } else { None },
            asked: match (
                r.get::<_, Option<String>>(11)?,
                r.get::<_, Option<String>>(12)?,
            ) {
                (Some(q), Some(why)) => Some((q, why)),
                _ => None,
            },
            flagged_kind: r
                .get::<_, Option<String>>(13)?
                .as_deref()
                .and_then(crate::triage::Kind::parse),
            profile: r.get(14)?,
        })
    };
    let mut cands: Vec<Cand> = {
        let mut st = tx.prepare(&format!(
            "SELECT {cols} FROM chats c JOIN projects p ON p.id = c.project_id
             WHERE c.archived = 0 AND c.side_source_id IS NULL
               AND (c.status IN ('starting', 'running', 'stopping')
                    OR EXISTS(SELECT 1 FROM unread_marks u WHERE u.chat_id = c.id)
                    OR EXISTS(SELECT 1 FROM chat_completions k WHERE k.chat_id = c.id AND k.read = 0)
                    OR EXISTS(SELECT 1 FROM chat_flags f WHERE f.chat_id = c.id)
                    OR EXISTS(SELECT 1 FROM chat_triage g WHERE g.chat_id = c.id AND g.needs = 1 AND g.failed = 0))
             ORDER BY c.updated_at DESC, c.id"
        ))?;
        let rows = st.query_map([now], read)?;
        rows.collect::<rusqlite::Result<_>>()?
    };
    let seen: HashSet<String> = cands.iter().map(|c| c.id.clone()).collect();
    for id in needs.keys().filter(|id| !seen.contains(*id)) {
        let mut st = tx.prepare_cached(&format!(
            "SELECT {cols} FROM chats c JOIN projects p ON p.id = c.project_id
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
        profile_id: c.profile.clone(),
    };
    let mut s = AttentionSummary {
        badge: 0,
        busy: 0,
        needs_you: vec![],
        running: vec![],
        unread: vec![],
        finished: vec![],
        stalled: vec![],
        flagged: vec![],
        muted: vec![],
        triage: if triage_on {
            crate::triage::rows(tx, now)?
        } else {
            vec![]
        },
        revision,
    };
    // The capped groups are capped per profile, not across all profiles at once: the badge and
    // tray count every chat on the PC, but the window, the inbox and Workspace status show one
    // profile's rows at a time, and one profile's overflow must not hide another's.
    let mut caps: HashMap<String, Caps> = HashMap::new();
    for c in &cands {
        if c.status.is_busy() {
            // Keep-awake counts every busy chat, muted or not.
            s.busy += 1;
            if !c.muted {
                let cap = caps.entry(c.profile.clone()).or_default();
                if cap.running < MAX_ROWS {
                    cap.running += 1;
                    s.running.push(row(c, AttentionKind::Running));
                }
            }
        }
        // A chat marked addressed or snoozed is out until something happens in it.
        if c.quiet {
            continue;
        }
        // A muted chat is out of the badge and every group but Running's busy count. What it holds
        // (a live request, or the question its agent flagged) is kept in the Muted group, where the
        // user can find it; it raises no notice.
        if c.muted {
            let live = needs.get(&c.id).copied();
            let held = match (live, &c.asked) {
                (Some(k), _) => Some((muted_kind(k), reason(k, false).to_owned())),
                (None, Some((question, why))) => Some((
                    MutedKind::Flag,
                    format!("Asked by the agent: {question} (Why it blocks the work: {why})"),
                )),
                (None, None) => None,
            };
            if let Some((kind, reason)) = held {
                s.muted.push(MutedRow {
                    chat_id: c.id.clone(),
                    title: c.title.clone(),
                    project: c.project.clone(),
                    kind,
                    reason,
                });
            }
            continue;
        }
        // A live request (approval, question, plan) comes first; else the agent's own flag.
        let live_need = needs.get(&c.id).copied();
        let need = live_need.or(c.asked.as_ref().map(|_| AttentionKind::Question));
        let unread = c.marked;
        // A chat that only finished and has not been read is not listed as needing attention and
        // never counted; it sits in `finished` so the finished notices (a toast's withdraw-on-read,
        // the away alerts) still hold it while the reply is unread. A chat the user marked is in
        // `unread` and does not need the second row.
        let finished = c.completion && !c.marked && c.status == ChatStatus::Idle && !c.error;
        let stalled = c.stalled && c.status.is_busy();
        // The idle check only speaks for a chat the host is not already listing. A decision is
        // flagged; a stall is listed with the stuck chats (the inbox offers Resume).
        let checked = c.flagged.is_some() && !c.status.is_busy();
        let idle_stalled = checked && c.flagged_kind == Some(crate::triage::Kind::Stalled);
        let flagged = checked && !idle_stalled;
        if need.is_some() || unread || stalled || idle_stalled || flagged {
            s.badge += 1;
        }
        if stalled || idle_stalled {
            let cap = caps.entry(c.profile.clone()).or_default();
            if cap.stalled < MAX_ROWS {
                cap.stalled += 1;
                let mut r = row(c, AttentionKind::Stalled);
                if let Some(why) = c.flagged.as_ref().filter(|w| idle_stalled && !w.is_empty()) {
                    r.reason = why.clone();
                }
                s.stalled.push(r);
            }
        }
        if let Some(k) = need {
            let mut r = row(c, k);
            if let (None, Some((question, why))) = (live_need, &c.asked) {
                r.reason =
                    format!("Asked by the agent: {question} (Why it blocks the work: {why})");
            }
            s.needs_you.push(r);
        }
        if unread {
            let cap = caps.entry(c.profile.clone()).or_default();
            if cap.unread < MAX_ROWS {
                cap.unread += 1;
                s.unread.push(row(c, AttentionKind::Unread));
            }
        }
        if finished {
            let cap = caps.entry(c.profile.clone()).or_default();
            if cap.finished < MAX_ROWS {
                cap.finished += 1;
                s.finished.push(row(c, AttentionKind::Unread));
            }
        }
        if flagged {
            let mut r = row(c, AttentionKind::Triage);
            if let Some(why) = c.flagged.as_ref().filter(|w| !w.is_empty()) {
                r.reason = why.clone();
            }
            s.flagged.push(r);
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

/// The current summary, for `attention_get`. A read that finds a change nobody has published yet
/// publishes it too: it becomes the last summary, so the writer's own publish right after would
/// see nothing new, and event-only listeners (the taskbar badge, keep-awake) would never hear of
/// it.
pub(crate) fn current(sh: &Shared) -> rusqlite::Result<AttentionSummary> {
    let mut last = sh.attention.lock().unwrap();
    let needs = live_needs(sh);
    let mut s = sh.db.snapshot(|tx| compute(tx, &needs, 0))?;
    match last.as_ref() {
        Some(l) if same(l, &s) => s.revision = l.revision,
        _ => {
            s.revision = sh.clock.next();
            *last = Some(s.clone());
            drop(last);
            sh.events
                .emit(ChatEvent::Attention { summary: s.clone() }.to_app_event());
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
        assert!(
            s.finished.is_empty(),
            "a marked chat is not also held as finished"
        );
        // Each row names the profile its chat belongs to, empty for a chat nobody owns, so the
        // window can keep the rows of the profile it shows.
        assert_eq!(s.needs_you[0].profile_id, "");
        exec(
            &db,
            "UPDATE chats SET profile_id = 'p-sam' WHERE id = ?1",
            &[&a],
        );
        let s = summary(&db, &needs);
        assert_eq!(s.needs_you[0].profile_id, "p-sam");
        assert_eq!(s.unread[0].profile_id, "p-sam");
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
    fn finished_chats_are_held_not_listed_and_only_marks_need_attention() {
        let (_d, db) = temp_db();
        let (a, b) = (chat(&db, ""), chat(&db, "marked"));
        for c in [&a, &b] {
            exec(
                &db,
                "INSERT INTO chat_completions(chat_id, id, completed_at) VALUES (?1, ?2, 1)",
                &[c.as_str(), c.as_str()],
            );
        }
        exec(
            &db,
            "INSERT INTO unread_marks(chat_id, token, marked_at) VALUES (?1, 't', 1)",
            &[&b],
        );
        // A finished, unread, unmarked chat is held in `finished`, never listed or counted; a
        // marked chat is listed and counted, whatever its status. The marked chat is not held a
        // second time.
        let s = summary(&db, &Needs::new());
        assert_eq!(
            (s.badge, s.unread.len(), s.finished.len()),
            (1, 1, 1),
            "only the mark needs attention"
        );
        assert_eq!(s.unread[0].chat_id, b);
        assert_eq!(s.unread[0].reason, "Marked unread");
        assert_eq!(
            (
                s.finished[0].chat_id.as_str(),
                s.finished[0].title.as_str(),
                s.finished[0].reason.as_str()
            ),
            (a.as_str(), "Untitled", "Finished; not read yet")
        );
        exec(
            &db,
            "UPDATE chats SET status = 'stopped' WHERE id = ?1",
            &[&a],
        );
        assert!(
            summary(&db, &Needs::new()).finished.is_empty(),
            "a stopped chat holds nothing"
        );
        exec(
            &db,
            "UPDATE chats SET status = 'idle', error = 'boom' WHERE id = ?1",
            &[&a],
        );
        assert!(
            summary(&db, &Needs::new()).finished.is_empty(),
            "an error holds nothing"
        );
        exec(&db, "UPDATE chats SET error = NULL WHERE id = ?1", &[&a]);
        exec(
            &db,
            "UPDATE chat_completions SET read = 1 WHERE chat_id = ?1",
            &[&a],
        );
        assert!(
            summary(&db, &Needs::new()).finished.is_empty(),
            "a read completion holds nothing"
        );
    }

    #[test]
    fn a_pending_allow_request_raises_the_badge_without_an_open_chat() {
        let (_d, db) = temp_db();
        let a = chat(&db, "needs an allow");
        assert_eq!(
            summary(&db, &Needs::new()).badge,
            0,
            "no table, nobody waiting"
        );
        exec(
            &db,
            "CREATE TABLE consent_requests (chat_id TEXT, state TEXT, expires_at INTEGER)",
            &[],
        );
        exec(
            &db,
            "INSERT INTO consent_requests VALUES (?1, 'pending', 9999999999999)",
            &[&a],
        );
        let s = summary(&db, &Needs::new());
        assert_eq!(s.badge, 1);
        assert_eq!(s.needs_you[0].kind, AttentionKind::Approval);
        exec(&db, "UPDATE consent_requests SET state = 'allowed'", &[]);
        assert_eq!(
            summary(&db, &Needs::new()).badge,
            0,
            "decided: no longer waiting"
        );
    }

    #[test]
    fn a_muted_chat_keeps_what_it_holds_out_of_the_badge_and_the_idle_check_lists_its_priority() {
        let (_d, db) = temp_db();
        let (flag, ask, plain, quiet) = (
            chat(&db, "flagged"),
            chat(&db, "asks"),
            chat(&db, "plain"),
            chat(&db, "quiet"),
        );
        for c in [&flag, &ask, &plain, &quiet] {
            exec(
                &db,
                "INSERT INTO chat_mutes(chat_id, muted_at) VALUES (?1, 1)",
                &[c],
            );
        }
        exec(
            &db,
            "INSERT INTO chat_flags(chat_id, question, reason, priority, created_at) VALUES (?1, 'Ship it?', 'Blocked', 'normal', 1)",
            &[&flag],
        );
        // An unread result on a muted chat is not held for the user: it is just kept as read state.
        exec(
            &db,
            "INSERT INTO chat_completions(chat_id, id, completed_at) VALUES (?1, 'k1', 1)",
            &[&plain],
        );
        let mut needs = Needs::new();
        needs.insert(ask.clone(), AttentionKind::Approval);
        let s = summary(&db, &needs);
        assert_eq!(s.badge, 0, "muted chats raise nothing");
        assert!(
            s.needs_you.is_empty()
                && s.unread.is_empty()
                && s.finished.is_empty()
                && s.flagged.is_empty()
        );
        let held: Vec<(&str, MutedKind)> =
            s.muted.iter().map(|r| (r.title.as_str(), r.kind)).collect();
        assert_eq!(held.len(), 2, "{held:?}");
        assert!(held.contains(&("flagged", MutedKind::Flag)));
        assert!(held.contains(&("asks", MutedKind::Approval)));
        assert!(s
            .muted
            .iter()
            .any(|r| r.reason.contains("Asked by the agent: Ship it?")));
        // Unmuting moves the same flag into Needs you, and the badge counts it.
        exec(&db, "DELETE FROM chat_mutes WHERE chat_id = ?1", &[&flag]);
        let s = summary(&db, &needs);
        assert_eq!((s.badge, s.needs_you.len(), s.muted.len()), (1, 1, 1));
        // Set aside, a muted chat's held request is out too.
        exec(
            &db,
            "INSERT INTO chat_attention(chat_id, addressed_at) VALUES (?1, 9999999999999)",
            &[&ask],
        );
        assert!(summary(&db, &needs).muted.is_empty());

        // A muted chat that is interrupted, errored, unread, stalled and flagged at once is still
        // listed only in Muted: it adds nothing to the badge or to any other group.
        let (cut, stuck) = (chat(&db, "cut"), chat(&db, "stuck"));
        for (c, status) in [(&cut, "interrupted"), (&stuck, "running")] {
            exec(
                &db,
                "INSERT INTO chat_mutes(chat_id, muted_at) VALUES (?1, 1)",
                &[c],
            );
            exec(
                &db,
                "UPDATE chats SET status = ?2, error = 'boom' WHERE id = ?1",
                &[c.as_str(), status],
            );
            exec(
                &db,
                "INSERT INTO chat_completions(chat_id, id, completed_at) VALUES (?1, ?1, 1)",
                &[c],
            );
            exec(
                &db,
                "INSERT INTO chat_flags(chat_id, question, reason, priority, created_at) VALUES (?1, 'Ship it?', 'Blocked', 'normal', 1)",
                &[c],
            );
        }
        exec(
            &db,
            "INSERT INTO chat_activity(chat_id, last_activity_at, stalled) VALUES (?1, 0, 1)",
            &[&stuck],
        );
        let s = summary(&db, &needs);
        assert_eq!(s.muted.len(), 2);
        assert!(s.muted.iter().all(|r| r.kind == MutedKind::Flag));
        assert_eq!(s.badge, 1, "only the unmuted chat's request counts");
        assert!(
            s.stalled.is_empty()
                && s.unread.is_empty()
                && s.finished.is_empty()
                && s.flagged.is_empty()
        );
        assert!(
            s.running.is_empty(),
            "a muted chat is not in Running either"
        );
        assert_eq!(s.busy, 1, "keep-awake still counts it");

        // The idle check: a flagged chat carries its priority; a check that gave up is listed
        // with Retry; a retry takes it off the list until it fails again.
        let tri = |c: &str,
                   needs: i64,
                   failed: i64,
                   tries: i64,
                   reason: &str,
                   priority: Option<&str>| {
            db.write(|tx| {
                tx.execute(
                    "INSERT OR REPLACE INTO chat_triage(chat_id, stamp, checked_at, needs, reason, failed, tries, priority)
                     SELECT id, MAX(updated_at, COALESCE((SELECT last_activity_at FROM chat_activity WHERE chat_id = ?1), 0)), 1, ?2, ?3, ?4, ?5, ?6
                     FROM chats WHERE id = ?1",
                    rusqlite::params![c, needs, reason, failed, tries, priority],
                )
            })
            .unwrap();
        };
        tri(&plain, 1, 0, 1, "Tests were never run", Some("high"));
        tri(
            &quiet,
            0,
            1,
            crate::triage::MAX_TRIES,
            crate::triage::GAVE_UP,
            None,
        );
        exec(
            &db,
            "DELETE FROM chat_mutes WHERE chat_id IN (?1, ?2)",
            &[&plain, &quiet],
        );
        let s = summary(&db, &Needs::new());
        let mut rows: Vec<(String, Option<String>, bool)> = s
            .triage
            .iter()
            .map(|r| (r.title.clone(), r.priority.clone(), r.failed))
            .collect();
        rows.sort();
        assert_eq!(
            rows,
            [
                ("plain".to_owned(), Some("high".to_owned()), false),
                ("quiet".to_owned(), None, true)
            ]
        );
        assert_eq!(
            s.flagged.len(),
            1,
            "a failed check is not a flag, and not in the badge"
        );
        db.write(|tx| {
            assert!(crate::triage::retry(tx, &quiet)?);
            assert!(
                !crate::triage::retry(tx, &plain)?,
                "only a failed check is retried"
            );
            Ok::<_, rusqlite::Error>(())
        })
        .unwrap();
        assert_eq!(summary(&db, &Needs::new()).triage.len(), 1);
    }

    #[test]
    fn the_idle_checks_kind_decides_where_a_chat_is_listed_and_helpers_clear_it() {
        let (_d, db) = temp_db();
        let (old, decision, stalled, waiting, helped, waited) = (
            chat(&db, "old"),
            chat(&db, "decision"),
            chat(&db, "stalled"),
            chat(&db, "waiting"),
            chat(&db, "helped"),
            chat(&db, "waited"),
        );
        let tri = |c: &str, needs: i64, reason: &str, kind: Option<&str>| {
            db.write(|tx| {
                tx.execute(
                    "INSERT OR REPLACE INTO chat_triage(chat_id, stamp, checked_at, needs, reason, failed, tries, kind)
                     SELECT id, MAX(updated_at, COALESCE((SELECT last_activity_at FROM chat_activity WHERE chat_id = ?1), 0)), 1, ?2, ?3, 0, 1, ?4
                     FROM chats WHERE id = ?1",
                    rusqlite::params![c, needs, reason, kind],
                )
            })
            .unwrap();
        };
        // A verdict from before kinds is a decision.
        tri(&old, 1, "Pick the blue one.", None);
        tri(&decision, 1, "Approve the release.", Some("decision"));
        tri(&stalled, 1, "Never started.", Some("stalled"));
        tri(&waiting, 0, "", Some("waiting"));
        tri(&helped, 1, "Approve the release.", Some("decision"));
        tri(&waited, 1, "Never started.", Some("stalled"));
        let listed = |s: &AttentionSummary| {
            let names = |rows: &[AttentionRow]| {
                let mut v: Vec<String> = rows.iter().map(|r| r.title.clone()).collect();
                v.sort();
                v
            };
            (names(&s.flagged), names(&s.stalled), s.badge)
        };
        let s = summary(&db, &Needs::new());
        assert_eq!(
            listed(&s),
            (
                vec![
                    "decision".to_owned(),
                    "helped".to_owned(),
                    "old".to_owned()
                ],
                vec!["stalled".to_owned(), "waited".to_owned()],
                5
            ),
            "a decision is flagged, a stall is listed with the stuck chats, a chat waiting on its helpers is not listed"
        );
        let stall = s.stalled.iter().find(|r| r.title == "stalled").unwrap();
        assert_eq!(
            (stall.kind, stall.reason.as_str()),
            (AttentionKind::Stalled, "Never started.")
        );
        let kinds: Vec<(String, Option<String>)> = {
            let mut v: Vec<_> = s
                .triage
                .iter()
                .map(|r| (r.title.clone(), r.kind.clone()))
                .collect();
            v.sort();
            v
        };
        assert_eq!(kinds[0], ("decision".to_owned(), Some("decision".into())));
        assert!(kinds.contains(&("stalled".to_owned(), Some("stalled".into()))));
        assert!(!kinds.iter().any(|(t, _)| t == "waiting"));

        // A chat that starts a helper chat, or declares a wait, drops out at once.
        exec(
            &db,
            "UPDATE chats SET status = 'running', created_by_chat = ?1 WHERE id = ?2",
            &[&helped, &waiting],
        );
        // A nested chat that is working clears its parent too; an archived or an idle helper chat
        // does not.
        let (parented, archived_kid, idle_kid) = (
            chat(&db, "parented"),
            chat(&db, "archived kid"),
            chat(&db, "idle kid"),
        );
        let (kid, archived_child, idle_child) = (
            chat(&db, "kid"),
            chat(&db, "archived child"),
            chat(&db, "idle child"),
        );
        for c in [&parented, &archived_kid, &idle_kid] {
            tri(c, 1, "Approve the release.", Some("decision"));
        }
        exec(
            &db,
            "UPDATE chats SET status = 'running', parent_chat_id = ?1 WHERE id = ?2",
            &[&parented, &kid],
        );
        exec(
            &db,
            "UPDATE chats SET status = 'running', archived = 1, created_by_chat = ?1 WHERE id = ?2",
            &[&archived_kid, &archived_child],
        );
        exec(
            &db,
            "UPDATE chats SET status = 'idle', parent_chat_id = ?1, created_by_chat = ?1 WHERE id = ?2",
            &[&idle_kid, &idle_child],
        );
        exec(
            &db,
            "CREATE TABLE agent_waits(chat_id TEXT NOT NULL, state TEXT NOT NULL)",
            &[],
        );
        exec(
            &db,
            "INSERT INTO agent_waits(chat_id, state) VALUES (?1, 'open')",
            &[&waited],
        );
        exec(
            &db,
            "INSERT INTO agent_waits(chat_id, state) VALUES (?1, 'fired')",
            &[&stalled],
        );
        let s = summary(&db, &Needs::new());
        let (flagged, stuck, badge) = listed(&s);
        assert_eq!(
            (flagged, stuck, badge),
            (
                vec![
                    "archived kid".to_owned(),
                    "decision".to_owned(),
                    "idle kid".to_owned(),
                    "old".to_owned()
                ],
                vec!["stalled".to_owned()],
                5
            ),
            "a working helper chat or an open wait clears the flag; a settled wait, an archived or an idle helper does not"
        );
        assert!(s
            .triage
            .iter()
            .all(|r| !["helped", "waited", "parented"].contains(&r.title.as_str())));
    }

    #[test]
    fn running_chats_are_busy_and_groups_hold_eight_per_profile() {
        let (_d, db) = temp_db();
        let mut ids = vec![];
        for i in 0..10 {
            let c = chat(&db, &format!("c{i}"));
            exec(
                &db,
                "UPDATE chats SET status = 'running' WHERE id = ?1",
                &[&c],
            );
            ids.push(c);
        }
        let s = summary(&db, &Needs::new());
        assert_eq!(s.busy, 10);
        assert_eq!(s.running.len(), MAX_ROWS, "one profile: eight rows");
        assert_eq!(s.badge, 0, "running alone is not a badge");
        // Nine chats of one profile and one of another, the other profile's the oldest activity.
        // The cap is per profile, so the second profile's row and count survive the first
        // profile's overflow; a shared cap would have cut it off.
        for (i, c) in ids.iter().enumerate() {
            let (profile, at) = if i < 9 {
                ("p-one", 3000 + i)
            } else {
                ("p-two", 1000)
            };
            exec(
                &db,
                &format!(
                    "UPDATE chats SET profile_id = '{profile}', updated_at = {at} WHERE id = ?1"
                ),
                &[c],
            );
        }
        let s = summary(&db, &Needs::new());
        assert_eq!(s.busy, 10, "keep-awake still counts every busy chat");
        assert_eq!(
            s.running.len(),
            MAX_ROWS + 1,
            "eight of one, one of the other"
        );
        let s = s.running.iter().find(|r| r.profile_id == "p-two").unwrap();
        assert_eq!(s.reason, "Working");
        // The same per-profile cap holds for the unread group: all ten marked unread, the oldest
        // is still listed.
        for c in &ids {
            exec(
                &db,
                "INSERT INTO unread_marks(chat_id, token, marked_at) VALUES (?1, 't', 1)",
                &[c],
            );
        }
        let s = summary(&db, &Needs::new());
        assert_eq!(s.badge, 10, "every marked chat counts once");
        assert_eq!(s.unread.len(), MAX_ROWS + 1);
        assert!(s.unread.iter().any(|r| r.profile_id == "p-two"));
    }

    #[test]
    fn waiting_and_flagged_groups_list_every_chat_even_unread_ones() {
        let (_d, db) = temp_db();
        let mut needs = Needs::new();
        for i in 0..10 {
            let c = chat(&db, &format!("w{i}"));
            needs.insert(c, AttentionKind::Question);
        }
        let mut flagged = vec![];
        for i in 0..10 {
            let c = chat(&db, &format!("f{i}"));
            exec(
                &db,
                "INSERT INTO chat_triage(chat_id, stamp, checked_at, needs, reason, failed)                  SELECT id, updated_at, 1, 1, 'Tests were never run', 0 FROM chats WHERE id = ?1",
                &[&c],
            );
            flagged.push(c);
        }
        // One flagged chat is also unread: it still has its flag.
        exec(
            &db,
            "INSERT INTO unread_marks(chat_id, token, marked_at) VALUES (?1, 't', 1)",
            &[&flagged[0]],
        );
        let s = summary(&db, &needs);
        assert_eq!(s.badge, 20);
        assert_eq!(s.needs_you.len(), 10, "no cap on chats waiting on you");
        assert_eq!(
            s.flagged.len(),
            10,
            "no cap, and unread chats keep their flag"
        );
        assert!(s.flagged.iter().any(|r| r.chat_id == flagged[0]));
    }
}
