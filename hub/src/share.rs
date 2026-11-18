//! `helm-hub share-existing`: the one-off move of memories that were private by default to shared.
//!
//! Before memories were shared by default, every memory a person's tools or an import wrote was
//! private to its owner. A household that now shares what it remembers wants those rows shared
//! too, but a plain `UPDATE` cannot be taken back and cannot tell a memory someone kept private
//! on purpose from one that was only private by default. This command makes the move safe:
//!
//! - `--dry-run` lists what would move and changes nothing.
//! - A run takes a backup of the database first, moves the memories that are private and owned
//!   (and their search copies), writes the ids it moved to a file, and records them in the
//!   `visibility_flips` table under one batch.
//! - `--keep 12,40` leaves those memory ids private; the dry run's list is how to find them. A
//!   memory kept stays private in every later run.
//! - A memory whose name, title, description or body looks like a credential is never moved: it
//!   stays private and is listed apart, because nothing screened it while it was private.
//! - A later run only looks at memories last changed before the first run that still stands, so
//!   a memory someone made private on purpose since then is left alone.
//! - `--revert` puts the latest batch back to private, but only rows still shared that it moved.
//!
//! Search copies of notes and sessions that a PC indexes are not touched: their visibility comes
//! from the source's own setting on that PC and arrives with its next index pass.

use rusqlite::{params, Connection};
use std::path::{Path, PathBuf};

#[derive(Debug, Default, PartialEq)]
pub struct ShareArgs {
    pub dry_run: bool,
    pub revert: bool,
    /// Memory ids that stay private.
    pub keep: Vec<i64>,
    /// Where the list of moved ids is written (default: beside the database).
    pub list: Option<PathBuf>,
}

/// Reads `[--dry-run] [--revert] [--keep ID,ID] [--list FILE]`.
pub fn parse_args(args: &[String]) -> Result<ShareArgs, String> {
    let mut out = ShareArgs::default();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--dry-run" => out.dry_run = true,
            "--revert" => out.revert = true,
            "--keep" => {
                let v = it.next().ok_or("--keep needs memory ids, like 12,40")?;
                for id in v.split(',').map(str::trim).filter(|s| !s.is_empty()) {
                    out.keep.push(
                        id.parse()
                            .map_err(|_| format!("{id:?} is not a memory id"))?,
                    );
                }
            }
            "--list" => {
                out.list = Some(PathBuf::from(it.next().ok_or("--list needs a file")?));
            }
            other => return Err(format!("unexpected argument {other:?}")),
        }
    }
    if out.revert && !out.keep.is_empty() {
        return Err("--keep does not apply to --revert".into());
    }
    Ok(out)
}

/// One memory that moves, or moved.
#[derive(Debug, Clone, PartialEq)]
pub struct Row {
    pub id: i64,
    pub name: Option<String>,
    pub title: String,
    pub source: Option<String>,
}

impl Row {
    /// One line of the list file.
    pub fn line(&self) -> String {
        format!(
            "{}\t{}\t{}\t{}",
            self.id,
            self.name.as_deref().unwrap_or("-"),
            self.source.as_deref().unwrap_or("-"),
            self.title.replace(['\t', '\n'], " ")
        )
    }
}

/// What a run would do: the memories it moves, and the ones it leaves private because they look
/// like they hold a credential.
#[derive(Debug, Default, PartialEq)]
pub struct Plan {
    pub flip: Vec<Row>,
    pub secret: Vec<Row>,
}

/// What a run did.
#[derive(Debug, PartialEq)]
pub struct Outcome {
    pub batch: i64,
    pub moved: Vec<Row>,
    pub secret: Vec<Row>,
}

const KEPT: &str = "CREATE TABLE IF NOT EXISTS visibility_kept (
    batch     INTEGER NOT NULL,
    memory_id INTEGER NOT NULL,
    PRIMARY KEY (batch, memory_id)
)";

const FLIPS: &str = "CREATE TABLE IF NOT EXISTS visibility_flips (
    batch       INTEGER NOT NULL,
    memory_id   INTEGER NOT NULL,
    flipped_at  INTEGER NOT NULL,
    reverted_at INTEGER,
    PRIMARY KEY (batch, memory_id)
)";

fn has_table(conn: &Connection, name: &str) -> rusqlite::Result<bool> {
    conn.query_row(
        "SELECT count(*) FROM sqlite_master WHERE type = 'table' AND name = ?1",
        [name],
        |r| r.get::<_, i64>(0),
    )
    .map(|n| n > 0)
}

/// When the first run that still stands moved memories, as the text `updated_at` uses (UTC ISO,
/// so text order is time order). `None` before any run, or when every run was undone.
pub fn cutoff(conn: &Connection) -> rusqlite::Result<Option<String>> {
    if !has_table(conn, "visibility_flips")? {
        return Ok(None);
    }
    conn.query_row(
        "SELECT strftime('%Y-%m-%dT%H:%M:%fZ', min(batch) / 1000.0, 'unixepoch')
         FROM visibility_flips WHERE reverted_at IS NULL",
        [],
        |r| r.get(0),
    )
}

/// The private, owned memories a share looks at: those last changed before the cutoff (if any),
/// not moved by a run that still stands and not kept by an earlier run, minus `keep`. Split into
/// what would move and what looks like a credential and stays.
pub fn plan(conn: &Connection, keep: &[i64]) -> rusqlite::Result<Plan> {
    let cutoff = cutoff(conn)?;
    let flips = if has_table(conn, "visibility_flips")? {
        " AND id NOT IN (SELECT memory_id FROM visibility_flips WHERE reverted_at IS NULL)"
    } else {
        ""
    };
    let kept = if has_table(conn, "visibility_kept")? {
        " AND id NOT IN (SELECT memory_id FROM visibility_kept)"
    } else {
        ""
    };
    let mut st = conn.prepare(&format!(
        "SELECT id, name, title, source, description, body FROM memories
         WHERE visibility = 'private' AND owner_person IS NOT NULL
           AND (?1 IS NULL OR updated_at < ?1){flips}{kept} ORDER BY id"
    ))?;
    let rows = st.query_map([cutoff], |r| {
        let row = Row {
            id: r.get(0)?,
            name: r.get(1)?,
            title: r.get(2)?,
            source: r.get(3)?,
        };
        let texts = [
            row.name.clone().unwrap_or_default(),
            row.title.clone(),
            r.get::<_, String>(4)?,
            r.get::<_, String>(5)?,
        ];
        let secret = texts.iter().any(|t| crate::secrets::looks_like_secret(t));
        Ok((row, secret))
    })?;
    let mut out = Plan::default();
    for r in rows {
        let (row, secret) = r?;
        if keep.contains(&row.id) {
            continue;
        }
        if secret {
            out.secret.push(row);
        } else {
            out.flip.push(row);
        }
    }
    Ok(out)
}

fn set_visibility(conn: &Connection, id: i64, from: &str, to: &str) -> rusqlite::Result<bool> {
    let n = conn.execute(
        "UPDATE memories SET visibility = ?3 WHERE id = ?1 AND visibility = ?2",
        params![id, from, to],
    )?;
    if n > 0 {
        conn.execute(
            "UPDATE ix_docs SET visibility = ?2 WHERE source_id = 'memory' AND key = ?1",
            params![id.to_string(), to],
        )?;
    }
    Ok(n > 0)
}

/// Move what [`plan`] finds to shared, recording it under a new batch (the time, in
/// milliseconds), and remember the memories `keep` named so a later run leaves them private.
pub fn share(conn: &mut Connection, keep: &[i64], now: i64) -> rusqlite::Result<Outcome> {
    let tx = conn.transaction()?;
    tx.execute_batch(FLIPS)?;
    tx.execute_batch(KEPT)?;
    let found = plan(&tx, &[])?;
    let mut moved = Vec::new();
    for row in found.flip {
        if keep.contains(&row.id) {
            tx.execute(
                "INSERT OR IGNORE INTO visibility_kept (batch, memory_id) VALUES (?1, ?2)",
                params![now, row.id],
            )?;
        } else if set_visibility(&tx, row.id, "private", "shared")? {
            tx.execute(
                "INSERT INTO visibility_flips (batch, memory_id, flipped_at) VALUES (?1, ?2, ?1)",
                params![now, row.id],
            )?;
            moved.push(row);
        }
    }
    tx.commit()?;
    Ok(Outcome {
        batch: now,
        moved,
        secret: found.secret,
    })
}

/// Put the latest batch that still has rows to undo back to private. Only a row that is still
/// shared goes back: one someone has changed since stays as it is. Returns the batch and the ids
/// put back.
pub fn revert(conn: &mut Connection, now: i64) -> rusqlite::Result<Option<(i64, Vec<i64>)>> {
    let tx = conn.transaction()?;
    tx.execute_batch(FLIPS)?;
    let batch: Option<i64> = tx.query_row(
        "SELECT max(batch) FROM visibility_flips WHERE reverted_at IS NULL",
        [],
        |r| r.get(0),
    )?;
    let Some(batch) = batch else {
        return Ok(None);
    };
    let ids: Vec<i64> = tx
        .prepare(
            "SELECT memory_id FROM visibility_flips
             WHERE batch = ?1 AND reverted_at IS NULL ORDER BY memory_id",
        )?
        .query_map([batch], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    let mut back = Vec::new();
    for id in ids {
        if set_visibility(&tx, id, "shared", "private")? {
            back.push(id);
        }
        tx.execute(
            "UPDATE visibility_flips SET reverted_at = ?3 WHERE batch = ?1 AND memory_id = ?2",
            params![batch, id, now],
        )?;
    }
    if has_table(&tx, "visibility_kept")? {
        tx.execute("DELETE FROM visibility_kept WHERE batch = ?1", [batch])?;
    }
    tx.commit()?;
    Ok(Some((batch, back)))
}

/// A copy of the database next to it, made before anything changes. The copy is a whole,
/// consistent database (no write-ahead file to carry along).
pub fn backup(conn: &Connection, db: &Path, now: i64) -> rusqlite::Result<PathBuf> {
    let mut name = db
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "hub.db".into());
    name.push_str(&format!(".before-share-{now}.bak"));
    let target = db.with_file_name(name);
    conn.execute("VACUUM INTO ?1", [target.to_string_lossy().as_ref()])?;
    Ok(target)
}

/// The default place for the list of moved ids.
pub fn list_path(db: &Path, now: i64) -> PathBuf {
    db.with_file_name(format!("share-existing-{now}.txt"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::{NewMemory, Store, Visibility};

    fn memory(store: &mut Store, name: &str, visibility: Visibility, owner: Option<i64>) -> i64 {
        store.set_default_owner(owner);
        let m = NewMemory {
            name: Some(name.into()),
            title: name.into(),
            body: format!("about {name}"),
            visibility,
            ..Default::default()
        };
        store.upsert_named(&m).unwrap();
        store
            .conn()
            .query_row("SELECT id FROM memories WHERE name = ?1", [name], |r| {
                r.get(0)
            })
            .unwrap()
    }

    fn visibility(store: &Store, id: i64) -> (String, String) {
        let c = store.conn();
        (
            c.query_row("SELECT visibility FROM memories WHERE id = ?1", [id], |r| {
                r.get(0)
            })
            .unwrap(),
            c.query_row(
                "SELECT visibility FROM ix_docs WHERE source_id = 'memory' AND key = ?1",
                [id.to_string()],
                |r| r.get(0),
            )
            .unwrap(),
        )
    }

    #[test]
    fn sharing_what_was_private_is_listed_backed_up_and_undone() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("hub.db");
        let mut store = Store::open(&db).unwrap();
        let a = memory(&mut store, "a", Visibility::Private, Some(1));
        let kept = memory(&mut store, "kept", Visibility::Private, Some(1));
        let already = memory(&mut store, "already", Visibility::Shared, Some(2));

        // The dry run's list is what a run would move, and it changes nothing.
        let all = plan(store.conn(), &[]).unwrap().flip;
        assert_eq!(all.iter().map(|r| r.id).collect::<Vec<_>>(), [a, kept]);
        assert_eq!(visibility(&store, a).0, "private");

        // A run backs up first, moves the rest, and records them.
        let bak = backup(store.conn(), &db, 5).unwrap();
        assert!(bak.exists());
        let done = share(store.conn_mut(), &[kept], 10).unwrap();
        let (batch, moved) = (done.batch, done.moved);
        assert_eq!(
            (batch, moved.iter().map(|r| r.id).collect::<Vec<_>>()),
            (10, vec![a])
        );
        assert_eq!(visibility(&store, a), ("shared".into(), "shared".into()));
        assert_eq!(visibility(&store, kept).0, "private");
        assert_eq!(visibility(&store, already).0, "shared");
        assert!(moved[0].line().starts_with(&format!("{a}\ta\t")));
        // The backup still holds the old state.
        let old = Connection::open(&bak).unwrap();
        let before: String = old
            .query_row("SELECT visibility FROM memories WHERE id = ?1", [a], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(before, "private");

        // A second run finds nothing more to move; the undo puts back only what it moved, and
        // not a row someone has changed since.
        assert!(share(store.conn_mut(), &[kept], 11)
            .unwrap()
            .moved
            .is_empty());
        let (batch, back) = revert(store.conn_mut(), 20).unwrap().unwrap();
        assert_eq!((batch, back), (10, vec![a]));
        assert_eq!(visibility(&store, a), ("private".into(), "private".into()));
        assert_eq!(
            visibility(&store, already).0,
            "shared",
            "never moved, never reverted"
        );
        assert!(
            revert(store.conn_mut(), 21).unwrap().is_none(),
            "nothing left to undo"
        );

        // A row someone has changed since the flip is left as it is by the undo.
        share(store.conn_mut(), &[kept], 30).unwrap();
        store
            .conn()
            .execute(
                "UPDATE memories SET visibility = 'private' WHERE id = ?1",
                [a],
            )
            .unwrap();
        let (_, back) = revert(store.conn_mut(), 31).unwrap().unwrap();
        assert!(back.is_empty());

        assert_eq!(
            parse_args(&["--dry-run".into(), "--keep".into(), "3, 4".into()]).unwrap(),
            ShareArgs {
                dry_run: true,
                keep: vec![3, 4],
                ..Default::default()
            }
        );
        assert!(parse_args(&["--keep".into(), "x".into()]).is_err());
        assert!(parse_args(&["--revert".into(), "--keep".into(), "1".into()]).is_err());
    }

    #[test]
    fn a_later_run_leaves_credentials_kept_and_newly_private_memories_alone() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = Store::open(&dir.path().join("hub.db")).unwrap();
        let old = memory(&mut store, "old", Visibility::Private, Some(1));
        let kept = memory(&mut store, "kept", Visibility::Private, Some(1));
        let token = format!("export GITHUB_TOKEN=ghp_{}", "a1B2".repeat(9));
        store.set_default_owner(Some(1));
        store
            .upsert_named(&NewMemory {
                name: Some("cred".into()),
                title: "deploy notes".into(),
                body: token,
                visibility: Visibility::Private,
                ..Default::default()
            })
            .unwrap();
        let cred: i64 = store
            .conn()
            .query_row("SELECT id FROM memories WHERE name = 'cred'", [], |r| {
                r.get(0)
            })
            .unwrap();
        store
            .conn()
            .execute(
                "UPDATE memories SET updated_at = '2020-01-01T00:00:00.000Z'",
                [],
            )
            .unwrap();

        // The dry run and the run agree: the credential stays, and is listed apart.
        let p = plan(store.conn(), &[kept]).unwrap();
        assert_eq!(p.flip.iter().map(|r| r.id).collect::<Vec<_>>(), [old]);
        assert_eq!(p.secret.iter().map(|r| r.id).collect::<Vec<_>>(), [cred]);
        let first = share(store.conn_mut(), &[kept], 1_700_000_000_000).unwrap();
        assert_eq!(first.moved.iter().map(|r| r.id).collect::<Vec<_>>(), [old]);
        assert_eq!(visibility(&store, cred).0, "private");
        assert_eq!(visibility(&store, old).0, "shared");

        // Someone makes a memory private on purpose after the first run, and a shared one
        // private again. A second run (without --keep) moves neither, nor the kept one, and
        // still reports the credential.
        let fresh = memory(&mut store, "fresh", Visibility::Private, Some(1));
        store
            .conn()
            .execute(
                "UPDATE memories SET visibility = 'private' WHERE id = ?1",
                [old],
            )
            .unwrap();
        let second = share(store.conn_mut(), &[], 1_700_000_100_000).unwrap();
        assert!(second.moved.is_empty());
        assert_eq!(
            second.secret.iter().map(|r| r.id).collect::<Vec<_>>(),
            [cred]
        );
        for id in [fresh, kept, cred, old] {
            assert_eq!(visibility(&store, id).0, "private", "{id}");
        }
        assert!(cutoff(store.conn())
            .unwrap()
            .unwrap()
            .starts_with("2023-11-14"));

        // Undoing the run takes the cutoff away, and the kept memory with it.
        revert(store.conn_mut(), 1_700_000_200_000).unwrap();
        assert_eq!(cutoff(store.conn()).unwrap(), None);
        let p = plan(store.conn(), &[]).unwrap();
        assert!(p.flip.iter().any(|r| r.id == kept));
    }
}
