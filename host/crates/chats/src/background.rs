//! The model a background job asks: chat titles, composer autocomplete, and the idle check.
//!
//! - The `backgroundModel` preference is `{claude, local}`, both null by default: all three jobs
//!   ask the helper's own small default through the user's Claude sign-in, as they always have.
//!   Nothing changes until a name is put here.
//! - `claude` names a model for jobs on Claude chats.
//! - `local` names a model on the chat's own local model server, for jobs on chats that run on
//!   one. The job carries the server's loopback address, and the server's key is handed to the
//!   helper through its environment (`ProcessSpawner::run_helper` moves it there out of the job),
//!   exactly as a worker launch does; the helper builds the child's environment with the worker's
//!   own loopback-only rule.
//! - A missing field or a chat with no name chosen leaves the job untouched, and the job runs on
//!   the default it would have used. A name that is chosen but cannot be used (a server that does
//!   not resolve) is reported to the caller: [`decorate_checked`] answers the reason, and a job
//!   about a local chat's own words (autocomplete) does not run rather than fall back to Claude.
//!   [`decorate`] logs it and carries on, for jobs that have a harmless default.
//! - The job does not run a turn's model preparation: a configured local variant that is not
//!   loaded may load slowly or be refused, and the job's own timeout covers that (a title still
//!   has its fallback). Choosing a variant that stays loaded is the person's decision.

use crate::runtime::Shared;
use rusqlite::OptionalExtension;
use serde_json::{json, Value};

pub(crate) const PREF_KEY: &str = "backgroundModel";

/// A name is at most this long; longer or blank counts as unset.
const MAX_NAME: usize = 200;

fn name(v: Option<&Value>) -> Option<String> {
    v.and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty() && s.len() <= MAX_NAME)
        .map(str::to_owned)
}

/// The preference's two names, either absent, null, or not a string when unset.
fn names(pref: Option<Value>) -> (Option<String>, Option<String>) {
    let Some(m) = pref.and_then(|v| v.as_object().cloned()) else {
        return (None, None);
    };
    (name(m.get("claude")), name(m.get("local")))
}

/// Whether a model is chosen for jobs on local chats.
pub(crate) fn local_name_chosen(tx: &rusqlite::Transaction<'_>) -> bool {
    names(helm_core::prefs::read(tx, PREF_KEY)).1.is_some()
}

/// [`decorate_checked`] for a job that runs on the default model when the chosen one cannot be
/// used: the reason is logged and the job stays as it was.
pub(crate) fn decorate(sh: &Shared, chat_id: &str, job: &mut Value) {
    if let Err(why) = decorate_checked(sh, chat_id, job) {
        log::warn!("chat {chat_id}: the background job stays on the default model ({why})");
    }
}

/// Add the chosen model to one job for one chat. A Claude chat takes the `claude` name; a chat
/// on a local server takes the `local` name resolved against its own server. No name chosen leaves
/// the job as it was. A name that is chosen but cannot be used (no model support, a server that
/// does not resolve) is an error, so a job that must not fall back to Claude (a local chat's draft)
/// can refuse to run.
pub(crate) fn decorate_checked(sh: &Shared, chat_id: &str, job: &mut Value) -> Result<(), String> {
    let read = sh.db.snapshot(|tx| {
        let pref = helm_core::prefs::read(tx, PREF_KEY);
        let provider: Option<String> = tx
            .query_row("SELECT provider FROM chats WHERE id = ?1", [chat_id], |r| {
                r.get::<_, Option<String>>(0)
            })
            .optional()?
            .flatten()
            .filter(|s| !s.is_empty());
        Ok::<_, rusqlite::Error>((pref, provider))
    });
    let Ok((pref, provider)) = read else {
        return Err("the chat could not be read".into());
    };
    let (claude, local) = names(pref);
    match (provider, claude, local) {
        (None, Some(m), _) => job["model"] = json!(m),
        (Some(server), _, Some(m)) => {
            let Some(lm) = sh.local.get().cloned() else {
                return Err("local models are not available".into());
            };
            let s = lm.server(&server, &m)?;
            job["model"] = json!(m);
            job["localBaseUrl"] = json!(s.base_url);
            job["localKey"] = json!(s.key);
            if let Some(n) = s.context_tokens {
                job["localContextTokens"] = json!(n);
            }
            if let Some(n) = s.max_output_tokens {
                job["localMaxOutputTokens"] = json!(n);
            }
        }
        _ => {}
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn the_preference_gives_two_names_and_shrugs_at_the_rest() {
        for (pref, want) in [
            (None, (None, None)),
            (Some(json!(null)), (None, None)),
            (Some(json!("haiku")), (None, None)),
            (Some(json!({})), (None, None)),
            (
                Some(json!({"claude": "sonnet", "local": "qwen-helm-4k"})),
                (Some("sonnet".to_owned()), Some("qwen-helm-4k".to_owned())),
            ),
            (
                // Null, blank, whitespace, oversize and wrong types all mean unset.
                Some(json!({"claude": null, "local": "   "})),
                (None, None),
            ),
            (
                Some(json!({"claude": 5, "local": "x".repeat(MAX_NAME + 1)})),
                (None, None),
            ),
            (
                Some(json!({"claude": " haiku \n"})),
                (Some("haiku".to_owned()), None),
            ),
        ] {
            assert_eq!(names(pref), want, "{want:?}");
        }
    }
}
