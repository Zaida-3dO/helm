//! The working mode as a settings key, `workmode`, for `app_get_settings` and the change tools.
//!
//! A chat reads the whole picture (the app's mode and every board's own mode) but cannot change
//! it itself: the key is `Risk::High` in the hosttools' `settings_allow`, so a chat stages the
//! change with `app_stage_settings` and Helm applies it when the user clicks Approve — changing
//! how much a chat may decide on its own is the user's call, not the chat's. An approved change
//! goes through the same machinery as the Settings page uses: the app's mode or one board's,
//! a line sent to each working chat whose mode changed, and the `workmode` event.
//!
//! | The value a staged change carries | Sets |
//! |---|---|
//! | `{"app": "run" or "check_in"}` | the default for every chat |
//! | `{"board": {"id": "<board id>", "mode": "run", "check_in" or null}}` | one board's own mode; null gives it back to the app |
//!
//! What `read` answers — `{"app": …, "boards": [{id, name, mode}]}` — is also what Undo puts
//! back.

use crate::workmode::{self, Mode};
use helm_core::db::Database;
use helm_hosttools::SettingsExtension;
use helm_ipc::AppEvent;
use rusqlite::Transaction;
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};

pub const KEY: &str = "workmode";

pub struct WorkModeKeys {
    db: Arc<Database>,
    runtime: Option<Arc<helm_chats::ChatRuntime>>,
    /// The effective modes the working chats had when the last approved change was written, so
    /// the chats whose mode truly changed are the only ones told.
    told: Mutex<Vec<(String, Mode)>>,
}

/// The extension to register as a [`helm_hosttools::SettingsExtensions`] entry.
pub fn extension(
    db: Arc<Database>,
    runtime: Option<Arc<helm_chats::ChatRuntime>>,
) -> Arc<dyn SettingsExtension> {
    Arc::new(WorkModeKeys {
        db,
        runtime,
        told: Mutex::new(Vec::new()),
    })
}

fn read_value(tx: &Transaction<'_>) -> Value {
    let boards = workmode::overrides(tx)
        .unwrap_or_default()
        .into_iter()
        .map(|(id, name, mode)| json!({"id": id, "name": name, "mode": mode.as_str()}))
        .collect::<Vec<_>>();
    json!({"app": workmode::app_mode(tx).as_str(), "boards": boards})
}

/// A value that is neither shape is refused in words an agent can act on. The returned parts
/// are already parsed: `app` Some only when the value names one, and likewise `board`.
struct Parsed {
    app: Option<Option<Mode>>,
    board: Option<(String, Option<Mode>)>,
}

fn parse(value: &Value) -> Result<Parsed, String> {
    let obj = value
        .as_object()
        .ok_or_else(|| "workmode is an object like {\"app\": \"run\"} or {\"board\": {\"id\": …, \"mode\": …}}.".to_owned())?;
    if obj.is_empty() {
        return Err("workmode needs an \"app\" or a \"board\".".to_owned());
    }
    let mut out = Parsed {
        app: None,
        board: None,
    };
    for (k, v) in obj {
        match k.as_str() {
            "app" => {
                out.app =
                    Some(match v {
                        Value::Null => None,
                        Value::String(s) => Some(Mode::parse(s).ok_or_else(|| {
                            "app must be \"run\", \"check_in\" or null.".to_owned()
                        })?),
                        _ => return Err("app must be \"run\", \"check_in\" or null.".to_owned()),
                    });
            }
            "board" => {
                let b = v
                    .as_object()
                    .ok_or_else(|| "board must be {\"id\": …, \"mode\": …}.".to_owned())?;
                let id = b
                    .get("id")
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty() && s.len() <= 200)
                    .ok_or_else(|| "board.id is the board's id.".to_owned())?;
                let mode = match b.get("mode") {
                    None | Some(Value::Null) => None,
                    Some(Value::String(s)) => Some(Mode::parse(s).ok_or_else(|| {
                        "board.mode must be \"run\", \"check_in\" or null.".to_owned()
                    })?),
                    Some(_) => {
                        return Err("board.mode must be \"run\", \"check_in\" or null.".to_owned())
                    }
                };
                if b.keys().any(|k| k != "id" && k != "mode") {
                    return Err("board takes only id and mode.".to_owned());
                }
                out.board = Some((id.to_owned(), mode));
            }
            other => return Err(format!("workmode has no {other:?}; use app or board.")),
        }
    }
    Ok(out)
}

fn is_board(tx: &Transaction<'_>, id: &str) -> bool {
    tx.query_row(
        "SELECT 1 FROM projects WHERE id = ?1 AND kind = 'board'",
        [id],
        |_| Ok(()),
    )
    .is_ok()
}

impl SettingsExtension for WorkModeKeys {
    fn keys(&self) -> Vec<&'static str> {
        vec![KEY]
    }

    fn describe(&self, key: &str) -> Option<Value> {
        (key == KEY).then(|| {
            json!({
                "type": "object",
                "default": {"app": "check_in"},
                "about": "Settings > Chat & agents > Gatekeeper: how chats work. run: chats carry on and decide the small things themselves, keeping each choice for the user to review. check_in: a chat stops and asks when a choice matters. Send {\"app\": \"run\"|\"check_in\"} for the default of every chat, or {\"board\": {\"id\": …, \"mode\": …|null}} to set one board's own mode (null gives it back to the app). Needs the user's approval: stage it with app_stage_settings.",
            })
        })
    }

    fn read(&self, tx: &Transaction<'_>, key: &str) -> Value {
        if key == KEY {
            read_value(tx)
        } else {
            Value::Null
        }
    }

    fn apply(
        &self,
        _tx: &Transaction<'_>,
        _key: &str,
        _value: &Value,
    ) -> Result<(Value, Value), String> {
        Err("The working mode changes how much a chat may decide on its own, so only you may stage it: use app_stage_settings and the user approves it.".to_owned())
    }

    fn apply_approved(
        &self,
        tx: &Transaction<'_>,
        key: &str,
        value: &Value,
    ) -> Result<(Value, Value), String> {
        if key != KEY {
            return Err(format!("{key} is not a working mode setting."));
        }
        let p = parse(value)?;
        let before = read_value(tx);
        // Remember how the working chats work right now: after the write commits, the ones this
        // change actually moved get a line saying so (an idle chat reads it next turn instead).
        if let Some(rt) = &self.runtime {
            let modes: Vec<(String, Mode)> = rt
                .busy_chats()
                .into_iter()
                .map(|c| {
                    let mode = workmode::effective(tx, &c).mode;
                    (c, mode)
                })
                .collect();
            *self.told.lock().unwrap() = modes;
        }
        if let Some(mode) = p.app {
            workmode::set_app(tx, mode).map_err(|e| e.to_string())?;
        }
        if let Some((id, mode)) = &p.board {
            if !is_board(tx, id) {
                return Err("That is not a board.".to_owned());
            }
            workmode::set_board(tx, id, *mode, helm_core::now_ms()).map_err(|e| e.to_string())?;
        }
        Ok((before, read_value(tx)))
    }

    fn check_approved(
        &self,
        tx: &Transaction<'_>,
        key: &str,
        value: &Value,
    ) -> Result<(Value, Value), String> {
        if key != KEY {
            return Err(format!("{key} is not a working mode setting."));
        }
        let p = parse(value)?;
        if let Some((id, _)) = &p.board {
            if !is_board(tx, id) {
                return Err("That is not a board.".to_owned());
            }
        }
        // What it would become, computed without writing: the card shows this.
        let before = read_value(tx);
        let mut after = before.clone();
        match p.app {
            Some(Some(m)) => after["app"] = json!(m.as_str()),
            Some(None) => after["app"] = json!(Mode::default().as_str()),
            None => {}
        }
        if let Some((id, mode)) = &p.board {
            let name = tx
                .query_row("SELECT name FROM projects WHERE id = ?1", [id], |r| {
                    r.get::<_, String>(0)
                })
                .unwrap_or_default();
            match mode {
                None => {
                    if let Some(list) = after["boards"].as_array_mut() {
                        list.retain(|b| b.get("id").and_then(Value::as_str) != Some(id));
                    }
                }
                Some(m) => {
                    let entry = json!({"id": id, "name": name, "mode": m.as_str()});
                    let list = after["boards"]
                        .as_array_mut()
                        .ok_or("boards is not a list")?;
                    match list
                        .iter_mut()
                        .find(|b| b.get("id").and_then(Value::as_str) == Some(id))
                    {
                        Some(b) => *b = entry,
                        None => list.push(entry),
                    }
                }
            }
        }
        Ok((before, after))
    }

    fn restore(&self, tx: &Transaction<'_>, key: &str, value: &Value) -> Result<(), String> {
        if key != KEY {
            return Err(format!("{key} is not a working mode setting."));
        }
        let app = value
            .get("app")
            .and_then(Value::as_str)
            .and_then(Mode::parse)
            .unwrap_or_default();
        workmode::set_app(tx, Some(app)).map_err(|e| e.to_string())?;
        // The snapshot is the whole picture: an override it does not list was not there when it
        // was taken, so it goes.
        let listed: Vec<(String, Option<Mode>)> = value
            .get("boards")
            .and_then(Value::as_array)
            .map(|list| {
                list.iter()
                    .filter_map(|b| {
                        let id = b.get("id")?.as_str()?.to_owned();
                        let mode = b.get("mode").and_then(Value::as_str).and_then(Mode::parse);
                        Some((id, mode))
                    })
                    .collect()
            })
            .unwrap_or_default();
        let now = helm_core::now_ms();
        for (id, _, _) in workmode::overrides(tx).unwrap_or_default() {
            if !listed.iter().any(|(l, _)| l == &id) {
                workmode::set_board(tx, &id, None, now).map_err(|e| e.to_string())?;
            }
        }
        for (id, mode) in listed {
            if is_board(tx, &id) {
                workmode::set_board(tx, &id, mode, now).map_err(|e| e.to_string())?;
            }
        }
        Ok(())
    }

    fn changed_event(&self, _key: &str) -> AppEvent {
        AppEvent::new("workmode")
    }

    fn after_commit(&self, key: &str) {
        if key != KEY {
            return;
        }
        let Some(rt) = &self.runtime else { return };
        let told = std::mem::take(&mut *self.told.lock().unwrap());
        for (chat, was) in told {
            let now = self
                .db
                .snapshot(|tx| Ok::<_, rusqlite::Error>(workmode::effective(tx, &chat).mode))
                .unwrap_or(Mode::CheckIn);
            if was == now {
                continue;
            }
            let _ = helm_chats::submit::host_send(
                rt,
                helm_chats::submit::HostSend {
                    chat_id: &chat,
                    submission_id: &uuid::Uuid::new_v4().to_string(),
                    text: &workmode::switch_line(now),
                    origin: helm_chats::types::SubmissionOrigin::App,
                    page: None,
                    kind: None,
                    wait: None,
                },
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use helm_core::db::MigrationRegistry;
    use rusqlite::params;

    fn env() -> (tempfile::TempDir, Arc<Database>) {
        let dir = tempfile::tempdir().unwrap();
        let mut reg = MigrationRegistry::new();
        helm_chats::register_migrations(&mut reg);
        reg.initializer("gatekeeper", crate::store::init);
        let db = Arc::new(Database::open(&dir.path().join("t.db"), &reg).unwrap());
        db.write(|tx| {
            tx.execute_batch(
                "CREATE TABLE chat_groups (chat_id TEXT PRIMARY KEY, project_id TEXT NOT NULL);",
            )
        })
        .unwrap();
        (dir, db)
    }

    fn board(db: &Database, id: &str, name: &str) {
        db.write(|tx| {
            tx.execute(
                "INSERT INTO projects (id, kind, name, created_at) VALUES (?1, 'board', ?2, 1)",
                params![id, name],
            )
        })
        .unwrap();
    }

    /// Run one extension call inside a write transaction, keeping its own refusal.
    fn run<T>(
        db: &Database,
        f: impl FnOnce(&Transaction<'_>) -> Result<T, String>,
    ) -> Result<T, String> {
        db.write(|tx| Ok::<_, rusqlite::Error>(f(tx)))
            .expect("the transaction")
    }

    #[test]
    fn the_workmode_key_reads_stages_applies_and_undoes() {
        let (_dir, db) = env();
        let ext = extension(db.clone(), None);
        let snap = || {
            db.snapshot(|tx| Ok::<_, rusqlite::Error>(ext.read(tx, KEY)))
                .unwrap()
        };
        assert_eq!(snap(), json!({"app": "check_in", "boards": []}));

        // A chat changing it itself is refused with the staging sentence.
        let why = run(&db, |tx| ext.apply(tx, KEY, &json!({"app": "run"}))).unwrap_err();
        assert!(why.contains("app_stage_settings"), "{why}");

        // The dry check answers what it would become; the transaction below rolls back, and
        // check_approved itself stores nothing anyway.
        let (before, after) = run(&db, |tx| {
            ext.check_approved(tx, KEY, &json!({"app": "run"}))
        })
        .unwrap();
        assert_eq!(before["app"], "check_in");
        assert_eq!(after["app"], "run");

        // The approved change lands, answering before and after.
        let (before, after) = run(&db, |tx| {
            ext.apply_approved(tx, KEY, &json!({"app": "run"}))
        })
        .unwrap();
        assert_eq!(before["app"], "check_in");
        assert_eq!(after["app"], "run");
        assert_eq!(snap()["app"], "run");

        // A board override joins the list; a bad board or a bad word is refused whole.
        board(&db, "b1", "Garden");
        let (_, after) = run(&db, |tx| {
            ext.apply_approved(tx, KEY, &json!({"board": {"id": "b1", "mode": "check_in"}}))
        })
        .unwrap();
        assert_eq!(
            after["boards"],
            json!([{"id": "b1", "name": "Garden", "mode": "check_in"}])
        );
        for bad in [
            json!({"board": {"id": "nope", "mode": "run"}}),
            json!({"app": "loud"}),
            json!({"boards": []}),
            json!([]),
        ] {
            assert!(
                run(&db, |tx| ext.apply_approved(tx, KEY, &bad.clone())).is_err(),
                "{bad}"
            );
        }

        // Undo puts the snapshot back: the app's mode and no board override.
        run(&db, |tx| {
            ext.restore(tx, KEY, &json!({"app": "check_in", "boards": []}))
        })
        .unwrap();
        assert_eq!(snap(), json!({"app": "check_in", "boards": []}));
    }
}
