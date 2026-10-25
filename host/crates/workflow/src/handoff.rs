//! `hand_off` and `finish`. A hand-off binds each change to its approved commit
//! (`{tip, tree, base}`, or the latest build's when review is off) and moves it toward the land
//! step by the mode the **host** computes from its own settings: draft rule → `landing_draft`,
//! merge rule → `pending_land`. The land step itself is [`crate::land`].

use crate::actions::{self, Act, Out};
use crate::engine;
use crate::store::{self, Event};
use crate::types::*;
use rusqlite::Transaction;
use serde_json::{json, Value};

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
            if c.approved_tip.as_ref() != c.tip.as_ref() {
                // The approval holds by tree: bind the tip that carries it.
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
