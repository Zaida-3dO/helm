//! Two project settings as keys of `app_get_settings` and `app_update_settings`.
//!
//! - `worktrees`: where agents' worktrees go and which repositories allow them,
//!   `{root, repos: {repoId: true or false}}`. A chat changes both itself, with the Undo notice.
//! - `boardAddress`: the Agent Standup address, as text (empty for none). Helm sends the board's
//!   work records to it, so a chat stages a change and the user approves. The board's token is a
//!   secret: a chat stages `credentials` for the Projects page instead.

use crate::board::{credentials, resolve, stored_url};
use crate::repos;
use crate::store::Refusal;
use helm_core::db::Database;
use helm_core::profile::Profile;
use helm_hosttools::{JsonKey, SettingsExtension, StagedPart};
use helm_ipc::Services;
use helm_platform::Platform;
use helm_standup::BoardLink;
use helm_worktrees::offlist;
use rusqlite::Transaction;
use serde_json::{json, Map, Value};
use std::sync::Arc;

pub const WORKTREES_KEY: &str = "worktrees";
pub const BOARD_KEY: &str = "boardAddress";

const NONE: &[StagedPart] = &[];

fn why(r: Refusal) -> String {
    match r {
        Refusal::BadArgs(m) | Refusal::NotFound(m) | Refusal::Conflict(m) | Refusal::Refused(m) => {
            m
        }
        Refusal::Sql(e) => e.to_string(),
    }
}

fn worktrees_value(tx: &Transaction<'_>) -> Result<Value, String> {
    let mut map = Map::new();
    for e in repos::all(tx).map_err(why)? {
        map.insert(e.repo_id.clone(), json!(e.worktrees_allowed));
    }
    let root = repos::root(tx).map_err(|e| e.to_string())?;
    Ok(json!({"root": root.to_string_lossy(), "repos": map}))
}

/// The extensions to register.
pub fn settings_extensions(services: &Services) -> Vec<Arc<dyn SettingsExtension>> {
    let (Some(db), Some(profile), Some(platform)) = (
        services.get::<Database>(),
        services.get::<Profile>(),
        services.get::<Platform>(),
    ) else {
        return Vec::new();
    };
    let default_root = || {
        helm_worktrees::rules::default_root()
            .to_string_lossy()
            .into_owned()
    };

    let off_db = db.clone();
    let off_file = profile.dir.join(offlist::FILE_NAME);
    let worktrees = JsonKey::new(
        WORKTREES_KEY,
        "repositories",
        json!({
            "type": "object",
            "merge": true,
            "about": "Settings > Projects > Worktrees: root is the folder agents' worktrees go in (an empty string restores the default), repos is {repoId: true or false}, whether agents may make worktrees in that repository (read the ids from the repositories key). Send only what changes.",
            "default": {"root": default_root(), "repos": {}},
        }),
        move || json!({"root": default_root(), "repos": {}}),
        |tx, _| worktrees_value(tx),
        |tx, _, v| {
            let o = v.as_object().ok_or("worktrees is {root, repos}.")?;
            let now = helm_core::now_ms();
            if let Some(root) = o.get("root") {
                let r = root.as_str().ok_or("worktrees.root is a folder path, or \"\".")?.trim();
                repos::set_root(tx, (!r.is_empty()).then_some(r)).map_err(why)?;
            }
            if let Some(m) = o.get("repos") {
                let m = m.as_object().ok_or("worktrees.repos is {repoId: true or false}.")?;
                let current = repos::all(tx).map_err(why)?;
                for (id, allowed) in m {
                    let allowed = allowed
                        .as_bool()
                        .ok_or_else(|| format!("worktrees.repos.{id} is true or false."))?;
                    let Some(have) = current.iter().find(|e| &e.repo_id == id) else {
                        return Err(format!("worktrees.repos.{id}: there is no such repository."));
                    };
                    if have.worktrees_allowed != allowed {
                        repos::set_worktrees(tx, id, allowed, now).map_err(why)?;
                    }
                }
            }
            worktrees_value(tx)
        },
    )
    .staged(NONE)
    .after_commit(move || {
        let lines = off_db.snapshot(repos::off_lines).unwrap_or_default();
        if let Err(e) = offlist::write(&off_file, &lines) {
            log::warn!("the worktree off list was not written: {e}");
        }
    })
    .extension();

    let (read_db, link_db, link) = (db.clone(), db.clone(), services.get::<BoardLink>());
    let board = JsonKey::new(
        BOARD_KEY,
        "board_connection",
        json!({
            "type": "string",
            "default": "",
            "about": "Settings > Projects: the Agent Standup address (http or https, no path needed); empty for none. Helm sends the board's work records to it, so a change is staged with app_stage_settings for the user to approve. The board's token is a secret: stage credentials {for: projects}.",
        }),
        || json!(""),
        move |_, _| Ok(json!(stored_url(&read_db))),
        |tx, _, v| {
            let raw = v.as_str().ok_or("boardAddress is text, or \"\" for none.")?;
            let url = if raw.trim().is_empty() {
                String::new()
            } else {
                helm_standup::normalize_url(raw).map_err(|e| e.to_string())?
            };
            tx.execute(
                "INSERT INTO board_connection(id, url) VALUES (1, ?1)
                 ON CONFLICT(id) DO UPDATE SET url = excluded.url",
                [&url],
            )
            .map_err(|e| e.to_string())?;
            Ok(json!(url))
        },
    )
    .staged(NONE)
    .after_commit(move || {
        if let Some(link) = &link {
            let creds = credentials(&profile, &platform);
            link.set(resolve(&link_db, &creds).ok());
        }
    })
    .extension();
    vec![worktrees, board]
}
