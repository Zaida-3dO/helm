//! `hand_off` and `finish`. A hand-off binds each change to the commit it carries its review
//! at (`{tip, tree, base}`, or the latest build's when review is off) and moves it toward the
//! land step by the mode the **host** computes from its own settings: draft rule →
//! `landing_draft`, merge rule → `pending_land`. A change whose review ran but no longer binds
//! at the latest commit still hands off — the land step nudges on that. The land step itself
//! is [`crate::land`].

use crate::actions::{self, Act, Out};
use crate::engine;
use crate::store::{self, Event};
use crate::types::*;
use rusqlite::Transaction;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

pub const MAX_SUMMARY: usize = 8000;

fn named(a: &Value) -> Result<Option<Vec<(String, String)>>, String> {
    let Some(list) = a.get("changes") else {
        return Ok(None);
    };
    let arr = list
        .as_array()
        .ok_or("changes must be a list of {repo, branch}.")?;
    if arr.len() > 32 {
        return Err("changes holds at most 32 entries.".into());
    }
    arr.iter()
        .map(actions::target)
        .collect::<Result<Vec<_>, _>>()
        .map(Some)
}

pub fn hand_off(tx: &Transaction<'_>, x: &Act<'_>, a: &Value) -> Out {
    let mut run = match actions::active(tx, x.chat_id)? {
        Ok(r) => r,
        Err(e) => return Ok(Err(e)),
    };
    if let Err(e) = actions::text(a, "summary", MAX_SUMMARY) {
        return Ok(Err(e));
    }
    let names = match named(a) {
        Ok(n) => n,
        Err(e) => return Ok(Err(e)),
    };
    let final_ = a
        .get("final")
        .and_then(Value::as_bool)
        .unwrap_or(names.is_none());
    let all = store::changes(tx, &run.id)?;
    let open: Vec<_> = all.iter().filter(|c| c.state == "open").cloned().collect();
    let chosen: Vec<_> = match &names {
        Some(n) => {
            let mut out = Vec::new();
            for (repo, branch) in n {
                match open.iter().find(|c| &c.repo == repo && &c.branch == branch) {
                    Some(c) => out.push(c.clone()),
                    None => {
                        return Ok(Err(format!(
                            "{repo} {branch} is not an open change of this task."
                        )))
                    }
                }
            }
            out
        }
        None => open.clone(),
    };
    let mut ready = Vec::new();
    let mut missing = Vec::new();
    for c in &chosen {
        let mut m = engine::missing(x.s, &run, c, &all);
        // The first claimant wins: a change an earlier task of another chat holds stays there.
        m.extend(crate::claims::earlier_holder(tx, &run, c, x.now)?);
        if m.is_empty() {
            ready.push(c.clone());
        } else {
            missing.extend(m);
        }
    }
    // Final and partial are all or nothing; final:false hands off whatever holds.
    let all_or_nothing = final_ || names.is_some();
    if (all_or_nothing && !missing.is_empty())
        || (final_ && names.is_some() && chosen.len() != open.len())
    {
        let mut why = missing.clone();
        if final_ && names.is_some() && chosen.len() != open.len() {
            why.push("a final hand-off must name every remaining change".into());
        }
        let ready_names: Vec<&str> = ready.iter().map(|c| c.branch.as_str()).collect();
        return Ok(Err(format!(
            "Nothing was handed off. Missing: {}.{}",
            why.join("; "),
            if final_ && !ready_names.is_empty() {
                format!(
                    " Ready now (hand them off with changes: [...]): {}.",
                    ready_names.join(", ")
                )
            } else {
                String::new()
            }
        )));
    }
    if final_ {
        let dup = store::count_events(
            tx,
            &run.id,
            "handoff_requested",
            Some(run.attempt),
            None,
            None,
            None,
        )? > 0;
        if dup {
            store::event(
                tx,
                x.chat_id,
                Some(&run),
                &Event {
                    kind: "handoff_duplicate",
                    ..Default::default()
                },
                x.now,
            )?;
            return Ok(Err(
                "This task was already handed off in this attempt.".into()
            ));
        }
    }
    let mut out = Vec::new();
    for mut c in ready {
        let (mode, method) = crate::settings::land_mode(x.s, &c.repo);
        if x.s.stages.review.use_ == StageUse::Required {
            if engine::approval_holds(&c) && c.approved_tip.as_ref() != c.tip.as_ref() {
                // The approval holds by tree: bind the tip that carries it. When it does not
                // hold at all (the code moved on since the review), the reviewed commit stays
                // as it was: the land step nudges from it, it does not claim a new approval.
                c.approved_tip = c.tip.clone();
            }
        } else {
            c.approved_tip = c.tip.clone();
            c.approved_tree = c.tree.clone();
            c.approved_base = c.base.clone();
        }
        c.handoff_attempt = Some(run.attempt);
        c.mode = Some(match mode {
            LandMode::Draft => "draft".into(),
            LandMode::Merge => format!(
                "merge:{}",
                serde_json::to_value(method)
                    .ok()
                    .and_then(|v| v.as_str().map(str::to_owned))
                    .unwrap_or_else(|| "merge".into())
            ),
        });
        let to = if mode == LandMode::Draft {
            "landing_draft"
        } else {
            "pending_land"
        };
        if let Err(e) = engine::move_change(tx, &run, &mut c, to, true, x.now)? {
            return Ok(Err(e));
        }
        store::event(
            tx,
            x.chat_id,
            Some(&run),
            &Event {
                kind: "change_handoff",
                repo: Some(&c.repo),
                branch: Some(&c.branch),
                tip: c.approved_tip.as_deref(),
                tree: c.approved_tree.as_deref(),
                verdict: c.mode.as_deref(),
                ..Default::default()
            },
            x.now,
        )?;
        out.push(json!({"repo": c.repo, "branch": c.branch, "tip": c.approved_tip, "mode": c.mode, "state": c.state}));
    }
    if final_ {
        store::event(
            tx,
            x.chat_id,
            Some(&run),
            &Event {
                kind: "handoff_requested",
                body: actions::text(a, "summary", MAX_SUMMARY).ok().flatten(),
                ..Default::default()
            },
            x.now,
        )?;
        run.status = "handed_off".into();
        store::save_run(tx, &run, None, x.now)?;
    }
    Ok(Ok(json!({
        "handedOff": out, "final": final_, "taskStatus": run.status,
        "missing": if final_ { Vec::<String>::new() } else { missing },
    })))
}

/// `finish`: close a task with no open changes (finished), or one whose changes are all handed
/// off, landed or dropped (handed off).
pub fn finish(tx: &Transaction<'_>, x: &Act<'_>) -> Out {
    let mut run = match actions::active(tx, x.chat_id)? {
        Ok(r) => r,
        Err(e) => return Ok(Err(e)),
    };
    let all = store::changes(tx, &run.id)?;
    let open: Vec<&str> = all
        .iter()
        .filter(|c| c.state == "open")
        .map(|c| c.branch.as_str())
        .collect();
    if !open.is_empty() {
        return Ok(Err(format!(
            "These changes are still open: {}. Hand them off or drop them first.",
            open.join(", ")
        )));
    }
    let live = all.iter().any(|c| c.state != "dropped");
    run.status = if live {
        "handed_off".into()
    } else {
        "finished".into()
    };
    store::save_run(tx, &run, None, x.now)?;
    let kind = if live {
        "handoff_requested"
    } else {
        "finished"
    };
    if !live
        || store::count_events(
            tx,
            &run.id,
            "handoff_requested",
            Some(run.attempt),
            None,
            None,
            None,
        )? == 0
    {
        store::event(
            tx,
            x.chat_id,
            Some(&run),
            &Event {
                kind,
                ..Default::default()
            },
            x.now,
        )?;
    }
    Ok(Ok(json!({"runId": run.id, "status": run.status})))
}

/// What the turn-end check has to say about a chat: the key that makes it once-per-state, and
/// the words. Changes that hold (approved at their latest commit) but are not handed off, or every
/// change handed off with the task still open. Unreviewed work is not this check's business: a
/// turn that ends with a question about it is nothing to flag.
pub struct TurnEndFinding {
    pub key: String,
    pub body: String,
    run: store::Run,
}

/// Read-only: the finding for a chat's active task, if there is one.
pub fn turn_end_finding(
    tx: &Transaction<'_>,
    chat_id: &str,
) -> rusqlite::Result<Option<TurnEndFinding>> {
    let s = crate::settings::load(tx)?;
    if !s.enabled || !crate::mode::in_effect(tx, chat_id, &s)? {
        return Ok(None);
    }
    let Some(run) = store::active_run(tx, chat_id)? else {
        return Ok(None);
    };
    let all = store::changes(tx, &run.id)?;
    let built: Vec<_> = all
        .iter()
        .filter(|c| c.state == "open" && c.tip.is_some() && engine::approval_holds(c))
        .collect();
    let (key, body) = if !built.is_empty() {
        let mut tips: Vec<String> = built
            .iter()
            .map(|c| format!("{}@{}", c.branch, c.tip.as_deref().unwrap_or("")))
            .collect();
        tips.sort();
        let names: Vec<&str> = built.iter().map(|c| c.branch.as_str()).collect();
        (
            hex::encode(Sha256::digest(tips.join("|").as_bytes()))[..16].to_owned(),
            format!(
                "Built but not handed off: {}. Hand them off with app_workflow hand_off, or say what is holding them back.",
                names.join(", ")
            ),
        )
    } else if all.iter().any(|c| c.state != "dropped") && all.iter().all(|c| c.state != "open") {
        (
            format!("task:{}", run.attempt),
            "Every change is handed off, landed or dropped, but the task is still open. Close it with app_workflow finish.".to_owned(),
        )
    } else {
        return Ok(None);
    };
    Ok(Some(TurnEndFinding { key, body, run }))
}

/// The turn-end check, for the worker's Stop hook. When the orchestrator's turn ends with
/// approved changes not handed off (and no agent still running), or with every change handed off
/// but the task itself still open, one `not_handed_off` row is written, which the panel lists
/// under Needs you. It is written once per set of tips (once per attempt for the second case), so
/// a quiet chat is not nagged. Returns the words the agent is told when the row was newly written
/// (the hook blocks the stop once with them), and `None` otherwise.
pub fn turn_end_check(
    tx: &Transaction<'_>,
    chat_id: &str,
    agents_busy: bool,
    now: i64,
) -> rusqlite::Result<Option<String>> {
    if agents_busy {
        return Ok(None);
    }
    let Some(f) = turn_end_finding(tx, chat_id)? else {
        return Ok(None);
    };
    if store::count_events(
        tx,
        &f.run.id,
        "not_handed_off",
        Some(f.run.attempt),
        None,
        None,
        Some(&f.key),
    )? > 0
    {
        return Ok(None);
    }
    store::event(
        tx,
        chat_id,
        Some(&f.run),
        &Event {
            kind: "not_handed_off",
            verdict: Some(&f.key),
            body: Some(&f.body),
            ..Default::default()
        },
        now,
    )?;
    Ok(Some(f.body))
}
