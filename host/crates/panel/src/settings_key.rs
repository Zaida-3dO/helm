//! The side panel's own settings as keys of `app_get_settings` and `app_update_settings`.
//!
//! - `panelContext`: whether the open file and selection of the side panel go to this chat as
//!   context (the eye toggle). Each chat has its own.
//! - `vaults`: the Obsidian vault folders the user added and the default vault's name, for note
//!   links: `{folders: [...], default: name or null}`. Obsidian's own vaults are listed by the
//!   panel and not touched here.
//!
//! A chat changes both itself, with the Undo notice.

use crate::{context, vault, viewer};
use helm_hosttools::{JsonKey, SettingsExtension, StagedPart};
use helm_platform::private::PrivateRoots;
use rusqlite::{params, OptionalExtension, Transaction};
use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::Arc;

pub const CONTEXT_KEY: &str = "panelContext";
pub const VAULTS_KEY: &str = "vaults";

const NONE: &[StagedPart] = &[];

fn sql(e: rusqlite::Error) -> String {
    e.to_string()
}

/// The extensions to register: the eye toggle of each chat, and the vault list. `profile_dirs`
/// are the private folders a vault may not be.
pub fn settings_extensions(profile_dirs: Vec<PathBuf>) -> Vec<Arc<dyn SettingsExtension>> {
    let eye = JsonKey::new(
        CONTEXT_KEY,
        "panel_context",
        json!({
            "type": "boolean",
            "default": false,
            "about": "Side panel eye toggle for this chat: whether the file and selection open in the side panel are sent to the chat as context with each message.",
        }),
        || json!(false),
        |tx, chat| {
            let chat = chat.ok_or("The eye toggle belongs to a chat.")?;
            context::is_on(tx, chat).map(Value::Bool).map_err(sql)
        },
        |tx, chat, v| {
            let chat = chat.ok_or("The eye toggle belongs to a chat.")?;
            let on = v.as_bool().ok_or("panelContext is true or false.")?;
            context::set_on(tx, chat, on).map_err(sql)?;
            Ok(Value::Bool(on))
        },
    )
    .per_chat()
    .staged(NONE)
    .extension();

    let roots = PrivateRoots::new(&profile_dirs);
    let vaults = JsonKey::new(
        VAULTS_KEY,
        "panel_vaults",
        json!({
            "type": "object",
            "merge": true,
            "default": {"folders": [], "default": null},
            "about": "Settings > Vaults: folders added as Obsidian vaults for note links (folders: a list of absolute folder paths) and the default vault's name (default: a vault's name, null for the first). Obsidian's own vaults are listed by the panel and not changed here.",
        }),
        || json!({"folders": [], "default": null}),
        |tx, _| {
            let (folders, default) = vault::stored(tx).map_err(sql)?;
            Ok(json!({
                "folders": folders.iter().map(|p| p.to_string_lossy()).collect::<Vec<_>>(),
                "default": default,
            }))
        },
        move |tx, _, v| {
            let o = v.as_object().ok_or("vaults is {folders, default}.")?;
            let mut paths: Vec<String> = Vec::new();
            for f in o
                .get("folders")
                .and_then(Value::as_array)
                .ok_or("vaults.folders is a list of folder paths.")?
            {
                let raw = f.as_str().ok_or("vaults.folders is a list of folder paths.")?;
                let path = viewer::admit(raw.trim(), &roots).map_err(|e| format!("{raw}: {e:?}"))?;
                if !path.is_dir() {
                    return Err(format!("{raw} is not an existing folder."));
                }
                paths.push(path.to_string_lossy().into_owned());
            }
            let default = match o.get("default") {
                None | Some(Value::Null) => None,
                Some(Value::String(s)) if !s.trim().is_empty() => Some(s.trim().to_owned()),
                Some(_) => return Err("vaults.default is a vault's name, or null.".into()),
            };
            replace_folders(tx, &paths)?;
            match &default {
                Some(name) => tx
                    .execute(
                        "INSERT INTO panel_vault_default (id, name) VALUES (1, ?1)
                         ON CONFLICT(id) DO UPDATE SET name = excluded.name",
                        [name],
                    )
                    .map(|_| ())
                    .map_err(sql)?,
                None => tx
                    .execute("DELETE FROM panel_vault_default WHERE id = 1", [])
                    .map(|_| ())
                    .map_err(sql)?,
            }
            let (folders, default) = vault::stored(tx).map_err(sql)?;
            Ok(json!({
                "folders": folders.iter().map(|p| p.to_string_lossy()).collect::<Vec<_>>(),
                "default": default,
            }))
        },
    )
    .staged(NONE)
    .extension();
    vec![eye, vaults]
}

/// Make the user's folders exactly `paths`, keeping the time each older one was added.
fn replace_folders(tx: &Transaction<'_>, paths: &[String]) -> Result<(), String> {
    let now = helm_core::now_ms();
    let have: Vec<String> = {
        let mut st = tx.prepare("SELECT path FROM panel_vaults").map_err(sql)?;
        let rows = st
            .query_map([], |r| r.get::<_, String>(0))
            .map_err(sql)?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(sql)?;
        rows
    };
    for old in &have {
        if !paths.iter().any(|p| p.eq_ignore_ascii_case(old)) {
            tx.execute("DELETE FROM panel_vaults WHERE path = ?1", [old])
                .map_err(sql)?;
        }
    }
    for p in paths {
        let known: Option<i64> = tx
            .query_row(
                "SELECT 1 FROM panel_vaults WHERE lower(path) = lower(?1)",
                [p],
                |r| r.get(0),
            )
            .optional()
            .map_err(sql)?;
        if known.is_none() {
            tx.execute(
                "INSERT INTO panel_vaults (path, added_at) VALUES (?1, ?2)",
                params![p, now],
            )
            .map_err(sql)?;
        }
    }
    Ok(())
}
