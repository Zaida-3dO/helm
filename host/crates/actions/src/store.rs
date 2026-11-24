//! The action set table, seeding, and validation.

use crate::seeds::{self, Seed, SEED_VERSION};
use crate::types::*;
use helm_core::db::Database;
use helm_ipc::ApiError;
use rusqlite::{OptionalExtension, Transaction};
use std::collections::HashSet;

pub fn init(tx: &Transaction<'_>) -> rusqlite::Result<()> {
    tx.execute_batch(
        "CREATE TABLE IF NOT EXISTS action_sets (
             id            INTEGER PRIMARY KEY CHECK (id = 1),
             revision      INTEGER NOT NULL,
             seed_version  INTEGER NOT NULL,
             buttons       TEXT NOT NULL
         );",
    )
}

fn sql(e: rusqlite::Error) -> ApiError {
    ApiError::internal(e.to_string())
}

/// `Database::write` needs an error type it can build from a SQLite error.
pub(crate) struct Fail(ApiError);

impl From<rusqlite::Error> for Fail {
    fn from(e: rusqlite::Error) -> Self {
        Fail(sql(e))
    }
}

impl From<ApiError> for Fail {
    fn from(e: ApiError) -> Self {
        Fail(e)
    }
}

fn read(tx: &Transaction<'_>) -> Result<Option<ActionSet>, ApiError> {
    let row = tx
        .query_row(
            "SELECT revision, seed_version, buttons FROM action_sets WHERE id = 1",
            [],
            |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, i64>(1)?,
                    r.get::<_, String>(2)?,
                ))
            },
        )
        .optional()
        .map_err(sql)?;
    row.map(|(revision, seed_version, json)| {
        let buttons = serde_json::from_str(&json).map_err(|e| ApiError::internal(e.to_string()))?;
        Ok(ActionSet {
            revision,
            seed_version,
            buttons,
        })
    })
    .transpose()
}

fn write_row(tx: &Transaction<'_>, set: &ActionSet) -> Result<(), ApiError> {
    let json =
        serde_json::to_string(&set.buttons).map_err(|e| ApiError::internal(e.to_string()))?;
    tx.execute(
        "INSERT INTO action_sets(id, revision, seed_version, buttons) VALUES (1, ?1, ?2, ?3)
         ON CONFLICT(id) DO UPDATE SET revision = excluded.revision,
             seed_version = excluded.seed_version, buttons = excluded.buttons",
        (set.revision, set.seed_version, json),
    )
    .map_err(sql)?;
    Ok(())
}

/// Read the set, writing the seeds on first read and any seeds added by a newer build, and
/// dropping untouched copies of seeds a newer build retired (see [`seeds::RETIRED`]). A button the
/// user made or touched is never changed, and a seed the user deleted is not brought back (only
/// seeds newer than the stored `seed_version` are added).
pub fn get_or_seed(db: &Database) -> Result<ActionSet, ApiError> {
    get_or_seed_with(db, seeds::SEEDS, SEED_VERSION)
}

pub(crate) fn get_or_seed_with(
    db: &Database,
    list: &[Seed],
    target: i64,
) -> Result<ActionSet, ApiError> {
    db.write::<_, Fail>(|tx| {
        let mut set = read(tx)?.unwrap_or(ActionSet {
            revision: 0,
            seed_version: 0,
            buttons: Vec::new(),
        });
        if set.seed_version >= target && set.revision > 0 {
            return Ok(set);
        }
        let mut fresh = seeds::seeds_after_in(list, set.seed_version);
        // A seed id clash with a user button is skipped rather than failing the read.
        let ids: HashSet<String> = set.buttons.iter().map(|b| b.id.clone()).collect();
        fresh.retain(|b| !ids.contains(&b.id));
        let room = MAX_BUTTONS.saturating_sub(set.buttons.len());
        fresh.truncate(room);
        set.buttons.extend(fresh);
        if set.seed_version < target {
            // Seeds retired by a later build: their capability moved to the skills layer, so a
            // stored button still exactly as shipped (label and text) leaves the bar with the
            // bump. A button the user touched — even just its label — is theirs and stays.
            set.buttons.retain(|b| {
                !b.seed_id.as_deref().is_some_and(|s| {
                    seeds::RETIRED.iter().any(|r| {
                        r.retired_in <= target
                            && r.id == s
                            && r.text == b.text
                            && (b.label == r.label || b.label == r.relabel_from.unwrap_or_default())
                    })
                })
            });
            // Seeds renamed by a later build: a stored button of that seed still carrying the old
            // label exactly gets the new one; a label the user edited is left alone.
            for s in list {
                let Some(old) = s.relabel_from else { continue };
                if let Some(b) = set
                    .buttons
                    .iter_mut()
                    .find(|b| b.seed_id.as_deref() == Some(s.id) && b.label == old)
                {
                    b.label = s.label.into();
                }
            }
        }
        set.seed_version = target;
        set.revision += 1;
        write_row(tx, &set)?;
        Ok(set)
    })
    .map_err(|f| f.0)
}

pub fn save(db: &Database, a: ActionsSaveArgs) -> Result<ActionSet, ApiError> {
    validate(&a.buttons)?;
    db.write::<_, Fail>(|tx| {
        let cur = read(tx)?;
        let (revision, seed_version) = cur.map(|c| (c.revision, c.seed_version)).unwrap_or((0, 0));
        if a.revision != revision {
            return Err(ApiError::conflict(
                "The buttons changed since you opened Settings. Reopen it and try again.",
            )
            .into());
        }
        let set = ActionSet {
            revision: revision + 1,
            seed_version: seed_version.max(SEED_VERSION),
            buttons: a.buttons,
        };
        write_row(tx, &set)?;
        Ok(set)
    })
    .map_err(|f| f.0)
}

pub fn validate(buttons: &[ActionButton]) -> Result<(), ApiError> {
    if buttons.len() > MAX_BUTTONS {
        return Err(ApiError::bad_args(format!(
            "At most {MAX_BUTTONS} buttons."
        )));
    }
    let mut ids = HashSet::new();
    let mut seeds = HashSet::new();
    for b in buttons {
        if b.id.is_empty() || b.id.len() > MAX_ID_BYTES {
            return Err(ApiError::bad_args(
                "A button needs an id of up to 64 bytes.",
            ));
        }
        if !ids.insert(b.id.as_str()) {
            return Err(ApiError::bad_args("Two buttons share an id."));
        }
        if let Some(s) = &b.seed_id {
            if s.is_empty() || s.len() > MAX_ID_BYTES || !seeds.insert(s.as_str()) {
                return Err(ApiError::bad_args(
                    "A seed id is empty, too long or repeated.",
                ));
            }
        }
        let label = b.label.trim();
        let chars = label.chars().count();
        if chars == 0 || chars > MAX_LABEL_CHARS || label.chars().any(char::is_control) {
            return Err(ApiError::bad_args(format!(
                "A label is 1 to {MAX_LABEL_CHARS} characters with no control characters."
            )));
        }
        if b.text.trim().is_empty() || b.text.len() > MAX_TEXT_BYTES {
            return Err(ApiError::bad_args(
                "A button's text is 1 byte to 16 KiB and not blank.",
            ));
        }
        if b.project_id
            .as_deref()
            .is_some_and(|p| p.is_empty() || p.len() > MAX_ID_BYTES)
        {
            return Err(ApiError::bad_args("A project id is empty or too long."));
        }
        if b.kind == ActionKind::Prompt && b.project_id.is_some() {
            return Err(ApiError::bad_args(
                "Only a new-chat button binds to a project.",
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use helm_core::db::MigrationRegistry;

    pub(crate) fn db() -> (tempfile::TempDir, Database) {
        let dir = tempfile::tempdir().unwrap();
        let mut reg = MigrationRegistry::new();
        crate::register_migrations(&mut reg);
        let db = Database::open(&dir.path().join("t.db"), &reg).unwrap();
        (dir, db)
    }

    fn button(id: &str) -> ActionButton {
        ActionButton {
            id: id.into(),
            label: "Go".into(),
            kind: ActionKind::Prompt,
            text: "go".into(),
            project_id: None,
            placement: Placement::Topbar,
            hidden: false,
            seed_id: None,
        }
    }

    #[test]
    fn first_read_writes_the_one_seed_once() {
        let (_d, db) = db();
        let a = get_or_seed(&db).unwrap();
        // The only seed left is the hidden new-chat launcher: the common requests it used to
        // seed are skills now, not buttons.
        let seeded: Vec<(&str, &str)> = a
            .buttons
            .iter()
            .map(|b| (b.label.as_str(), b.text.as_str()))
            .collect();
        assert_eq!(seeded, [("New session", "Let's start a new session.")]);
        assert_eq!(a.revision, 1);
        assert_eq!(a.seed_version, seeds::SEED_VERSION);
        let b = get_or_seed(&db).unwrap();
        assert_eq!(a, b, "a second read changes nothing");
    }

    #[test]
    fn the_new_session_launcher_is_hidden_unbound_and_nothing_shows_in_the_bar() {
        let (_d, db) = db();
        let a = get_or_seed(&db).unwrap();
        let launcher = a.buttons.iter().find(|b| b.label == "New session").unwrap();
        assert!(launcher.hidden);
        assert_eq!(launcher.kind, ActionKind::NewChat);
        assert_eq!(launcher.project_id, None);
        let visible_topbar = a
            .buttons
            .iter()
            .filter(|b| b.placement == Placement::Topbar && !b.hidden)
            .count();
        assert_eq!(visible_topbar, 0, "a fresh install shows no seeded button");
    }

    #[test]
    fn retired_seeds_leave_the_bar_only_while_untouched() {
        let (_d, db) = db();
        // An install made by an older build: the five prompt buttons and the launcher, as shipped.
        let v2: Vec<Seed> = seeds::RETIRED
            .iter()
            .map(|r| Seed {
                id: r.id,
                since: 1,
                label: r.label,
                relabel_from: r.relabel_from,
                kind: ActionKind::Prompt,
                text: r.text,
                placement: Placement::Topbar,
                hidden: false,
            })
            .chain(seeds::SEEDS.iter().copied())
            .collect();
        let mut a = get_or_seed_with(&db, &v2, 2).unwrap();
        assert_eq!(a.buttons.len(), 6);
        // The user renames one retired button, edits another's text, and adds one of their own
        // whose text happens to match a third retired seed exactly.
        a.buttons
            .iter_mut()
            .find(|b| b.seed_id.as_deref() == Some("update"))
            .unwrap()
            .label = "Status please".into();
        a.buttons
            .iter_mut()
            .find(|b| b.seed_id.as_deref() == Some("ship-it"))
            .unwrap()
            .text = "Ship it my way.".into();
        let mut mine = button("mine");
        mine.text = seeds::RETIRED[4].text.into();
        a.buttons.push(mine);
        db.write::<_, Fail>(|tx| {
            let mut set = read(tx)?.unwrap();
            set.buttons = a.buttons;
            set.revision += 1;
            Ok(write_row(tx, &set)?)
        })
        .map_err(|f| f.0)
        .unwrap();
        // This build's read bumps the seed version: buttons still exactly as shipped leave, the
        // touched ones and the user's own stay (a user button is never a seed's copy).
        let b = get_or_seed(&db).unwrap();
        let ids: Vec<&str> = b
            .buttons
            .iter()
            .map(|x| x.seed_id.as_deref().unwrap_or(x.id.as_str()))
            .collect();
        assert_eq!(
            ids,
            ["update", "ship-it", "new-session", "mine"],
            "touched buttons and the launcher stay, the rest leave"
        );
        assert_eq!(b.seed_version, seeds::SEED_VERSION);
        assert_eq!(
            b.buttons,
            get_or_seed(&db).unwrap().buttons,
            "a second read changes nothing"
        );
    }

    #[test]
    fn a_later_seed_version_adds_seeds_relabels_untouched_labels_and_keeps_edits() {
        let (_d, db) = db();
        // An install made by an older build.
        let v1: Vec<Seed> = vec![
            Seed {
                id: "flag",
                since: 1,
                label: "Flag it",
                relabel_from: None,
                kind: ActionKind::Prompt,
                text: "flag?",
                placement: Placement::Topbar,
                hidden: false,
            },
            Seed {
                id: "quiet",
                since: 1,
                label: "Quiet",
                relabel_from: None,
                kind: ActionKind::Prompt,
                text: "quiet?",
                placement: Placement::Topbar,
                hidden: false,
            },
        ];
        let mut a = get_or_seed_with(&db, &v1, 1).unwrap();
        // The user renames "Flag it" and writes the set back; "Quiet" is left as shipped.
        a.buttons[0].label = "Flag for me".into();
        db.write::<_, Fail>(|tx| {
            let mut set = read(tx)?.unwrap();
            set.buttons = a.buttons;
            set.revision += 1;
            Ok(write_row(tx, &set)?)
        })
        .map_err(|f| f.0)
        .unwrap();
        // A newer build ships one more seed (since 2) and renames both buttons: the untouched
        // "Quiet" takes the new label, the edited "Flag for me" keeps the user's.
        let list: Vec<Seed> = vec![
            Seed {
                id: "flag",
                since: 1,
                label: "Needs attention",
                relabel_from: Some("Flag it"),
                kind: ActionKind::Prompt,
                text: "flag?",
                placement: Placement::Topbar,
                hidden: false,
            },
            Seed {
                id: "quiet",
                since: 1,
                label: "Hush",
                relabel_from: Some("Quiet"),
                kind: ActionKind::Prompt,
                text: "quiet?",
                placement: Placement::Topbar,
                hidden: false,
            },
            Seed {
                id: "extra",
                since: 2,
                label: "Extra",
                relabel_from: None,
                kind: ActionKind::Prompt,
                text: "extra",
                placement: Placement::Overflow,
                hidden: false,
            },
        ];
        let b = get_or_seed_with(&db, &list, 2).unwrap();
        let labels: Vec<&str> = b.buttons.iter().map(|x| x.label.as_str()).collect();
        assert_eq!(labels, ["Flag for me", "Hush", "Extra"]);
        assert_eq!(b.seed_version, 2);
    }

    #[test]
    fn save_is_compare_and_set() {
        let (_d, db) = db();
        let a = get_or_seed(&db).unwrap();
        let ok = save(
            &db,
            ActionsSaveArgs {
                revision: a.revision,
                buttons: vec![button("a")],
            },
        )
        .unwrap();
        assert_eq!(ok.revision, a.revision + 1);
        let stale = save(
            &db,
            ActionsSaveArgs {
                revision: a.revision,
                buttons: vec![button("b")],
            },
        )
        .unwrap_err();
        assert_eq!(stale.code, "conflict");
        assert_eq!(get_or_seed(&db).unwrap().buttons, vec![button("a")]);
    }

    #[test]
    fn validation_limits() {
        assert!(validate(&[button("a")]).is_ok());
        let mut b = button("a");
        b.label = "x".repeat(40);
        assert!(validate(&[b.clone()]).is_ok());
        b.label = "x".repeat(41);
        assert!(validate(&[b.clone()]).is_err(), "label over 40");
        b.label = "   ".into();
        assert!(validate(&[b.clone()]).is_err(), "blank label");
        b.label = "ok".into();
        b.text = "x".repeat(16 * 1024);
        assert!(validate(&[b.clone()]).is_ok());
        b.text = "x".repeat(16 * 1024 + 1);
        assert!(validate(&[b.clone()]).is_err(), "text over 16 KiB");
        b.text = String::new();
        assert!(validate(&[b.clone()]).is_err(), "empty text");
        let many: Vec<ActionButton> = (0..25).map(|i| button(&format!("b{i}"))).collect();
        assert!(validate(&many[..24]).is_ok());
        assert!(validate(&many).is_err(), "25 buttons");
        assert!(
            validate(&[button("a"), button("a")]).is_err(),
            "duplicate id"
        );
        let mut p = button("p");
        p.project_id = Some("proj".into());
        assert!(
            validate(&[p.clone()]).is_err(),
            "a prompt button has no project"
        );
        p.kind = ActionKind::NewChat;
        assert!(validate(&[p]).is_ok());
    }

    #[test]
    fn the_op_rejects_unknown_fields() {
        use helm_ipc::{NullSink, OpCtx, Services};
        use std::sync::Arc;
        let (_d, db) = db();
        let mut s = Services::new();
        s.insert(Arc::new(db));
        let ctx = OpCtx {
            services: Arc::new(s),
            events: Arc::new(NullSink),
        };
        let bad = serde_json::json!({"revision": 0, "buttons": [], "extra": 1});
        assert_eq!(
            crate::ops::actions_save(&ctx, bad).unwrap_err().code,
            "bad_args"
        );
        let got = crate::ops::actions_get(&ctx, serde_json::Value::Null).unwrap();
        assert_eq!(got["buttons"].as_array().unwrap().len(), 1);
        assert_eq!(got["buttons"][0]["kind"], "new_chat");
    }
}
