//! The workflow's allow kinds on the consent primitive, and the one
//! helper every consented step goes through. The primitive itself is consumed, never changed.
//!
//! Every target is built by the host from its own records and is run-scoped (task id plus
//! attempt), so nothing allowed before a Reopen counts after it.

use crate::store;
use helm_consent::{AllowKind, AllowKinds, Canonical, ConsentRecord};
use rusqlite::Transaction;
use serde_json::{json, Value};

pub fn register_allow_kinds(k: &mut AllowKinds) {
    k.register(kind("land", "Merge pull request", land))
        .register(kind("ready", "Mark ready for review", ready))
        .register(kind("drop", "Drop a change", drop))
        .register(kind("skip", "Skip a stage", skip))
        .register(kind("allowMore", "Allow more rounds", allow_more))
        .register(kind("reopen", "Reopen the task", reopen));
}

fn kind(
    id: &'static str,
    label: &'static str,
    canonical: helm_consent::kinds::CanonicalFn,
) -> AllowKind {
    AllowKind {
        id,
        label,
        accepts_words: true,
        canonical,
    }
}

fn s<'a>(t: &'a Value, k: &str) -> Result<&'a str, String> {
    t.get(k)
        .and_then(Value::as_str)
        .filter(|v| !v.is_empty() && v.len() <= 255)
        .ok_or_else(|| format!("The target needs {k}."))
}

/// `#N` for a PR URL ending `/pull/N`, else the branch.
pub fn pr_label(pr: Option<&str>, branch: &str) -> String {
    pr.and_then(|p| p.rsplit_once("/pull/"))
        .map(|(_, n)| n.trim_end_matches('/'))
        .filter(|n| !n.is_empty() && n.len() <= 9 && n.bytes().all(|b| b.is_ascii_digit()))
        .map(|n| format!("#{n}"))
        .unwrap_or_else(|| branch.to_owned())
}

fn stage_label(stage: &str) -> Option<&'static str> {
    Some(match stage {
        "plan" => "plan",
        "planReview" => "plan review",
        "review" => "review",
        "cleanup" => "clean-up",
        "smoketest" => "smoketest",
        _ => return None,
    })
}

fn active(tx: &Transaction<'_>, chat: &str) -> Result<store::Run, String> {
    store::active_run(tx, chat)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "This chat has no active task.".to_owned())
}

/// A change of this chat's tasks, newest task first.
fn change_target(
    tx: &Transaction<'_>,
    chat: &str,
    t: &Value,
) -> Result<(store::Run, store::Change), String> {
    let repo =
        crate::settings::normalize_repo(s(t, "repo")?).ok_or("repo must be host/owner/name")?;
    let branch = s(t, "branch")?;
    // A step that already knows its task names it, so the consent binds to exactly that task.
    if let Some(id) = t.get("runId").and_then(Value::as_str) {
        let run = store::run(tx, id)
            .map_err(|e| e.to_string())?
            .filter(|r| r.chat_id == chat)
            .ok_or("That task is not one of this chat's.")?;
        let c = store::change(tx, &run.id, &repo, branch)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| format!("That task has no change {repo} {branch}."))?;
        return Ok((run, c));
    }
    for run in store::open_runs(tx, chat).map_err(|e| e.to_string())? {
        if let Some(c) = store::change(tx, &run.id, &repo, branch).map_err(|e| e.to_string())? {
            return Ok((run, c));
        }
    }
    Err(format!(
        "No task in this chat has the change {repo} {branch}."
    ))
}

fn change_canonical(
    tx: &Transaction<'_>,
    chat: &str,
    t: &Value,
    verb: &str,
    form: impl Fn(&str) -> String,
) -> Result<Canonical, String> {
    let (run, c) = change_target(tx, chat, t)?;
    let label = pr_label(c.pr.as_deref(), &c.branch);
    Ok(Canonical {
        target: json!({"runId": run.id, "attempt": run.attempt, "repo": c.repo, "branch": c.branch}),
        summary: format!(
            "{verb} {label} ({}) in task \"{}\"",
            c.repo,
            crate::mode::tooltip_reason(&run.title)
        ),
        fixed_form: form(&label),
    })
}

fn land(tx: &Transaction<'_>, chat: &str, t: &Value) -> Result<Canonical, String> {
    change_canonical(tx, chat, t, "Merge", |l| format!("Merge {l}."))
}

fn ready(tx: &Transaction<'_>, chat: &str, t: &Value) -> Result<Canonical, String> {
    change_canonical(tx, chat, t, "Mark ready", |l| format!("Mark {l} ready."))
}

fn drop(tx: &Transaction<'_>, chat: &str, t: &Value) -> Result<Canonical, String> {
    // "Drop change #N." rather than "Drop #N.": a one-digit PR would be under the verifier's
    // 8-character minimum.
    change_canonical(tx, chat, t, "Drop", |l| format!("Drop change {l}."))
}

fn skip(tx: &Transaction<'_>, chat: &str, t: &Value) -> Result<Canonical, String> {
    let stage = s(t, "stage")?;
    let label = stage_label(stage).ok_or("That is not a stage.")?;
    let run = active(tx, chat)?;
    Ok(Canonical {
        target: json!({"runId": run.id, "attempt": run.attempt, "stage": stage}),
        summary: format!(
            "Skip the {label} stage of task \"{}\"",
            crate::mode::tooltip_reason(&run.title)
        ),
        fixed_form: format!("Skip the {label} stage."),
    })
}

fn allow_more(tx: &Transaction<'_>, chat: &str, t: &Value) -> Result<Canonical, String> {
    let stage = s(t, "stage")?;
    if !matches!(stage, "planReview" | "review" | "smoketest") {
        return Err("allowMore takes planReview, review or smoketest.".into());
    }
    let label = stage_label(stage).unwrap_or(stage);
    let n = t.get("n").and_then(Value::as_u64).unwrap_or(1);
    if !(1..=5).contains(&n) {
        return Err("n must be 1 to 5.".into());
    }
    let run = active(tx, chat)?;
    Ok(Canonical {
        target: json!({"runId": run.id, "attempt": run.attempt, "stage": stage, "n": n}),
        summary: format!(
            "Allow {n} more {label} rounds in task \"{}\"",
            crate::mode::tooltip_reason(&run.title)
        ),
        fixed_form: format!("Allow {n} more {label} rounds."),
    })
}

fn reopen(tx: &Transaction<'_>, chat: &str, t: &Value) -> Result<Canonical, String> {
    let runs = store::open_runs(tx, chat).map_err(|e| e.to_string())?;
    let run = match t.get("runId").and_then(Value::as_str) {
        Some(id) => runs.into_iter().find(|r| r.id == id),
        None => runs
            .into_iter()
            .find(|r| matches!(r.status.as_str(), "parked" | "handed_off")),
    }
    .ok_or("No parked or handed-off task in this chat to reopen.")?;
    Ok(Canonical {
        target: json!({"runId": run.id, "attempt": run.attempt}),
        summary: format!(
            "Reopen task \"{}\"",
            crate::mode::tooltip_reason(&run.title)
        ),
        fixed_form: "Reopen the task.".into(),
    })
}

/// The proof for a consented step: an agent's (the user's words or an Allow grant), or the user's
/// own click.
#[derive(Clone, Debug, Default)]
pub struct Proof {
    pub user_quote: Option<String>,
    pub grant: Option<String>,
    /// The user's own click in the main window ([`crate::user`]). Set only by [`Proof::user`],
    /// which only a main-window op calls; never read from an agent's arguments.
    pub(crate) user: bool,
}

impl Proof {
    pub fn from_args(a: &Value) -> Self {
        let get = |k: &str| a.get(k).and_then(Value::as_str).map(str::to_owned);
        Self {
            user_quote: get("userQuote"),
            grant: get("grant"),
            user: false,
        }
    }
    /// The user acting from the workflow panel: the click is the consent.
    pub fn user() -> Self {
        Self {
            user: true,
            ..Default::default()
        }
    }
    pub fn any(&self) -> bool {
        self.user || self.user_quote.is_some() || self.grant.is_some()
    }
}

/// Spend the proof for this step on the caller's transaction, or record the ask and return the
/// refusal (with the Allow hint) that tells the agent how to get consent. The user's own click
/// needs nothing spent: it is the consent the words and grants stand in for.
#[allow(clippy::too_many_arguments)]
pub fn consent(
    tx: &Transaction<'_>,
    kinds: &AllowKinds,
    chat: &str,
    kind_id: &str,
    target: &Value,
    proof: &Proof,
    why: &str,
    now: i64,
) -> rusqlite::Result<Result<ConsentRecord, String>> {
    if proof.user {
        return Ok(Ok(ConsentRecord {
            id: format!("user:{kind_id}"),
            proof: "user",
            request_id: None,
            submission_id: None,
        }));
    }
    let spent = if proof.grant.is_some() {
        helm_consent::store::spend_grant(tx, kinds, chat, kind_id, target, "app_workflow", now)
    } else if proof.user_quote.is_some() {
        helm_consent::store::spend_words(tx, kinds, chat, kind_id, target, "app_workflow", now)
    } else {
        if let Err(r) = helm_consent::store::record_ask(tx, kinds, chat, kind_id, target, now) {
            return Ok(Err(r.0));
        }
        let form = kinds
            .get(kind_id)
            .and_then(|k| (k.canonical)(tx, chat, target).ok())
            .map(|c| c.fixed_form)
            .unwrap_or_default();
        let body = format!("{why} This needs the user's permission: ask them to send exactly \"{form}\" and pass their words as userQuote.");
        let text = helm_consent::store::with_allow_hint(tx, kinds, chat, kind_id, target, &body)
            .unwrap_or(body);
        return Ok(Err(text));
    };
    Ok(spent.map_err(|r| r.0))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_six_kinds_register_once() {
        let mut k = AllowKinds::new();
        register_allow_kinds(&mut k);
        assert_eq!(
            k.ids(),
            ["land", "ready", "drop", "skip", "allowMore", "reopen"]
        );
    }

    #[test]
    fn pr_labels_use_the_number_only_for_a_pull_url() {
        assert_eq!(pr_label(Some("https://github.com/o/r/pull/12"), "b"), "#12");
        assert_eq!(pr_label(Some("https://github.com/o/r/pull/x"), "b"), "b");
        assert_eq!(pr_label(None, "feat"), "feat");
    }
}
