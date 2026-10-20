//! Settings → Orchestrator for chats: the `orchestrator` key of `app_get_settings` and
//! `app_update_settings`.
//!
//! A chat reads every field. It may change only two kinds of field: `newChats` and the agent a
//! stage uses (`stages.<stage>.agent`). Everything else decides who may act or what may land
//! without the user (the master switch, Move fast, who signs off a plan, whether the orchestrator
//! edits, the landing rules, each stage's use and round cap, which agents may start building), so
//! it stays the user's: an update that would change any of it is refused whole, with the Settings
//! page named. An update may repeat a user-only field at its current value.
//!
//! An update names only the fields to change. Objects merge field by field; a list or a value
//! replaces the old one. The merged result goes through the same strict validation as the
//! Settings page, so an unknown or invalid field refuses the whole update. `null` resets the
//! fields a chat may change to their defaults and leaves the rest alone.

use crate::settings;
use crate::types::WorkflowSettings;
use helm_hosttools::SettingsExtension;
use helm_ipc::AppEvent;
use rusqlite::Transaction;
use serde_json::{json, Value};
use std::collections::BTreeSet;
use std::sync::Arc;

pub const KEY: &str = "orchestrator";
const PAGE: &str = "Settings > Orchestrator";
const STAGES: [&str; 6] = [
    "plan",
    "planReview",
    "build",
    "review",
    "cleanup",
    "smoketest",
];

pub struct OrchestratorSettings;

/// The extension to register as a [`helm_hosttools::SettingsExtensions`] entry.
pub fn extension() -> Arc<dyn SettingsExtension> {
    Arc::new(OrchestratorSettings)
}

fn as_json(s: &WorkflowSettings) -> Value {
    serde_json::to_value(s).expect("settings serialise")
}

/// `v` without the fields a chat may change: what is left must not differ across an update.
fn locked(v: &Value) -> Value {
    let mut v = v.clone();
    if let Some(o) = v.as_object_mut() {
        o.remove("newChats");
        if let Some(stages) = o.get_mut("stages").and_then(Value::as_object_mut) {
            for s in STAGES {
                if let Some(st) = stages.get_mut(s).and_then(Value::as_object_mut) {
                    st.remove("agent");
                }
            }
        }
    }
    v
}

/// The path of the first difference between `a` and `b`, if any.
fn first_diff(a: &Value, b: &Value, path: &str) -> Option<String> {
    match (a, b) {
        (Value::Object(x), Value::Object(y)) => {
            let keys: BTreeSet<&String> = x.keys().chain(y.keys()).collect();
            keys.into_iter().find_map(|k| {
                let p = if path.is_empty() {
                    k.clone()
                } else {
                    format!("{path}.{k}")
                };
                match (x.get(k), y.get(k)) {
                    (Some(l), Some(r)) => first_diff(l, r, &p),
                    _ => Some(p),
                }
            })
        }
        _ if a == b => None,
        _ => Some(path.to_owned()),
    }
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

/// The current value with the fields a chat may change set back to their defaults.
fn reset_changeable(current: &Value) -> Value {
    let defaults = as_json(&WorkflowSettings::default());
    let mut out = current.clone();
    if let (Some(o), Some(d)) = (out.as_object_mut(), defaults.as_object()) {
        if let Some(n) = d.get("newChats") {
            o.insert("newChats".into(), n.clone());
        }
        if let (Some(stages), Some(dstages)) = (
            o.get_mut("stages").and_then(Value::as_object_mut),
            d.get("stages").and_then(Value::as_object),
        ) {
            for s in STAGES {
                let agent = dstages.get(s).and_then(|v| v.get("agent")).cloned();
                if let (Some(st), Some(agent)) =
                    (stages.get_mut(s).and_then(Value::as_object_mut), agent)
                {
                    st.insert("agent".into(), agent);
                }
            }
        }
    }
    out
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
    let mut o = json!({
        "type": "object",
        "about": "Settings > Orchestrator: the staged workflow (plan, review, build, review, clean up, smoke test) and how its work lands. Read all of it. You may change only newChats and each stage's agent; everything else is the user's and an update that touches it is refused. Send only the fields to change: objects merge field by field, lists replace. null resets newChats and the stage agents to their defaults.",
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
                    "repos": {"type": "list", "items": {"repo": "host/owner/name", "mode": word(&["draft", "merge"]), "method": "merge, squash or rebase; only on a merge rule"}, "maxItems": settings::MAX_REPOS},
                    "draftLabels": {"type": "list", "items": {"type": "string"}, "maxItems": settings::MAX_LABELS},
                    "flipLabel": {"type": "string or null"},
                    "protectedBranches": {"type": "list", "items": {"type": "string"}, "maxItems": settings::MAX_PROTECTED},
                },
            },
            "moveFast": {"type": "boolean", "about": "Move fast: tests only what a change touches. Only the user can turn it on or off."},
            "newChats": {"type": "enum", "allowed": ["auto", "always", "never"], "about": "Whether new chats start with Orchestrator on."},
        },
    });
    let mut changeable = vec!["newChats".to_owned()];
    changeable.extend(STAGES.iter().map(|s| format!("stages.{s}.agent")));
    o["chatsMayChange"] = json!(changeable);
    o["userOnly"] = json!(format!(
        "Every other field. Readable here, changed only by the user in {PAGE}."
    ));
    o
}

impl SettingsExtension for OrchestratorSettings {
    fn keys(&self) -> Vec<&'static str> {
        vec![KEY]
    }

    fn describe(&self, key: &str) -> Option<Value> {
        (key == KEY).then(options)
    }

    fn needs_all_settings(&self, _key: &str) -> bool {
        true
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
            reset_changeable(&before)
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
        if let Some(path) = first_diff(&locked(&before), &locked(&after), "") {
            return Err(format!(
                "{KEY}.{path} is yours to change only: ask the user to change it in {PAGE}. You may change newChats and each stage's agent."
            ));
        }
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

    fn refused(db: &helm_core::db::Database, v: Value, field: &str) {
        let before = load(db);
        let why = apply(db, v).unwrap_err();
        assert!(
            why.contains(&format!("{KEY}.{field} is yours to change only")),
            "{why}"
        );
        assert!(why.contains(PAGE), "{why}");
        assert_eq!(load(db), before, "a refused update stores nothing");
    }

    #[test]
    fn a_chat_reads_everything_with_its_allowed_values() {
        let (_d, db) = db(&[]);
        let v = db
            .snapshot(|tx| Ok::<_, rusqlite::Error>(OrchestratorSettings.read(tx, KEY)))
            .unwrap();
        assert_eq!(v["moveFast"], false);
        assert_eq!(v["newChats"], "auto");
        assert_eq!(v["stages"]["review"]["maxRounds"], 3);
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
        assert!(o["chatsMayChange"]
            .as_array()
            .unwrap()
            .contains(&json!("stages.review.agent")));
        assert!(OrchestratorSettings.describe("theme").is_none());
    }

    #[test]
    fn the_key_is_in_the_all_tier() {
        assert!(OrchestratorSettings.needs_all_settings(KEY));
    }

    #[test]
    fn a_chat_may_change_new_chats_and_stage_agents_and_the_rest_merges() {
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
        assert_eq!(s.stages.review.max_rounds, 3);
        assert_eq!(s.stages.plan.agent, "planner");
        assert_eq!(s.landing.protected_branches, ["main", "master"]);
    }

    #[test]
    fn user_only_fields_are_refused_and_nothing_is_stored() {
        let (_d, db) = db(&[]);
        refused(&db, json!({"moveFast": true}), "moveFast");
        refused(&db, json!({"enabled": true}), "enabled");
        refused(&db, json!({"planSignOff": "me"}), "planSignOff");
        refused(
            &db,
            json!({"orchestratorEdits": "allow"}),
            "orchestratorEdits",
        );
        refused(
            &db,
            json!({"landing": {"default": "merge"}}),
            "landing.default",
        );
        refused(
            &db,
            json!({"landing": {"protectedBranches": []}}),
            "landing.protectedBranches",
        );
        refused(&db, json!({"homeRepos": ["github.com/o/r"]}), "homeRepos");
        refused(
            &db,
            json!({"stages": {"review": {"maxRounds": 9}}}),
            "stages.review.maxRounds",
        );
        refused(
            &db,
            json!({"stages": {"review": {"use": "off"}}}),
            "stages.review.use",
        );
        refused(
            &db,
            json!({"stages": {"build": {"buildExempt": ["x"]}}}),
            "stages.build.buildExempt",
        );
        // A good field next to a user-only one refuses the whole update.
        refused(
            &db,
            json!({"newChats": "never", "moveFast": true}),
            "moveFast",
        );
        assert_eq!(load(&db).new_chats, NewChats::Auto);
    }

    #[test]
    fn repeating_a_user_only_field_at_its_current_value_is_fine() {
        let (_d, db) = db(&[]);
        let all = apply(&db, json!({})).unwrap().1;
        let mut echo = all.clone();
        echo["newChats"] = json!("always");
        let (_, after) = apply(&db, echo).unwrap();
        assert_eq!(after["newChats"], "always");
        assert_eq!(after["moveFast"], false);
    }

    #[test]
    fn a_user_who_turned_move_fast_on_keeps_it_across_a_chat_update() {
        let (_d, db) = db(&[]);
        db.write(|tx| {
            let mut s = WorkflowSettings::default();
            s.enabled = true;
            s.move_fast = true;
            settings::store(tx, &s, 1)
        })
        .unwrap();
        apply(&db, json!({"newChats": "always"})).unwrap();
        let s = load(&db);
        assert!(s.enabled && s.move_fast);
        assert_eq!(s.new_chats, NewChats::Always);
        // Turning it off is still the user's.
        refused(&db, json!({"moveFast": false}), "moveFast");
        assert!(load(&db).move_fast);
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
    fn null_resets_what_a_chat_may_change_and_nothing_else() {
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
        let s = load(&db);
        assert_eq!(s.new_chats, NewChats::Auto);
        assert_eq!(s.stages.review.agent, "reviewer");
        assert!(s.move_fast, "the user's Move fast is not reset");
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
