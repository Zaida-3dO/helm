//! A ledger of the Ollama variants Helm made for chat windows: `base-helm-<n>k` models created
//! through the `create` call (see `servers::ollama_create`). Helm already removes older variants
//! of a model when it makes a new one, but a variant of a model nobody chats on any more stays on
//! the server, and nothing recorded that Helm made it. One row per variant lets a later step list
//! the stale ones and hide or delete them. Keys and addresses are not here; the row names the
//! server by its settings id.

use rusqlite::{params, Transaction};

/// Create the table. Runs under the crate's schema initializer.
pub fn init(tx: &Transaction<'_>) -> rusqlite::Result<()> {
    tx.execute_batch(
        "CREATE TABLE IF NOT EXISTS local_model_variants (
             server_id  TEXT NOT NULL,
             name       TEXT NOT NULL,
             base       TEXT NOT NULL,
             context    INTEGER NOT NULL,
             created_at INTEGER NOT NULL,
             PRIMARY KEY (server_id, name)
         );",
    )
}

/// Record one variant. `made_now` restamps the row (Helm created it on this pass); a row for a
/// variant that was already on the server when Helm first looked keeps its original row.
pub fn note_variant(
    tx: &Transaction<'_>,
    server_id: &str,
    name: &str,
    base: &str,
    context: u32,
    made_now: bool,
) -> rusqlite::Result<()> {
    let insert = "INSERT INTO local_model_variants(server_id, name, base, context, created_at)
                  VALUES (?1, ?2, ?3, ?4, ?5)";
    if made_now {
        tx.execute(
            &format!("{insert}
             ON CONFLICT(server_id, name) DO UPDATE
               SET base = excluded.base, context = excluded.context, created_at = excluded.created_at"),
            params![server_id, name, base, context, helm_core::now_ms()],
        )?;
    } else {
        tx.execute(
            &format!("{insert} ON CONFLICT(server_id, name) DO NOTHING"),
            params![server_id, name, base, context, helm_core::now_ms()],
        )?;
    }
    Ok(())
}

/// A variant Helm deleted is off the server: its row goes with it.
pub fn forget_variant(tx: &Transaction<'_>, server_id: &str, name: &str) -> rusqlite::Result<()> {
    tx.execute(
        "DELETE FROM local_model_variants WHERE server_id = ?1 AND name = ?2",
        params![server_id, name],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_ledger_stamps_new_variants_backfills_seen_ones_and_lets_them_go() {
        let dir = tempfile::tempdir().unwrap();
        let db = helm_core::db::Database::open(
            &dir.path().join("t.sqlite3"),
            &helm_core::db::MigrationRegistry::new(),
        )
        .unwrap();
        db.write(|tx| {
            init(tx)?;
            note_variant(tx, "ollama", "qwen3:8b-helm-64k", "qwen3:8b", 65_536, true)?;
            note_variant(tx, "ollama", "qwen3:8b-helm-64k", "qwen3:8b", 65_536, false)?;
            Ok::<_, rusqlite::Error>(())
        })
        .unwrap();
        let rows = |db: &helm_core::db::Database| -> Vec<(String, String, String, i64)> {
            db.snapshot(|tx| {
                let mut st =
                    tx.prepare("SELECT server_id, name, base, context FROM local_model_variants")?;
                let got = st
                    .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                Ok::<_, rusqlite::Error>(got)
            })
            .unwrap()
        };
        assert_eq!(
            rows(&db),
            vec![(
                "ollama".to_owned(),
                "qwen3:8b-helm-64k".to_owned(),
                "qwen3:8b".to_owned(),
                65_536
            )],
            "a seen variant does not double or overwrite the made row"
        );
        let stamped = db
            .snapshot(|tx| {
                tx.query_row(
                    "SELECT created_at FROM local_model_variants WHERE name = 'qwen3:8b-helm-64k'",
                    [],
                    |r| r.get::<_, i64>(0),
                )
            })
            .unwrap();
        db.write(|tx| note_variant(tx, "ollama", "qwen3:8b-helm-64k", "qwen3:8b", 65_536, true))
            .unwrap();
        assert!(
            db.snapshot(|tx| tx.query_row(
                "SELECT created_at >= ?1 FROM local_model_variants WHERE name = 'qwen3:8b-helm-64k'",
                [stamped],
                |r| r.get::<_, bool>(0)
            ))
            .unwrap(),
            "a fresh make restamps the row"
        );
        db.write(|tx| forget_variant(tx, "ollama", "qwen3:8b-helm-64k"))
            .unwrap();
        assert!(rows(&db).is_empty());
    }
}
