//! The projects crate's side tables (host/README.md "Adding SQLite tables"). The core
//! `projects` and `chats` tables belong to the core and are never altered here:
//! everything the sidebar adds lives in these tables, keyed by project or chat id.
//!
//! | Table | Holds |
//! |---|---|
//! | `project_local` | per project, local-only state: pin, position, collapse, icon, archive, a board project's local rename, primary folder, CAS revision |
//! | `project_parents` | local nesting: one parent per project, `source` board or local |
//! | `chat_groups` | the group a chat is shown under when it is not its folder project |
//! | `project_archive_members` | exactly which chats a project archive archived, so restore brings back only those |
//! | `board_projects` | the read-only Agent Standup mirror: one row per board project, with whether all its work is finished |
//! | `board_sync_state` | the last sync's time and error (one row) |
//! | `board_connection` | the Agent Standup address (one row); the token is in the credential store |
//! | `project_top_level` | projects the user moved to the top level, so a board parent never re-nests them |
//! | `project_notes` | the shared context note of a folder project, with its revision |
//!
//! Three triggers make an archived project refuse work at the database: no new
//! chat may be inserted into it, none of its chats may be un-archived, and no chat may be grouped
//! under it, until it is restored.

use helm_core::db::MigrationRegistry;
use rusqlite::Transaction;

pub const ARCHIVED_REFUSAL: &str = "Restore this project first.";

pub fn register(reg: &mut MigrationRegistry) {
    reg.initializer("projects-side-tables", init)
        .one_time("projects-board-completed", add_board_completed)
        .initializer("projects-repositories", crate::repos::init);
}

fn init(tx: &Transaction<'_>) -> rusqlite::Result<()> {
    tx.execute_batch(&format!(
        "CREATE TABLE IF NOT EXISTS project_local (
             project_id                 TEXT PRIMARY KEY REFERENCES projects(id) ON DELETE CASCADE,
             pinned                     INTEGER NOT NULL DEFAULT 0,
             position                   INTEGER,
             collapsed                  INTEGER NOT NULL DEFAULT 0,
             icon                       TEXT,
             archived                   INTEGER NOT NULL DEFAULT 0,
             title_override             TEXT,
             primary_folder_project_id  TEXT REFERENCES projects(id) ON DELETE SET NULL,
             revision                   INTEGER NOT NULL DEFAULT 0
         );
         CREATE TABLE IF NOT EXISTS project_parents (
             project_id  TEXT PRIMARY KEY REFERENCES projects(id) ON DELETE CASCADE,
             parent_id   TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
             source      TEXT NOT NULL CHECK (source IN ('board', 'local'))
         );
         CREATE INDEX IF NOT EXISTS project_parents_parent ON project_parents(parent_id);
         CREATE TABLE IF NOT EXISTS chat_groups (
             chat_id     TEXT PRIMARY KEY REFERENCES chats(id) ON DELETE CASCADE,
             project_id  TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE
         );
         CREATE INDEX IF NOT EXISTS chat_groups_project ON chat_groups(project_id);
         CREATE TABLE IF NOT EXISTS project_archive_members (
             project_id  TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
             chat_id     TEXT NOT NULL REFERENCES chats(id) ON DELETE CASCADE,
             PRIMARY KEY (project_id, chat_id)
         );
         CREATE TABLE IF NOT EXISTS board_projects (
             project_id        TEXT PRIMARY KEY REFERENCES projects(id) ON DELETE CASCADE,
             board_url         TEXT NOT NULL,
             board_project_id  TEXT NOT NULL,
             title             TEXT NOT NULL,
             headline          TEXT,
             area              TEXT,
             repo              TEXT,
             priority          TEXT,
             last_activity     TEXT,
             stale             INTEGER NOT NULL DEFAULT 0,
             synced_at         INTEGER NOT NULL,
             completed         INTEGER NOT NULL DEFAULT 0,
             UNIQUE (board_url, board_project_id)
         );
         CREATE TABLE IF NOT EXISTS board_sync_state (
             id            INTEGER PRIMARY KEY CHECK (id = 1),
             last_sync_at  INTEGER,
             error         TEXT,
             project_count INTEGER NOT NULL DEFAULT 0
         );
         CREATE TABLE IF NOT EXISTS board_connection (
             id   INTEGER PRIMARY KEY CHECK (id = 1),
             url  TEXT NOT NULL
         );
         CREATE TABLE IF NOT EXISTS project_top_level (
             project_id  TEXT PRIMARY KEY REFERENCES projects(id) ON DELETE CASCADE
         );
         CREATE TABLE IF NOT EXISTS project_notes (
             project_id  TEXT PRIMARY KEY REFERENCES projects(id) ON DELETE CASCADE,
             text        TEXT NOT NULL,
             revision    INTEGER NOT NULL,
             updated_at  INTEGER NOT NULL
         );
         CREATE INDEX IF NOT EXISTS chats_sidebar_order ON chats(archived, pinned DESC, position, created_at DESC, id);
         CREATE TRIGGER IF NOT EXISTS projects_archived_refuse_insert
         BEFORE INSERT ON chats
         WHEN EXISTS (SELECT 1 FROM project_local WHERE project_id = NEW.project_id AND archived = 1)
         BEGIN SELECT RAISE(ABORT, '{ARCHIVED_REFUSAL}'); END;
         CREATE TRIGGER IF NOT EXISTS projects_archived_refuse_restore
         BEFORE UPDATE OF archived ON chats
         WHEN OLD.archived = 1 AND NEW.archived = 0 AND EXISTS (
             SELECT 1 FROM project_local
             WHERE archived = 1 AND project_id IN (
                 NEW.project_id,
                 (SELECT project_id FROM chat_groups WHERE chat_id = NEW.id)))
         BEGIN SELECT RAISE(ABORT, '{ARCHIVED_REFUSAL}'); END;
         CREATE TRIGGER IF NOT EXISTS projects_archived_refuse_group
         BEFORE INSERT ON chat_groups
         WHEN EXISTS (SELECT 1 FROM project_local WHERE project_id = NEW.project_id AND archived = 1)
         BEGIN SELECT RAISE(ABORT, '{ARCHIVED_REFUSAL}'); END;"
    ))
}

/// `board_projects.completed` arrived after the table did: a profile made before it gets the
/// column here, unset until the next sync fills it. A new profile already has it from `init`.
fn add_board_completed(tx: &Transaction<'_>) -> rusqlite::Result<()> {
    let present: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM pragma_table_info('board_projects') WHERE name = 'completed')",
        [],
        |r| r.get(0),
    )?;
    if !present {
        tx.execute_batch(
            "ALTER TABLE board_projects ADD COLUMN completed INTEGER NOT NULL DEFAULT 0",
        )?;
    }
    Ok(())
}

#[cfg(test)]
pub(crate) mod tests {
    use helm_core::db::{Database, MigrationRegistry};

    /// A scratch database with the chats and projects side tables registered.
    pub fn temp_db() -> (tempfile::TempDir, Database) {
        let mut reg = MigrationRegistry::new();
        helm_chats::register_migrations(&mut reg);
        crate::register_migrations(&mut reg);
        let dir = tempfile::tempdir().unwrap();
        let db = Database::open(&dir.path().join("t.sqlite3"), &reg).unwrap();
        (dir, db)
    }
}
