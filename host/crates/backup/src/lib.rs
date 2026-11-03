//! Backing Helm up, restoring it, and getting one chat out as plain text.
//!
//! Helm keeps everything in one profile folder: the database, the per-chat session logs,
//! artefacts and attachments. This crate writes a plain, readable archive of that folder, lets you
//! queue one of those archives as the profile's next start-up, and exports one chat as a Markdown
//! file.
//!
//! | op | class | args to result |
//! |---|---|---|
//! | `backup_create` | heavy | [`CreateArgs`] to [`Created`] |
//! | `backup_list` | blocking | [`ListArgs`] to [`BackupList`] (the default folder, its archives, and a staged restore if one waits) |
//! | `restore_prepare` | maintenance | [`RestoreArgs`] to [`RestoreStaged`] |
//! | `chat_export_markdown` | blocking | [`ExportArgs`] to [`Exported`] |
//!
//! Three guardrails shape the design:
//!
//! - An archive never holds anything credential-shaped. The credential folder, any `.cred` file at
//!   any level, the lock file, the database sidecars and the restore bookkeeping are excluded by
//!   name, and one test proves no credential path lands in an archive. Envelopes are encrypted to
//!   their own profile anyway, but the rule here is stated and enforced, not inherited.
//! - A restore is refuse-by-default. It replaces the chats, the settings and the approvals, so the
//!   host demands the exact confirmation phrase; nothing changes at the moment of the request, only
//!   a pending note is written. The pending restore is applied at the next start, before anything
//!   opens the profile, and the current database is moved into a dated safety folder first.
//! - The database is copied with SQLite's own online backup API, so the archive holds a consistent
//!   snapshot even while Helm is running; the live file is never copied byte-for-byte.

pub mod archive;
pub mod bindings;
pub mod export;
mod ops;
pub mod restore;
pub mod types;

pub use ops::register_ops;
pub use restore::apply_pending;
pub use types::{
    BackupEntry, BackupList, CreateArgs, Created, ExportArgs, Exported, ListArgs, RestoreArgs,
    RestoreStaged,
};

use std::path::{Path, PathBuf};

/// The one error of this crate; ops map it to an [`helm_ipc::ApiError`] code.
#[derive(Debug, thiserror::Error)]
pub enum BackupError {
    #[error("file: {0}")]
    Io(#[from] std::io::Error),
    #[error("database: {0}")]
    Db(#[from] rusqlite::Error),
    #[error("that folder is not a Helm backup (no readable manifest or database file)")]
    NotABackup,
    #[error("the backup folder must be outside Helm's profile, not inside it")]
    InsideProfile,
    #[error("a restore is already staged; it applies the next time Helm starts")]
    AlreadyStaged,
    #[error("the confirmation phrase must be typed exactly as shown")]
    WrongConfirm,
    #[error("the chat was not found")]
    ChatNotFound,
    #[error("no Documents folder could be found for this user")]
    NoDocuments,
}

impl From<BackupError> for helm_ipc::ApiError {
    fn from(e: BackupError) -> Self {
        use helm_ipc::ApiError;
        match e {
            BackupError::WrongConfirm => {
                ApiError::bad_args("The restore needs the confirmation phrase typed exactly.")
            }
            BackupError::ChatNotFound => ApiError::new("not_found", "That chat is not here."),
            BackupError::NotABackup
            | BackupError::InsideProfile
            | BackupError::AlreadyStaged
            | BackupError::NoDocuments => ApiError::conflict(e.to_string()),
            BackupError::Io(_) | BackupError::Db(_) => ApiError::internal(e.to_string()),
        }
    }
}

/// The user's Documents folder (`USERPROFILE`/`HOME` is read here, never printed).
pub fn user_documents() -> Option<PathBuf> {
    let home =
        std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" }).map(PathBuf::from)?;
    Some(home.join("Documents"))
}

/// Whether `p` is `root` itself or somewhere under it. Compared case- and separator-insensitively,
/// which is right for the only two places it guards: a backup destination and a staged archive.
pub(crate) fn inside(root: &Path, p: &Path) -> bool {
    let norm = |q: &Path| {
        let s = q.to_string_lossy().replace('\\', "/");
        s.trim_end_matches('/').to_lowercase()
    };
    let (r, n) = (norm(root), norm(p));
    n == r || n.starts_with(&format!("{r}/"))
}

/// `20261103-095512` — sortable, collision-safe folder stamp (UTC).
pub(crate) fn stamp(ms: i64) -> String {
    chrono::DateTime::from_timestamp_millis(ms)
        .map(|d| d.format("%Y%m%d-%H%M%S").to_string())
        .unwrap_or_else(|| "unknown-time".to_owned())
}

/// The same moment in the form a person reads in an exported chat's header.
pub(crate) fn stamp_human(ms: i64) -> String {
    chrono::DateTime::from_timestamp_millis(ms)
        .map(|d| d.format("%Y-%m-%d %H:%M").to_string())
        .unwrap_or_else(|| "unknown time".to_owned())
}

#[cfg(test)]
pub(crate) mod testutil {
    use rusqlite::Connection;
    use std::fs;
    use std::path::{Path, PathBuf};
    use tempfile::TempDir;

    /// A profile-shaped fixture: the data folders, a database holding one known row (`tag`),
    /// credential files at the top and in their folder, the lock file, database sidecars, a
    /// stale pending note and an old safety folder.
    pub struct Fixture {
        pub dir: TempDir,
        pub db: PathBuf,
    }

    pub fn profile(tag: &str) -> Fixture {
        let t = TempDir::new().unwrap();
        let dir = t.path();
        let db = dir.join(helm_core::profile::DB_FILE);
        {
            let c = Connection::open(&db).unwrap();
            c.execute_batch(&format!(
                "CREATE TABLE notes(x TEXT); INSERT INTO notes(x) VALUES ('{tag}');"
            ))
            .unwrap();
        }
        fs::write(dir.join(format!("{}-wal", helm_core::profile::DB_FILE)), "sidecar").unwrap();
        fs::create_dir_all(dir.join("sessions/s1")).unwrap();
        fs::write(dir.join("sessions/s1/log.jsonl"), "a session line").unwrap();
        fs::create_dir_all(dir.join("artifacts")).unwrap();
        fs::write(dir.join("artifacts/a.json"), "{}").unwrap();
        fs::create_dir_all(dir.join("attachments")).unwrap();
        fs::create_dir_all(dir.join("credentials")).unwrap();
        fs::write(dir.join("credentials/key.cred"), "an envelope").unwrap();
        fs::write(dir.join("token.cred"), "another envelope").unwrap();
        fs::write(dir.join(helm_core::profile::LOCK_FILE), "lock").unwrap();
        fs::write(dir.join(archive_pending_note()), "{}").unwrap();
        fs::create_dir_all(dir.join("before-restore-20260101-000000")).unwrap();
        Fixture { dir: t, db }
    }

    fn archive_pending_note() -> &'static str {
        crate::restore::PENDING_FILE
    }

    /// The profile's single database row, as stored there right now.
    pub fn db_note(profile_dir: &Path) -> String {
        let c = Connection::open(profile_dir.join(helm_core::profile::DB_FILE)).unwrap();
        c.query_row("SELECT x FROM notes", [], |r| r.get(0)).unwrap()
    }

    /// Every entry under `root`, `/`-joined and sorted; folders end in `/`.
    pub fn tree(root: &Path) -> Vec<String> {
        let mut out = Vec::new();
        walk(root, root, &mut out);
        out.sort();
        out
    }

    fn walk(root: &Path, at: &Path, out: &mut Vec<String>) {
        for e in fs::read_dir(at).unwrap().flatten() {
            let p = e.path();
            if p.is_dir() {
                out.push(rel(root, &p) + "/");
                walk(root, &p, out);
            } else {
                out.push(rel(root, &p));
            }
        }
    }

    fn rel(root: &Path, p: &Path) -> String {
        p.strip_prefix(root)
            .unwrap()
            .to_string_lossy()
            .replace('\\', "/")
    }
}
