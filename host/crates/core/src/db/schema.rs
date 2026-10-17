//! Versioned core migrations.
//!
//! Version 1 is a new baseline. Choices made here:
//!
//! - `projects.kind` is `folder` or `board` from the start.
//! - No `chats.deleted` flag: deletion is hard and only from Archived.
//! - `chats.harness` accepts `claude` and `codex` only.
//! - `chats.status` is unconstrained text: the failed-state names are left open, and a CHECK
//!   would force a table rebuild each time one is added.
//! - Retry markers get their own side table, `chat_retry_markers`, which recovery clears.
//! - Intent tables (harness switches, message edits, merges, deletions, quit continuations) are
//!   side tables owned by the features that write them, added through feature initializers.
//!
//! Core tables keep this shape; features add side tables.

/// The newest core schema this build understands. A database with a higher `user_version` is
/// refused.
pub const CURRENT_VERSION: u32 = 1;

/// (version, SQL) in order. A migration runs in an immediate transaction followed by a
/// foreign-key check, then sets `user_version`.
pub const CORE_MIGRATIONS: &[(u32, &str)] = &[(1, BASELINE_V1)];

const BASELINE_V1: &str = r#"
CREATE TABLE projects (
    id          TEXT PRIMARY KEY,
    kind        TEXT NOT NULL DEFAULT 'folder' CHECK (kind IN ('folder', 'board')),
    name        TEXT NOT NULL,
    path        TEXT,
    color       TEXT,
    created_at  INTEGER NOT NULL
);

CREATE TABLE chats (
    id              TEXT PRIMARY KEY,
    project_id      TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    title           TEXT NOT NULL DEFAULT '',
    pinned          INTEGER NOT NULL DEFAULT 0,
    archived        INTEGER NOT NULL DEFAULT 0,
    status          TEXT NOT NULL DEFAULT 'idle',
    harness         TEXT NOT NULL DEFAULT 'claude' CHECK (harness IN ('claude', 'codex')),
    provider        TEXT,
    model           TEXT,
    thinking        TEXT,
    created_at      INTEGER NOT NULL,
    updated_at      INTEGER NOT NULL,
    draft           TEXT NOT NULL DEFAULT '',
    draft_revision  INTEGER NOT NULL DEFAULT 0,
    scroll_anchor   TEXT,
    error           TEXT,
    import_source   TEXT UNIQUE,
    position        INTEGER,
    side_source_id  TEXT,
    parent_chat_id  TEXT REFERENCES chats(id) ON DELETE SET NULL
);
CREATE INDEX chats_project ON chats(project_id);

CREATE TABLE submissions (
    id          TEXT PRIMARY KEY,
    chat_id     TEXT NOT NULL REFERENCES chats(id) ON DELETE CASCADE,
    text        TEXT NOT NULL,
    mode        TEXT NOT NULL DEFAULT 'prompt',
    state       TEXT NOT NULL,
    created_at  INTEGER NOT NULL
);
CREATE INDEX submissions_chat ON submissions(chat_id);

CREATE TABLE queued_submissions (
    submission_id  TEXT PRIMARY KEY REFERENCES submissions(id) ON DELETE CASCADE,
    chat_id        TEXT NOT NULL REFERENCES chats(id) ON DELETE CASCADE,
    position       INTEGER NOT NULL
);

CREATE TABLE draft_queue_slots (
    chat_id   TEXT PRIMARY KEY REFERENCES chats(id) ON DELETE CASCADE,
    position  INTEGER NOT NULL,
    revision  INTEGER NOT NULL
);

CREATE TABLE runs (
    id          TEXT PRIMARY KEY,
    chat_id     TEXT NOT NULL REFERENCES chats(id) ON DELETE CASCADE,
    cwd         TEXT,
    model       TEXT,
    thinking    TEXT,
    status      TEXT NOT NULL,
    started_at  INTEGER NOT NULL,
    ended_at    INTEGER
);
CREATE INDEX runs_chat ON runs(chat_id);

CREATE TABLE messages (
    id            INTEGER PRIMARY KEY,
    chat_id       TEXT NOT NULL REFERENCES chats(id) ON DELETE CASCADE,
    source_key    TEXT NOT NULL,
    role          TEXT NOT NULL,
    text          TEXT NOT NULL DEFAULT '',
    thinking      TEXT NOT NULL DEFAULT '',
    tool_name     TEXT,
    tool_call_id  TEXT,
    status        TEXT NOT NULL DEFAULT 'complete',
    timestamp     INTEGER NOT NULL,
    sequence      INTEGER NOT NULL DEFAULT 0,
    revision      INTEGER NOT NULL DEFAULT 0,
    entry_id      TEXT,
    parent_id     TEXT,
    active        INTEGER NOT NULL DEFAULT 1,
    bytes         INTEGER NOT NULL DEFAULT 0,
    artifact      TEXT,
    details       TEXT,
    UNIQUE (chat_id, source_key)
);

CREATE TABLE message_records (
    message_id  INTEGER PRIMARY KEY REFERENCES messages(id) ON DELETE CASCADE,
    raw         TEXT NOT NULL
);

CREATE TABLE entries (
    chat_id    TEXT NOT NULL REFERENCES chats(id) ON DELETE CASCADE,
    entry_id   TEXT NOT NULL,
    parent_id  TEXT,
    kind       TEXT NOT NULL,
    raw        TEXT NOT NULL,
    PRIMARY KEY (chat_id, entry_id)
);

-- Full-text search over user and assistant text only; tool output is excluded.
CREATE VIRTUAL TABLE message_search USING fts5(
    text, content = 'messages', content_rowid = 'id', tokenize = 'unicode61'
);
CREATE TRIGGER message_search_ai AFTER INSERT ON messages
WHEN new.role IN ('user', 'assistant') BEGIN
    INSERT INTO message_search(rowid, text) VALUES (new.id, new.text);
END;
CREATE TRIGGER message_search_ad AFTER DELETE ON messages
WHEN old.role IN ('user', 'assistant') BEGIN
    INSERT INTO message_search(message_search, rowid, text) VALUES ('delete', old.id, old.text);
END;
CREATE TRIGGER message_search_au AFTER UPDATE OF text, role ON messages BEGIN
    INSERT INTO message_search(message_search, rowid, text)
        SELECT 'delete', old.id, old.text WHERE old.role IN ('user', 'assistant');
    INSERT INTO message_search(rowid, text)
        SELECT new.id, new.text WHERE new.role IN ('user', 'assistant');
END;

CREATE TABLE preferences (
    key    TEXT PRIMARY KEY,
    value  TEXT NOT NULL
);

CREATE TABLE counters (
    key    TEXT PRIMARY KEY,
    value  INTEGER NOT NULL
);

CREATE TABLE chat_retry_markers (
    chat_id   TEXT PRIMARY KEY REFERENCES chats(id) ON DELETE CASCADE,
    attempt   INTEGER NOT NULL,
    next_at   INTEGER,
    reason    TEXT
);
"#;
