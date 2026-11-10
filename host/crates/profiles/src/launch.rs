//! The `who` prompt fragment: whose chat this is and where it runs.
//!
//! Every chat tells its agent which person it is working with (the chat's profile: name and the
//! `about` text), which device it runs on, and who else shares the household, on any device. The
//! fragment is left out while the chat's profile is an unnamed placeholder, since "Me" tells an
//! agent nothing. It is a launch contributor of its own, added ahead of the prompt addendum so it
//! opens the system prompt.

use crate::store::{self, Profile};
use helm_core::db::Database;
use helm_workers::contrib::{LaunchContributor, LaunchCtx, LaunchExtras};
use rusqlite::Connection;
use std::sync::Arc;

pub struct WhoLaunch {
    pub db: Arc<Database>,
}

impl LaunchContributor for WhoLaunch {
    fn contribute(&self, ctx: &LaunchCtx<'_>) -> LaunchExtras {
        let text = self
            .db
            .snapshot(|tx| who_text(tx, ctx.chat_id))
            .map_err(|e| log::warn!("the who fragment could not be read: {e}"))
            .ok()
            .flatten();
        LaunchExtras {
            system_prompt: text.into_iter().collect(),
            ..Default::default()
        }
    }
}

/// The fragment's text for a chat, or `None` when the chat's profile is still a placeholder.
pub fn who_text(conn: &Connection, chat_id: &str) -> rusqlite::Result<Option<String>> {
    let Some(me) = store::profile_of_chat(conn, chat_id)? else {
        return Ok(None);
    };
    if me.needs_name {
        return Ok(None);
    }
    // The device the chat runs on. Only this one is known by name here; a chat recorded as
    // running elsewhere is told nothing rather than a guess.
    let device = conn
        .query_row(
            "SELECT d.name FROM device_self d
             WHERE d.uid = COALESCE(
                 NULLIF((SELECT c.host_device_id FROM chats c WHERE c.id = ?1), ''), d.uid)
             ORDER BY d.created_at, d.rowid LIMIT 1",
            [chat_id],
            |r| r.get::<_, String>(0),
        )
        .ok();
    let others: Vec<String> = store::list(conn)?
        .into_iter()
        .filter(|p| p.id != me.id)
        .map(|p| p.name)
        .collect();
    Ok(Some(compose(&me, device.as_deref(), &others)))
}

/// The text from its parts; empty parts are left out.
pub fn compose(me: &Profile, device: Option<&str>, others: &[String]) -> String {
    let mut out = format!("You are working with {} (their Helm profile).", me.name);
    let about = me.about.trim();
    if !about.is_empty() {
        out.push(' ');
        out.push_str(about);
    }
    if let Some(d) = device {
        out.push_str(&format!(
            " This chat runs on {d} under that computer's Claude sign-in."
        ));
    }
    if !others.is_empty() {
        out.push_str(&format!(
            " Other people in this household: {}. Their chats and memories are not yours to read unless {} asks.",
            others.join(", "),
            me.name
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn profile(name: &str, about: &str, needs_name: bool) -> Profile {
        Profile {
            id: name.to_lowercase(),
            name: name.into(),
            colour: "#6d8cff".into(),
            hub_person_id: None,
            is_home: false,
            about: about.into(),
            needs_name,
        }
    }

    #[test]
    fn the_fragment_names_the_person_the_device_and_the_household_and_omits_what_is_empty() {
        let alex = profile("Alex", "Prefers plain words.\nCalled Al.", false);
        let sam = vec!["Sam".to_owned(), "Jo".to_owned()];
        for (about, device, others, want) in [
            (
                alex.about.as_str(),
                Some("Desk"),
                sam.as_slice(),
                "You are working with Alex (their Helm profile). Prefers plain words.\nCalled Al. \
                 This chat runs on Desk under that computer's Claude sign-in. Other people in this \
                 household: Sam, Jo. Their chats and memories are not yours to read unless Alex asks.",
            ),
            (
                "",
                None,
                &[][..],
                "You are working with Alex (their Helm profile).",
            ),
            (
                "  ",
                Some("Laptop"),
                &[][..],
                "You are working with Alex (their Helm profile). This chat runs on Laptop under that computer's Claude sign-in.",
            ),
        ] {
            let mut p = alex.clone();
            p.about = about.into();
            assert_eq!(compose(&p, device, others), want);
        }
    }

    #[test]
    fn a_stored_chat_gets_its_profile_and_this_device_but_a_placeholder_gets_nothing() {
        let mut reg = helm_core::db::MigrationRegistry::new();
        helm_chats::register_migrations(&mut reg);
        crate::register_migrations(&mut reg);
        let dir = tempfile::tempdir().unwrap();
        let db = Database::open(&dir.path().join("t.sqlite3"), &reg).unwrap();
        let text = |db: &Database| db.snapshot(|tx| who_text(tx, "c1")).unwrap();
        db.write(|tx| -> Result<_, store::ProfileError> {
            tx.execute(
                "INSERT INTO projects(id, kind, name, created_at) VALUES ('p1', 'folder', 'p', 1)",
                [],
            )?;
            tx.execute(
                "INSERT INTO chats(id, project_id, created_at, updated_at) VALUES ('c1', 'p1', 1, 1)",
                [],
            )?;
            Ok(())
        })
        .unwrap();
        assert_eq!(text(&db), None, "the home profile has no name yet");
        let device = db
            .write(|tx| -> Result<_, store::ProfileError> {
                let home = store::home(tx)?.unwrap();
                let patch = store::Patch {
                    name: Some("Alex".into()),
                    about: Some("Likes short answers.".into()),
                    ..Default::default()
                };
                store::update(tx, &home.id, patch)?;
                store::create(tx, "Sam", None, None, 2)?;
                Ok(helm_devices::self_name(tx).unwrap())
            })
            .unwrap();
        assert_eq!(
            text(&db).unwrap(),
            format!(
                "You are working with Alex (their Helm profile). Likes short answers. This chat runs on {device} under that computer's Claude sign-in. Other people in this household: Sam. Their chats and memories are not yours to read unless Alex asks."
            )
        );
    }
}
