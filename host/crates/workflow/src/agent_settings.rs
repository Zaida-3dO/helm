//! Settings → Orchestrator for chats: the `orchestrator` key of `app_get_settings` and
//! `app_update_settings`.
//!
//! A chat reads every field and changes any of them itself, with the Undo notice; there is no
//! part of the key it must leave alone.
//!
//! An update names only the fields to change. Objects merge field by field; a list or a value
//! replaces the old one. The merged result goes through the same strict validation as the
//! Settings page, so an unknown or invalid field refuses the whole update. `null` restores every
//! field to its default.

use crate::settings;
use crate::types::WorkflowSettings;
use helm_hosttools::{SettingsExtension, StagedPart};
use helm_ipc::AppEvent;
use rusqlite::Transaction;
use serde_json::{json, Value};
use std::sync::Arc;

pub const KEY: &str = "orchestrator";

pub struct OrchestratorSettings;

/// The extension to register as a [`helm_hosttools::SettingsExtensions`] entry.
pub fn extension() -> Arc<dyn SettingsExtension> {
    Arc::new(OrchestratorSettings)
}

fn as_json(s: &WorkflowSettings) -> Value {
    serde_json::to_value(s).expect("settings serialise")
}

/// Merge `patch` into `base`: objects field by field, anything else replaces.
fn merge(base: &mut Value, patch: &Value) {
    match (base, patch) {
        (Value::Object(b), Value::Object(p)) => {
            for (k, v) in p {
                match b.get_mut(k) {
                    Some(slot) => merge(slot, v),
                    None => {
                        b.insert(k.clone(), v.clone());
                    }
                }
            }
        }
        (slot, v) => *slot = v.clone(),
    }
}

fn sql(e: rusqlite::Error) -> String {
    format!("The Orchestrator settings could not be read or saved: {e}")
}

fn word(allowed: &[&str]) -> Value {
    json!({"type": "enum", "allowed": allowed})
}

fn agent_name() -> Value {
    json!({
        "type": "string",
        "rule": "a-z, 0-9 and -, at most 64 characters, starting with a letter or digit, without --",
    })
}

fn rounds() -> Value {
    json!({"type": "integer", "min": 1, "max": settings::MAX_ROUNDS})
}

fn options() -> Value {
    let uses = word(&["required", "auto", "off"]);
    json!({
        "type": "object",
        "about": "Settings > Orchestrator: the staged workflow (plan, review, build, review, clean up, smoke test) and how its work lands. Read all of it and change any field. Send only the fields to change: objects merge field by field, lists replace. null restores every field to its default.",
        "default": as_json(&WorkflowSettings::default()),
        "merge": true,
        "fields": {
            "enabled": {"type": "boolean", "about": "The master switch."},
            "stages": {
                "type": "object",
                "about": "Each stage must have its own agent.",
                "fields": {
                    "plan": {"use": uses, "agent": agent_name()},
                    "planReview": {"use": uses, "agent": agent_name(), "maxRounds": rounds()},
                    "build": {
                        "agent": agent_name(),
                        "startAgents": {"type": "list", "items": agent_name(), "maxItems": settings::MAX_AGENT_LIST},
                        "buildExempt": {"type": "list", "items": agent_name(), "maxItems": settings::MAX_AGENT_LIST},
                        "note": "Build is always required.",
                    },
                    "review": {"use": word(&["required", "off"]), "agent": agent_name(), "maxRounds": rounds()},
                    "cleanup": {"use": uses, "agent": agent_name()},
                    "smoketest": {"use": uses, "agent": agent_name(), "maxRounds": rounds()},
                },
            },
            "orchestratorEdits": word(&["deny", "allow"]),
            "planSignOff": word(&["review", "me"]),
            "homeRepos": {"type": "list", "items": {"type": "string", "rule": "host/owner/name"}, "maxItems": settings::MAX_REPOS},
            "landing": {
                "type": "object",
                "fields": {
                    "default": word(&["draft", "merge"]),
                    "method": word(&["merge", "squash", "rebase"]),
                    "repos": {"type": "list", "items": {"repo": "host/owner/name", "mode": word(&["draft", "merge"]), "method": "merge, squash, rebase or fast_forward; only on a merge rule"}, "maxItems": settings::MAX_REPOS},
                    "draftLabels": {"type": "list", "items": {"type": "string"}, "maxItems": settings::MAX_LABELS},
                    "flipLabel": {"type": "string or null"},
                    "protectedBranches": {"type": "list", "items": {"type": "string"}, "maxItems": settings::MAX_PROTECTED},
                },
            },
            "moveFast": {"type": "boolean", "about": "Move fast: tests only what a change touches. Changing it is noted for the user, with Undo."},
            "landingGuard": {"type": "boolean", "about": "The landing guard: refuses pushes, merges and other landings in chats with a task. Changing it is noted for the user, with Undo."},
            "dispatchGate": {"type": "boolean", "about": "The dispatch gate: during a task, agent spawns face their stage's gate. Changing it is noted for the user, with Undo."},
            "newChats": {"type": "enum", "allowed": ["auto", "always", "never"], "about": "Whether new chats start with Orchestrator on."},
        },
    })
}

impl SettingsExtension for OrchestratorSettings {
    fn keys(&self) -> Vec<&'static str> {
        vec![KEY]
    }

    fn describe(&self, key: &str) -> Option<Value> {
        (key == KEY).then(options)
    }

    fn staged_parts(&self, _key: &str) -> &'static [StagedPart] {
        &[]
    }

    fn read(&self, tx: &Transaction<'_>, _key: &str) -> Value {
        settings::load(tx)
            .map(|s| as_json(&s))
            .unwrap_or_else(|_| as_json(&WorkflowSettings::default()))
    }

    fn apply(
        &self,
        tx: &Transaction<'_>,
        _key: &str,
        value: &Value,
    ) -> Result<(Value, Value), String> {
        let before = as_json(&settings::load(tx).map_err(sql)?);
        let wanted = if value.is_null() {
            as_json(&WorkflowSettings::default())
        } else if value.is_object() {
            let mut m = before.clone();
            merge(&mut m, value);
            m
        } else {
            return Err(format!(
                "{KEY} must be an object with only the fields to change (see options.{KEY}), or null."
            ));
        };
        let next = settings::validate(wanted).map_err(|e| format!("{KEY}.{e}"))?;
        let after = as_json(&next);
        settings::store(tx, &next, helm_core::now_ms()).map_err(sql)?;
        Ok((before, after))
    }

    fn restore(&self, tx: &Transaction<'_>, _key: &str, value: &Value) -> Result<(), String> {
        let s = settings::validate(value.clone()).map_err(|e| format!("{KEY}.{e}"))?;
        settings::store(tx, &s, helm_core::now_ms()).map_err(sql)
    }

    fn changed_event(&self, _key: &str) -> AppEvent {
        AppEvent::new("workflow_settings")
    }
}

#[cfg(test)]
#[allow(clippy::field_reassign_with_default)]
mod tests {
    use super::*;
    use crate::mode::tests::db;
    use crate::types::{NewChats, WorkflowSettings};

    fn apply(db: &helm_core::db::Database, v: Value) -> Result<(Value, Value), String> {
        db.write(|tx| Ok::<_, rusqlite::Error>(OrchestratorSettings.apply(tx, KEY, &v)))
            .unwrap()
    }

    fn load(db: &helm_core::db::Database) -> WorkflowSettings {
        db.snapshot(settings::load).unwrap()
    }

    #[test]
    fn a_chat_reads_everything_with_its_allowed_values() {
        let (_d, db) = db(&[]);
        let v = db
            .snapshot(|tx| Ok::<_, rusqlite::Error>(OrchestratorSettings.read(tx, KEY)))
            .unwrap();
        assert_eq!(v["moveFast"], false);
        assert_eq!(v["newChats"], "auto");
        assert_eq!(v["stages"]["review"]["maxRounds"], 2);
        let o = OrchestratorSettings.describe(KEY).unwrap();
        assert_eq!(
            o["fields"]["newChats"]["allowed"],
            json!(["auto", "always", "never"])
        );
        assert_eq!(
            o["fields"]["planSignOff"]["allowed"],
            json!(["review", "me"])
        );
        assert_eq!(
            o["fields"]["stages"]["fields"]["review"]["use"]["allowed"],
            json!(["required", "off"])
        );
        assert_eq!(o["default"], v);
        assert!(OrchestratorSettings.describe("theme").is_none());
    }

    #[test]
    fn no_part_of_the_key_is_held_back() {
        assert!(OrchestratorSettings.staged_parts(KEY).is_empty());
    }

    #[test]
    fn a_chat_changes_any_field_and_the_rest_merges() {
        let (_d, db) = db(&[]);
        let (before, after) = apply(
            &db,
            json!({"newChats": "never", "stages": {"review": {"agent": "my-reviewer"}}}),
        )
        .unwrap();
        assert_eq!(before["newChats"], "auto");
        assert_eq!(after["newChats"], "never");
        let s = load(&db);
        assert_eq!(s.new_chats, NewChats::Never);
        assert_eq!(s.stages.review.agent, "my-reviewer");
        // Fields the update left out kept their values.
        assert_eq!(s.stages.review.max_rounds, 2);
        assert_eq!(s.stages.plan.agent, "planner");
        assert_eq!(s.landing.protected_branches, ["main", "master"]);
    }

    #[test]
    fn every_field_is_changed_and_a_bad_one_stores_nothing() {
        let (_d, db) = db(&[]);
        for (patch, check) in [
            (json!({"moveFast": true}), "moveFast"),
            (json!({"enabled": true}), "enabled"),
            (json!({"planSignOff": "me"}), "planSignOff"),
            (json!({"orchestratorEdits": "allow"}), "orchestratorEdits"),
            (json!({"landing": {"default": "merge"}}), "landing.default"),
            (
                json!({"landing": {"protectedBranches": []}}),
                "landing.protectedBranches",
            ),
            (json!({"homeRepos": ["github.com/o/r"]}), "homeRepos"),
            (
                json!({"stages": {"review": {"maxRounds": 9}}}),
                "stages.review.maxRounds",
            ),
            (
                json!({"stages": {"review": {"use": "off"}}}),
                "stages.review.use",
            ),
            (
                json!({"stages": {"build": {"buildExempt": ["x"]}}}),
                "stages.build.buildExempt",
            ),
        ] {
            let (before, after) = apply(&db, patch).unwrap();
            assert_ne!(before, after, "{check}");
        }
        let s = load(&db);
        assert!(s.move_fast && s.enabled);
        // A bad field next to a good one stores neither.
        let kept = load(&db);
        assert!(apply(&db, json!({"newChats": "never", "bogus": 1})).is_err());
        assert_eq!(load(&db), kept);
    }

    #[test]
    fn strict_validation_names_the_field() {
        let (_d, db) = db(&[]);
        let e = apply(&db, json!({"newChats": "sometimes"})).unwrap_err();
        assert!(e.contains("newChats must be always, never or auto"), "{e}");
        let e = apply(&db, json!({"bogus": 1})).unwrap_err();
        assert!(e.contains("bogus: unknown setting"), "{e}");
        let e = apply(&db, json!({"stages": {"review": {"agent": "coder"}}})).unwrap_err();
        assert!(e.contains("each stage needs its own"), "{e}");
        let e = apply(&db, json!("auto")).unwrap_err();
        assert!(e.contains("must be an object"), "{e}");
    }

    #[test]
    fn null_restores_every_field_to_its_default() {
        let (_d, db) = db(&[]);
        db.write(|tx| {
            let mut s = WorkflowSettings::default();
            s.move_fast = true;
            s.new_chats = NewChats::Never;
            s.stages.review.agent = "other".into();
            settings::store(tx, &s, 1)
        })
        .unwrap();
        apply(&db, Value::Null).unwrap();
        assert_eq!(load(&db), WorkflowSettings::default());
    }

    #[test]
    fn restore_puts_the_old_value_back() {
        let (_d, db) = db(&[]);
        let (before, _) = apply(&db, json!({"newChats": "never"})).unwrap();
        db.write(|tx| Ok::<_, rusqlite::Error>(OrchestratorSettings.restore(tx, KEY, &before)))
            .unwrap()
            .unwrap();
        assert_eq!(load(&db).new_chats, NewChats::Auto);
        assert_eq!(
            OrchestratorSettings.changed_event(KEY).kind,
            "workflow_settings"
        );
    }
}
