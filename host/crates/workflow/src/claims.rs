//! Who holds a change, a board item started twice, and moving a change between chats.
//!
//! - A change is **held** by its task while the change is not landed, flipped or dropped and the
//!   task is not ended or finished. Finishing, dropping and ending release; so does landing.
//! - **Opening** a change (`update`, a build report) that another task holds is refused, naming
//!   the holder and the ways on: `take_over`, or for this chat's own other task, Reopen or drop.
//! - **Starting** a task with a board item id that another chat's live task carries is refused the
//!   same way. An archived chat's tasks never block, and the user's words or an Allow grant let a
//!   second start through.
//! - **Take-over** (`take_over {repo, branch, reason, check?}`) is decided here from the host's own
//!   records. It is never allowed mid-land or for a declined change. It goes through without the
//!   user when the holder is archived or stopped (its task parked, or its chat idle for 30 minutes,
//!   or the change untouched for 6 hours); otherwise it needs the user's words or an Allow grant.
//!   Approvals never move: the claimer's change starts fresh with only the PR kept, the holder's
//!   change is dropped, and both chats get a row and a transcript note.
//! - **Hand-off** of a change an earlier task of another chat still holds is refused (the first
//!   claimant wins), which covers holds made before this check existed.

use crate::actions::{self, need, target, text, Act, Out, MAX_REASON};
use crate::engine;
use crate::kinds::{self, Proof};
use crate::store::{self, Change, Event, Run};
use helm_consent::Canonical;
use rusqlite::{params, OptionalExtension, Transaction};
use serde_json::{json, Value};

/// A holder whose chat has been idle this long counts as stopped.
pub const IDLE_MS: i64 = 30 * 60 * 1000;
/// A change nothing has touched for this long counts as stopped, even in a busy chat.
pub const UNTOUCHED_MS: i64 = 6 * 60 * 60 * 1000;

/// The states in which a change still holds its branch.
const HOLDING: &str = "c.state NOT IN ('landed', 'flipped', 'dropped')
    AND r.status IN ('active', 'parked', 'handed_off')";

#[derive(Clone, Debug)]
pub struct Holder {
    pub run: Run,
    pub change: Change,
    pub chat_title: String,
    pub archived: bool,
    pub busy: bool,
    /// Sub-agents or background tasks of the holder's chat are running (read from the live
    /// runtime before the transaction; only a take-over fills it in).
    pub agents_running: bool,
    /// The later of the change's last write and the chat's.
    pub touched_at: i64,
}

impl Holder {
    /// Stopped: the holder's own work has gone quiet, so nobody needs to be asked.
    pub fn stopped(&self, now: i64) -> Option<&'static str> {
        if self.agents_running {
            // Work is still running there, whatever the clock says.
            None
        } else if self.archived {
            Some("its chat is archived")
        } else if self.run.status == "parked" {
            Some("its task is parked")
        } else if !self.busy && now - self.touched_at >= IDLE_MS {
            Some("its chat has been idle for 30 minutes or more")
        } else if now - self.touched_at > UNTOUCHED_MS {
            Some("nothing has touched it for more than 6 hours")
        } else {
            None
        }
    }

    fn describe(&self, now: i64) -> String {
        let mins = ((now - self.touched_at).max(0) / 60_000).max(0);
        let pr = self
            .change
            .pr
            .as_deref()
            .map(|p| format!(", PR {p}"))
            .unwrap_or_default();
        format!(
            "task \"{}\" ({}, change {}) in chat \"{}\" ({}), last touched {mins} min ago{pr}",
            crate::mode::tooltip_reason(&self.run.title),
            self.run.status,
            self.change.state,
            crate::mode::tooltip_reason(&self.chat_title),
            self.run.chat_id,
        )
    }
}

/// Every task holding `repo` `branch`, oldest claim first.
pub fn holders(tx: &Transaction<'_>, repo: &str, branch: &str) -> rusqlite::Result<Vec<Holder>> {
    let mut st = tx.prepare(&format!(
        "SELECT c.run_id, MAX(c.updated_at, ch.updated_at), ch.title, ch.archived, ch.status
         FROM workflow_changes c JOIN workflow_runs r ON r.id = c.run_id
         JOIN chats ch ON ch.id = r.chat_id
         WHERE c.repo = ?1 AND c.branch = ?2 AND {HOLDING}
         ORDER BY c.rowid"
    ))?;
    let rows = st
        .query_map(params![repo, branch], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, i64>(3)? != 0,
                r.get::<_, String>(4)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    let mut out = Vec::new();
    for (run_id, touched_at, chat_title, archived, status) in rows {
        let (Some(run), Some(change)) = (
            store::run(tx, &run_id)?,
            store::change(tx, &run_id, repo, branch)?,
        ) else {
            continue;
        };
        out.push(Holder {
            run,
            change,
            chat_title,
            archived,
            busy: matches!(status.as_str(), "starting" | "running" | "stopping"),
            agents_running: false,
            touched_at,
        });
    }
    Ok(out)
}

/// The refusal for opening `repo` `branch` in `run`, when another task holds it.
pub fn open_refusal(
    tx: &Transaction<'_>,
    run: &Run,
    repo: &str,
    branch: &str,
    now: i64,
) -> rusqlite::Result<Option<String>> {
    let Some(h) = holders(tx, repo, branch)?
        .into_iter()
        .find(|h| h.run.id != run.id)
    else {
        return Ok(None);
    };
    if h.run.chat_id == run.chat_id {
        return Ok(Some(format!(
            "{branch} is already held by this chat's other task \"{}\" ({}). Reopen that task to carry on with it, or release it with app_workflow drop {{repo, branch, reason}}.",
            crate::mode::tooltip_reason(&h.run.title),
            h.run.status
        )));
    }
    Ok(Some(format!(
        "{branch} in {repo} is held by {}. One change belongs to one task. Check it with app_workflow take_over {{repo, branch, check: true}} (changes nothing). {}",
        h.describe(now),
        take_over_hint(&h, now),
    )))
}

fn take_over_hint(h: &Holder, now: i64) -> String {
    match h.stopped(now) {
        Some(why) => format!(
            "Take it over with take_over {{repo, branch, reason}}: {why}, so it should go through without the user (unless agents are still running there)."
        ),
        None => "Taking it over (take_over {repo, branch, reason}) needs the user while that chat is active: ask them, or let that chat drop the change.".into(),
    }
}

/// The refusal for starting a task for `item` here while another chat's live task carries it.
pub fn item_refusal(
    tx: &Transaction<'_>,
    chat: &str,
    item: &str,
) -> rusqlite::Result<Option<(String, String)>> {
    let other = tx
        .query_row(
            "SELECT r.title, r.status, ch.title, ch.id FROM workflow_runs r JOIN chats ch ON ch.id = r.chat_id
             WHERE r.item_id = ?1 AND r.chat_id != ?2 AND ch.archived = 0
               AND (r.status = 'active' OR (r.status = 'handed_off' AND EXISTS (
                   SELECT 1 FROM workflow_changes c WHERE c.run_id = r.id
                     AND c.state NOT IN ('landed', 'flipped', 'dropped', 'declined'))))
             ORDER BY r.created_at LIMIT 1",
            params![item, chat],
            |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                ))
            },
        )
        .optional()?;
    Ok(other.map(|(task, status, title, id)| {
        (
            format!(
                "Board item {item} is already being worked on by task \"{}\" ({status}) in chat \"{}\" ({id}). Two orchestrators on one item would duplicate the work. Leave it to that chat, start this task without itemId, or, if the user wants a second start here, ask them.",
                crate::mode::tooltip_reason(&task),
                crate::mode::tooltip_reason(&title),
            ),
            id,
        )
    }))
}

/// Check a start's item id; with the user's proof the second start goes through.
pub fn check_item(
    tx: &Transaction<'_>,
    x: &Act<'_>,
    item: &str,
    a: &Value,
) -> rusqlite::Result<Result<bool, String>> {
    let Some((why, _)) = item_refusal(tx, x.chat_id, item)? else {
        return Ok(Ok(false));
    };
    let t = json!({"itemId": item});
    if let Err(e) = kinds::consent(
        tx,
        x.kinds,
        x.chat_id,
        "takeOver",
        &t,
        &Proof::from_args(a),
        &why,
        x.now,
    )? {
        return Ok(Err(e));
    }
    Ok(Ok(true))
}

/// The `takeOver` allow kind's canonical target: a change another chat holds, or a board item.
pub fn canonical(tx: &Transaction<'_>, chat: &str, t: &Value) -> Result<Canonical, String> {
    if let Some(item) = t.get("itemId").and_then(Value::as_str) {
        if item.is_empty() || item.len() > 64 {
            return Err("itemId must be 1 to 64 characters.".into());
        }
        let (_, holder) = item_refusal(tx, chat, item)
            .map_err(|e| e.to_string())?
            .ok_or("No other chat is working on that board item.")?;
        return Ok(Canonical {
            target: json!({"itemId": item, "holder": holder}),
            summary: format!("Start a second task for board item {item} in this chat"),
            fixed_form: format!("Take over item {item}."),
        });
    }
    let (repo, branch) = target(t)?;
    let h = holders(tx, &repo, &branch)
        .map_err(|e| e.to_string())?
        .into_iter()
        .find(|h| h.run.chat_id != chat)
        .ok_or_else(|| format!("No other chat holds {repo} {branch}."))?;
    let label = kinds::pr_label(h.change.pr.as_deref(), &branch);
    Ok(Canonical {
        target: json!({"repo": repo, "branch": branch, "holderRun": h.run.id}),
        summary: format!(
            "Take over {label} ({repo}) from chat \"{}\"",
            crate::mode::tooltip_reason(&h.chat_title)
        ),
        fixed_form: format!("Take over {label}."),
    })
}

/// A change in `run` that an earlier task of another chat still holds: the hand-off is refused.
pub fn earlier_holder(
    tx: &Transaction<'_>,
    run: &Run,
    c: &Change,
    now: i64,
) -> rusqlite::Result<Option<String>> {
    let first = holders(tx, &c.repo, &c.branch)?.into_iter().next();
    Ok(first
        .filter(|h| h.run.id != run.id && h.run.chat_id != run.chat_id)
        .map(|h| {
            format!(
                "{}: an earlier claim holds it ({}); take it over or drop it",
                c.branch,
                h.describe(now)
            )
        }))
}

/// Why a holder's change can't be taken over at all, whoever asks.
fn blocked_by(h: &Holder) -> Option<String> {
    match h.change.state.as_str() {
        "landing_draft" | "pending_land" | "landing" => {
            Some("it is being landed right now; wait until its land finishes or fails".to_owned())
        }
        "declined" => {
            Some("the user declined its merge; only End task on that chat releases it".to_owned())
        }
        "open" => None,
        _ if h.archived => None,
        s => Some(format!(
            "it is {s} in a chat that is not archived; the user's Drop change or End task on that chat releases it"
        )),
    }
}

/// The chats holding `repo` `branch` per the arguments of a `take_over`, read before its
/// transaction so the caller can ask the live runtime which of them still run agents.
pub fn holder_chats(db: &helm_core::db::Database, a: &Value) -> Vec<String> {
    let Ok((repo, branch)) = target(a) else {
        return Vec::new();
    };
    db.snapshot(|tx| holders(tx, &repo, &branch))
        .map(|hs| hs.into_iter().map(|h| h.run.chat_id).collect())
        .unwrap_or_default()
}

/// `take_over {repo, branch, reason, check?, userQuote?, grant?}`. `agents_live` lists the
/// holder chats whose sub-agents or background tasks are running: they never count as stopped.
pub fn take_over(tx: &Transaction<'_>, x: &Act<'_>, a: &Value, agents_live: &[String]) -> Out {
    let (repo, branch) = match target(a) {
        Ok(t) => t,
        Err(e) => return Ok(Err(e)),
    };
    let check = a.get("check").and_then(Value::as_bool).unwrap_or(false);
    let reason = if check {
        text(a, "reason", MAX_REASON).ok().flatten().unwrap_or("")
    } else {
        match need(a, "reason", MAX_REASON) {
            Ok(r) => r,
            Err(e) => return Ok(Err(e)),
        }
    };
    let run = match actions::active(tx, x.chat_id)? {
        Ok(r) => r,
        Err(e) => return Ok(Err(e)),
    };
    let all = holders(tx, &repo, &branch)?;
    if let Some(own) = all
        .iter()
        .find(|h| h.run.chat_id == x.chat_id && h.run.id != run.id)
    {
        return Ok(Err(format!(
            "This chat's own task \"{}\" ({}) holds {branch}: Reopen it, or release it with drop {{repo, branch, reason}}. take_over moves changes between chats.",
            crate::mode::tooltip_reason(&own.run.title),
            own.run.status
        )));
    }
    let others: Vec<Holder> = all
        .into_iter()
        .filter(|h| h.run.chat_id != x.chat_id)
        .map(|mut h| {
            h.agents_running = agents_live.contains(&h.run.chat_id);
            h
        })
        .collect();
    let Some(h) = others.first() else {
        return Ok(Err(format!(
            "No other chat holds {repo} {branch}; open it with update {{repo, branch}}."
        )));
    };
    // The verdict, from the host's own records only. Every holder must allow it (there is
    // normally one; holds made before the claim check could leave more).
    let blocked = others.iter().find_map(|o| blocked_by(o).map(|w| (o, w)));
    let blocked = blocked.map(|(o, w)| {
        if o.run.id == h.run.id {
            w
        } else {
            format!(
                "{w} (in chat \"{}\")",
                crate::mode::tooltip_reason(&o.chat_title)
            )
        }
    });
    let stopped = h.stopped(x.now);
    let all_stopped = others.iter().all(|o| o.stopped(x.now).is_some());
    let needs_user = blocked.is_none() && !all_stopped;
    if check {
        return Ok(Ok(json!({
            "check": true,
            "holder": {"chatId": h.run.chat_id, "chatTitle": h.chat_title, "task": h.run.title,
                "taskStatus": h.run.status, "state": h.change.state, "pr": h.change.pr,
                "archived": h.archived, "running": h.busy},
            "wouldGoThrough": blocked.is_none() && !needs_user,
            "needsUser": needs_user,
            "why": blocked.clone().or(stopped.map(str::to_owned)).unwrap_or_else(|| "that chat is active, so the user must agree".into()),
        })));
    }
    if let Some(why) = blocked {
        return Ok(Err(format!("{branch} can't be taken over: {why}.")));
    }
    let existing = store::change(tx, &run.id, &repo, &branch)?;
    if existing.is_none() && store::changes(tx, &run.id)?.len() >= 32 {
        return Ok(Err("A task holds at most 32 changes.".into()));
    }
    let mut by_user = false;
    if needs_user {
        let t = json!({"repo": repo, "branch": branch});
        let why = format!(
            "{branch} is held by {}, which is active.",
            h.describe(x.now)
        );
        if let Err(e) = kinds::consent(
            tx,
            x.kinds,
            x.chat_id,
            "takeOver",
            &t,
            &Proof::from_args(a),
            &why,
            x.now,
        )? {
            return Ok(Err(e));
        }
        by_user = true;
    }
    // Apply: every other chat's hold is released; approvals never move.
    let label = kinds::pr_label(h.change.pr.as_deref(), &branch);
    let claimer_title = tx
        .query_row("SELECT title FROM chats WHERE id = ?1", [x.chat_id], |r| {
            r.get::<_, String>(0)
        })
        .optional()?
        .unwrap_or_default();
    let pr = h.change.pr.clone();
    for h in &others {
        let mut c = h.change.clone();
        if let Err(e) = engine::move_change(tx, &h.run, &mut c, "dropped", false, x.now)? {
            return Ok(Err(e));
        }
        let body = format!(
            "Taken over by chat \"{}\": {reason}",
            crate::mode::tooltip_reason(&claimer_title)
        );
        store::event(
            tx,
            &h.run.chat_id,
            Some(&h.run),
            &Event {
                kind: "change_dropped",
                repo: Some(&repo),
                branch: Some(&branch),
                verdict: Some("taken_over"),
                body: Some(&body),
                ..Default::default()
            },
            x.now,
        )?;
        store::event(
            tx,
            &h.run.chat_id,
            Some(&h.run),
            &Event {
                kind: "claim_takeover",
                repo: Some(&repo),
                branch: Some(&branch),
                verdict: Some("released"),
                body: Some(&body),
                ..Default::default()
            },
            x.now,
        )?;
        crate::notes::queue(
            &h.run.chat_id,
            format!(
                "Workflow: {label} ({repo}) was taken over by chat \"{}\" and dropped from task \"{}\". This chat no longer holds it; do not push to it from here. Reason: {reason}",
                crate::mode::tooltip_reason(&claimer_title),
                crate::mode::tooltip_reason(&h.run.title)
            ),
            true,
            format!("wf-takeover-{}-{}-{}", h.run.id, x.now, branch),
        );
    }
    if existing.is_none() {
        store::upsert_change(
            tx,
            &Change {
                run_id: run.id.clone(),
                repo: repo.clone(),
                branch: branch.clone(),
                pr: pr.clone(),
                state: "open".into(),
                ..Default::default()
            },
            x.now,
        )?;
    }
    let body = format!(
        "Taken over from chat \"{}\" ({}): {reason}",
        crate::mode::tooltip_reason(&h.chat_title),
        if by_user {
            "the user agreed"
        } else {
            stopped.unwrap_or("its holder had stopped")
        },
    );
    store::event(
        tx,
        x.chat_id,
        Some(&run),
        &Event {
            kind: "claim_takeover",
            repo: Some(&repo),
            branch: Some(&branch),
            verdict: Some(if by_user { "by_user" } else { "stale" }),
            body: Some(&body),
            ..Default::default()
        },
        x.now,
    )?;
    crate::notes::queue(
        x.chat_id,
        format!("Workflow: took over {label} ({repo}) into task \"{}\". {body}. It starts fresh: build, review and approval happen again here.", crate::mode::tooltip_reason(&run.title)),
        false,
        format!("wf-takeover-{}-{}-{}", run.id, x.now, branch),
    );
    Ok(Ok(json!({
        "takenOver": true, "repo": repo, "branch": branch, "pr": pr,
        "from": {"chatId": h.run.chat_id, "chatTitle": h.chat_title, "task": h.run.title},
        "withUser": by_user,
        "next": "The change starts fresh in this task: its approvals did not move. Build, review and hand off as usual.",
    })))
}
