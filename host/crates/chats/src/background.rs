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
//! - Nothing here can fail a job: a missing field, a chat with no server, or a server that does
//!   not resolve leaves the job untouched, and the job runs on the default it would have used.
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

/// Add the chosen model to one job for one chat. A Claude chat takes the `claude` name; a chat
/// on a local server takes the `local` name resolved against its own server. No match leaves the
/// job as it was.
pub(crate) fn decorate(sh: &Shared, chat_id: &str, job: &mut Value) {
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
        return;
    };
    let (claude, local) = names(pref);
    match (provider, claude, local) {
        (None, Some(m), _) => job["model"] = json!(m),
        (Some(server), _, Some(m)) => {
            let Some(lm) = sh.local.get().cloned() else {
                return;
            };
            match lm.server(&server, &m) {
                Ok(s) => {
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
                Err(why) => log::warn!(
                    "chat {chat_id}: the background job stays on the default model ({why})"
                ),
            }
        }
        _ => {}
    }
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
