//! Undo for what a chat changes about a project beyond its name: where it sits (its parent and
//! its place among its siblings), its folder, its picture, and what its chats are told first
//! (startup instructions, context files, MCP servers).
//!
//! The tools read a snapshot of the project before and after a change (the `project_snapshot`
//! op) and file the pair in the change ring under [`KEY`], so the user sees the usual notice with
//! Undo. The value is `{projectId: snapshot}`; Undo puts each snapshot back, and is refused when
//! a project is no longer as the chat left it, like every other change in the ring.

use crate::store::{self, Refusal, Res};
use crate::types::ProjectUpdateArgs;
use base64::Engine;
use helm_hosttools::{SettingsExtension, StagedPart};
use helm_ipc::AppEvent;
use rusqlite::Transaction;
use serde_json::{json, Map, Value};
use std::sync::Arc;

/// The change-ring key these edits are filed under.
pub const KEY: &str = "projectFields";

const NONE: &[StagedPart] = &[];

fn why(r: Refusal) -> String {
    match r {
        Refusal::BadArgs(m) | Refusal::NotFound(m) | Refusal::Conflict(m) | Refusal::Refused(m) => {
            m
        }
        Refusal::Sql(e) => e.to_string(),
    }
}

/// One project as these edits see it.
pub fn snapshot(tx: &Transaction<'_>, id: &str) -> Res<Value> {
    let node = store::get_project(tx, id)?;
    let (siblings, _) = store::project_siblings(tx, id, "")?;
    let next = siblings
        .iter()
        .position(|s| s == id)
        .and_then(|i| siblings.get(i + 1).cloned());
    let image =
        store::icon_image(tx, id)?.map(|png| base64::engine::general_purpose::STANDARD.encode(png));
    Ok(json!({
        "parentId": node.parent_id,
        "path": node.path,
        "icon": node.icon,
        "iconImage": image,
        "startupInstructions": node.startup_instructions,
        "startupFiles": node.startup_files,
        "startupServers": node.startup_servers,
        "nextSibling": next,
    }))
}

/// Put one project back as `snap` says.
fn put_back(tx: &Transaction<'_>, id: &str, snap: &Value) -> Res<()> {
    let node = store::get_project(tx, id)?;
    let text = |k: &str| snap[k].as_str().map(str::to_owned);
    let list = |k: &str| -> Vec<String> {
        snap[k]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default()
    };
    let args = ProjectUpdateArgs {
        project_id: id.to_owned(),
        revision: node.revision,
        parent_id: Some(text("parentId")),
        before_project_id: Some(text("nextSibling")),
        path: node.path.as_ref().map(|_| text("path")),
        startup_instructions: Some(text("startupInstructions")),
        startup_files: Some(list("startupFiles")),
        startup_servers: Some(list("startupServers")),
        icon: Some(text("icon")).filter(|i| i.as_deref() != Some("img")),
        ..ProjectUpdateArgs::default()
    };
    store::update_project(tx, &args)?;
    if snap["icon"].as_str() == Some("img") {
        if let Some(b64) = snap["iconImage"].as_str() {
            let png = store::validate_icon_image(b64)?;
            store::set_icon_image(tx, id, &png)?;
        }
    }
    Ok(())
}

struct ProjectFields;

/// The extension to register as a [`helm_hosttools::SettingsExtensions`] entry: it only carries
/// Undo, and is not listed among the settings.
pub fn settings_extension() -> Arc<dyn SettingsExtension> {
    Arc::new(ProjectFields)
}

impl SettingsExtension for ProjectFields {
    fn staged_parts(&self, _key: &str) -> &'static [StagedPart] {
        NONE
    }

    fn keys(&self) -> Vec<&'static str> {
        vec![KEY]
    }

    fn listed(&self, _key: &str) -> bool {
        false
    }

    fn describe(&self, key: &str) -> Option<Value> {
        (key == KEY).then(|| {
            json!({"type": "object", "about": "Carries Undo for a project's place, folder, picture, startup instructions, files and servers, which app_update_project changes."})
        })
    }

    fn read(&self, _tx: &Transaction<'_>, _key: &str) -> Value {
        json!({})
    }

    fn apply(
        &self,
        _tx: &Transaction<'_>,
        _key: &str,
        _value: &Value,
    ) -> Result<(Value, Value), String> {
        Err("A project is changed with app_update_project, not here.".into())
    }

    fn unchanged_since(&self, tx: &Transaction<'_>, _key: &str, after: &Value) -> Option<bool> {
        let map = after.as_object()?;
        Some(
            map.iter()
                .all(|(id, snap)| snapshot(tx, id).is_ok_and(|now| &now == snap)),
        )
    }

    fn restore(&self, tx: &Transaction<'_>, _key: &str, value: &Value) -> Result<(), String> {
        let empty = Map::new();
        for (id, snap) in value.as_object().unwrap_or(&empty) {
            put_back(tx, id, snap).map_err(why)?;
        }
        Ok(())
    }

    fn changed_event(&self, _key: &str) -> AppEvent {
        AppEvent::new("sidebar").with("chatIds", json!([]))
    }
}
