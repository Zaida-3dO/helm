//! Rules for preference keys owned by crates the settings tools do not depend on: what the
//! generic writer cannot know about the key's value. The relay crate's own rules (the update
//! folder and the household switches) are in `helm_relay::pref_rules`.
//!
//! - `githubPrView` keeps a revision that its own save compares, so a change moves it on.
//! - `defaultBoard` must name an open board, or ask each time.
//! - `globalHotkey` is normalised the way Settings → Shell normalises it; the shell adopts the
//!   stored value as soon as the change is announced.

use helm_hosttools::PrefRule;
use rusqlite::Transaction;
use serde_json::{json, Value};
use std::sync::Arc;

struct GithubPrView;

impl PrefRule for GithubPrView {
    fn key(&self) -> &'static str {
        "githubPrView"
    }

    fn prepare(
        &self,
        tx: &Transaction<'_>,
        _before: Option<&Value>,
        value: &Value,
    ) -> Result<Value, String> {
        let enabled = value
            .get("enabled")
            .and_then(Value::as_bool)
            .ok_or("githubPrView is {enabled: true or false}.")?;
        let revision = helm_github::read_settings(tx).revision;
        Ok(json!({"enabled": enabled, "revision": revision.saturating_add(1)}))
    }
}

struct DefaultBoard;

impl PrefRule for DefaultBoard {
    fn key(&self) -> &'static str {
        "defaultBoard"
    }

    fn prepare(
        &self,
        tx: &Transaction<'_>,
        _before: Option<&Value>,
        value: &Value,
    ) -> Result<Value, String> {
        let id = value
            .as_str()
            .ok_or("defaultBoard is a board's id, or \"ask\".")?;
        if id != helm_projects::boards::ASK_EACH_TIME
            && !helm_projects::boards::is_open_board(tx, id).map_err(|e| e.to_string())?
        {
            return Err(
                "Pick an open board; folders are where chats run, not where they are shown.".into(),
            );
        }
        Ok(value.clone())
    }
}

struct GlobalHotkey;

impl PrefRule for GlobalHotkey {
    fn key(&self) -> &'static str {
        "globalHotkey"
    }

    fn prepare(
        &self,
        _tx: &Transaction<'_>,
        _before: Option<&Value>,
        value: &Value,
    ) -> Result<Value, String> {
        let chord = value
            .as_str()
            .ok_or("globalHotkey is a shortcut or \"\".")?;
        // An empty string is "cleared": a null would delete the row, which reads as the default.
        if chord.is_empty() {
            return Ok(json!(""));
        }
        helm_shell::settings::normalise_chord(chord).map(Value::String)
    }
}

/// The rules of the preference keys the app wires: its own and the relay crate's.
pub fn pref_rules(services: &helm_ipc::Services) -> helm_hosttools::PrefRules {
    let mut rules: Vec<Arc<dyn PrefRule>> = vec![
        Arc::new(GithubPrView),
        Arc::new(DefaultBoard),
        Arc::new(GlobalHotkey),
    ];
    rules.extend(helm_relay::pref_rules::pref_rules(services));
    helm_hosttools::PrefRules(rules)
}
