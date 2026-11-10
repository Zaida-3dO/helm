//! Undo for a chat's edits to the repository list.
//!
//! When a chat changes the list (`app_update_repo`, `app_register_checkout`), the host tool reads
//! the list before and after through `repo_snapshot` and files the pair in the change ring under
//! the key [`KEY`]. The user sees the usual notice with Undo, and Undo puts the earlier list back
//! through [`RepositoryList`].
//!
//! The recorded value is a plain picture of the list: for each repository its remote, default
//! branch, protected bases and checkouts (path and label). It holds nothing the user alone sets (a
//! repository's worktrees switch lives apart and is left as it is). Undo is refused when the list
//! is no longer what the chat left, like every other change in the ring, so it never overwrites
//! something the user did since.
//!
//! Chats cannot write the list through the settings tools: [`RepositoryList::apply`] always
//! refuses and points at the repository tool. The key is listed for chats only so the ring and its
//! Undo can reach it, and, like the tool, only while "Chats can change all settings" is on.

use crate::repos::{self, Entry};
use helm_hosttools::SettingsExtension;
use helm_ipc::AppEvent;
use helm_worktrees::{config, offlist, rules};
use rusqlite::{params, Transaction};
use serde_json::{json, Map, Value};
use std::path::PathBuf;
use std::sync::Arc;

/// The change-ring key a repository-list edit is filed under.
pub const KEY: &str = "repositories";

fn picture(e: &Entry) -> Value {
    json!({
        "remoteUrl": e.remote_url,
        "defaultBranch": e.default_branch,
        "protectedBases": e.protected_bases,
        "checkouts": e.locations.iter()
            .map(|l| json!({"path": l.path, "label": l.label}))
            .collect::<Vec<_>>(),
    })
}

/// The whole list as the ring records it: `{repoId: {remoteUrl, defaultBranch, protectedBases,
/// checkouts: [{path, label}]}}`.
pub fn projection(tx: &Transaction<'_>) -> Result<Value, String> {
    let mut out = Map::new();
    for e in repos::all(tx).map_err(|e| format!("{e:?}"))? {
        out.insert(e.repo_id.clone(), picture(&e));
    }
    Ok(Value::Object(out))
}

/// Keep a folder out of the list the next time it is filled from the projects, as removing it by
/// hand does.
fn ignore(tx: &Transaction<'_>, path: &str) -> Result<(), String> {
    tx.execute(
        "INSERT OR IGNORE INTO repo_ignored(path_key) VALUES (?1)",
        [rules::path_key(path)],
    )
    .map(|_| ())
    .map_err(|e| e.to_string())
}

/// Put the list back as `target` (a [`projection`]) says. Repositories that did not exist then
/// are removed, ones that were removed come back, and every other one gets its remote, bases and
/// checkouts as they were. Checkouts and repositories dropped here are remembered as removed, so
/// the next fill from the projects does not bring them back.
pub fn restore(tx: &Transaction<'_>, target: &Value, now: i64) -> Result<(), String> {
    let sql = |e: rusqlite::Error| e.to_string();
    let want = target
        .as_object()
        .ok_or("The saved repository list cannot be read.")?;
    let have = repos::all(tx).map_err(|e| format!("{e:?}"))?;
    for e in have.iter().filter(|e| !want.contains_key(&e.repo_id)) {
        for l in &e.locations {
            ignore(tx, &l.path)?;
        }
        tx.execute(
            "DELETE FROM repo_checkouts WHERE repo_id = ?1",
            [&e.repo_id],
        )
        .map_err(sql)?;
        tx.execute("DELETE FROM repositories WHERE repo_id = ?1", [&e.repo_id])
            .map_err(sql)?;
    }
    for (id, w) in want {
        let remote = w["remoteUrl"].as_str().and_then(config::parse_remote);
        let bases = match &w["protectedBases"] {
            b @ Value::Array(_) => b.to_string(),
            _ => "[]".into(),
        };
        let branch = w["defaultBranch"].as_str();
        let (host, owner, name) = match &remote {
            Some(r) => (Some(&r.host), Some(&r.owner), Some(&r.name)),
            None => (None, None, None),
        };
        if have.iter().any(|e| &e.repo_id == id) {
            tx.execute(
                "UPDATE repositories SET remote_host = ?2, remote_owner = ?3, remote_name = ?4,
                        default_branch = ?5, protected_bases = ?6 WHERE repo_id = ?1",
                params![id, host, owner, name, branch, bases],
            )
            .map_err(sql)?;
            repos::bump(tx, id).map_err(sql)?;
        } else {
            tx.execute(
                "INSERT INTO repositories(repo_id, remote_host, remote_owner, remote_name,
                        default_branch, protected_bases, revision, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, 0, ?7)",
                params![id, host, owner, name, branch, bases, now],
            )
            .map_err(sql)?;
        }
        let wanted: Vec<(&str, Option<&str>)> = w["checkouts"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|c| Some((c["path"].as_str()?, c["label"].as_str())))
                    .collect()
            })
            .unwrap_or_default();
        let current = repos::get(tx, id).map_err(|e| format!("{e:?}"))?.locations;
        for l in &current {
            let key = rules::path_key(&l.path);
            if !wanted.iter().any(|(p, _)| rules::path_key(p) == key) {
                ignore(tx, &l.path)?;
                tx.execute("DELETE FROM repo_checkouts WHERE id = ?1", [&l.id])
                    .map_err(sql)?;
            }
        }
        for (path, label) in wanted {
            let key = rules::path_key(path);
            match current.iter().find(|l| rules::path_key(&l.path) == key) {
                Some(l) if l.label.as_deref() != label => {
                    tx.execute(
                        "UPDATE repo_checkouts SET label = ?2 WHERE id = ?1",
                        params![l.id, label],
                    )
                    .map_err(sql)?;
                }
                Some(_) => {}
                None => {
                    repos::insert_checkout(tx, id, path, label).map_err(|e| format!("{e:?}"))?;
                }
            }
        }
    }
    Ok(())
}

/// The extension to register as a [`helm_hosttools::SettingsExtensions`] entry. `profile_dir` is
/// where the list of repositories with worktrees off is rewritten after an Undo.
pub fn settings_extension(profile_dir: &std::path::Path) -> Arc<dyn SettingsExtension> {
    Arc::new(RepositoryList {
        off_list: profile_dir.join(offlist::FILE_NAME),
    })
}

struct RepositoryList {
    off_list: PathBuf,
}

impl SettingsExtension for RepositoryList {
    fn keys(&self) -> Vec<&'static str> {
        vec![KEY]
    }

    fn describe(&self, key: &str) -> Option<Value> {
        (key == KEY).then(|| {
            json!({
                "type": "object",
                "default": {},
                "about": "The saved repositories and their checkouts. Read only here: change them with app_update_repo. A change you make that way is filed so the user can undo it.",
            })
        })
    }

    fn needs_all_settings(&self, _key: &str) -> bool {
        true
    }

    fn read(&self, tx: &Transaction<'_>, _key: &str) -> Value {
        projection(tx).unwrap_or_else(|_| json!({}))
    }

    fn apply(
        &self,
        _tx: &Transaction<'_>,
        _key: &str,
        _value: &Value,
    ) -> Result<(Value, Value), String> {
        Err("The repository list is changed with app_update_repo, not here.".into())
    }

    fn restore(&self, tx: &Transaction<'_>, _key: &str, value: &Value) -> Result<(), String> {
        restore(tx, value, helm_core::now_ms())?;
        // The git stand-in reads this file, so it follows the list. A failure here is not the
        // Undo's: the next change to the list writes it again.
        if let Ok(lines) = repos::off_lines(tx) {
            if let Err(e) = offlist::write(&self.off_list, &lines) {
                log::warn!("the worktree off list was not written: {e}");
            }
        }
        Ok(())
    }

    fn changed_event(&self, _key: &str) -> AppEvent {
        AppEvent::new("repositories")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::repos::Patch;

    fn db() -> (tempfile::TempDir, helm_core::db::Database) {
        crate::schema::tests::temp_db()
    }

    fn pic(db: &helm_core::db::Database) -> Value {
        db.snapshot(|tx| projection(tx).map_err(rusqlite::Error::InvalidParameterName))
            .unwrap()
    }

    /// A folder that looks like the root of a clone, with the remote it should have.
    fn clone_at(dir: &std::path::Path, name: &str, remote: &str) -> String {
        let root = dir.join(name);
        std::fs::create_dir_all(root.join(".git")).unwrap();
        std::fs::write(
            root.join(".git").join("config"),
            format!("[remote \"origin\"]\n\turl = {remote}\n"),
        )
        .unwrap();
        root.to_string_lossy().into_owned()
    }

    #[test]
    fn undo_puts_back_what_a_chat_changed_and_nothing_else() {
        let (_d, db) = db();
        let base = tempfile::tempdir().unwrap();
        let a = clone_at(base.path(), "a", "https://github.com/o/alpha");
        let b = clone_at(base.path(), "b", "https://github.com/o/beta");
        let extra = base.path().join("extra");
        std::fs::create_dir_all(&extra).unwrap();
        let extra = extra.to_string_lossy().into_owned();
        let (alpha, beta) = db
            .write(|tx| {
                let (alpha, _) = repos::add_clone(tx, std::path::Path::new(&a), None, 1)?;
                let (beta, _) = repos::add_clone(tx, std::path::Path::new(&b), None, 1)?;
                Ok::<_, crate::store::Refusal>((alpha, beta))
            })
            .unwrap();
        let before = pic(&db);
        assert_eq!(before.as_object().unwrap().len(), 2);

        // A chat adds a checkout and protects a branch on one, and removes the other.
        db.write(|tx| {
            let rev = repos::get(tx, &alpha)?.revision;
            repos::update(
                tx,
                &Patch {
                    repo_id: alpha.clone(),
                    expected_revision: rev,
                    add_locations: vec![(extra.clone(), Some("spare".into()))],
                    protected_bases: Some(vec!["main".into(), "release".into()]),
                    ..Patch::default()
                },
            )?;
            let rev = repos::get(tx, &beta)?.revision;
            repos::update(
                tx,
                &Patch {
                    repo_id: beta.clone(),
                    expected_revision: rev,
                    remove: true,
                    ..Patch::default()
                },
            )?;
            Ok::<_, crate::store::Refusal>(())
        })
        .unwrap();
        let after = pic(&db);
        assert_ne!(before, after);
        assert_eq!(after.as_object().unwrap().len(), 1);

        // Undo restores the whole picture, labels included.
        db.write(|tx| restore(tx, &before, 9).map_err(rusqlite::Error::InvalidParameterName))
            .unwrap();
        assert_eq!(pic(&db), before);
        // Undoing again changes nothing.
        db.write(|tx| restore(tx, &before, 10).map_err(rusqlite::Error::InvalidParameterName))
            .unwrap();
        assert_eq!(pic(&db), before);

        // A checkout that undo took away stays out when the list is next filled from projects.
        db.write(|tx| restore(tx, &after, 11).map_err(rusqlite::Error::InvalidParameterName))
            .unwrap();
        assert_eq!(pic(&db), after);
        let ignored: i64 = db
            .snapshot(|tx| {
                tx.query_row(
                    "SELECT count(*) FROM repo_ignored WHERE path_key = ?1",
                    [rules::path_key(&b)],
                    |r| r.get(0),
                )
            })
            .unwrap();
        assert_eq!(ignored, 1);

        // A value that is not a list is refused and changes nothing.
        let err = db
            .write(|tx| {
                restore(tx, &json!("nope"), 12).map_err(rusqlite::Error::InvalidParameterName)
            })
            .unwrap_err();
        assert!(err.to_string().contains("cannot be read"), "{err}");
        assert_eq!(pic(&db), after);
    }
}
