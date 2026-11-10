//! SQLite storage: memories, their tags, an FTS5 index and optional vectors.
//!
//! Vectors are computed by the caller (on the PC) and stored as little-endian
//! f32 blobs next to the id of the model that produced them. The hub never
//! computes an embedding; vector search is a brute-force cosine scan over the
//! rows whose dimension (and model, when given) match the query vector, which
//! is fast enough well past the tens of thousands of rows a personal memory
//! store will hold.
//!
//! Every memory has an owner (a person id) and a visibility, `private` (the default: only its
//! owner sees it) or `shared` (the whole household sees it). Reads that pass a viewer see their
//! own memories plus shared ones; only the owner changes or deletes a memory. A memory with no
//! owner (stored before owners existed) is adopted by one person at start-up ([`Store::adopt_orphans`])
//! and is invisible to everyone until then.

use rusqlite::{params, params_from_iter, Connection, OptionalExtension};
use serde::Serialize;
use std::collections::HashMap;
use std::path::Path;

/// Largest accepted vector dimension. Common embedding models sit at 384–3072.
pub const MAX_DIM: usize = 4096;
pub const MAX_TITLE: usize = 500;
pub const MAX_BODY: usize = 1_000_000;
pub const MAX_TAGS: usize = 32;
pub const MAX_K: usize = 100;
/// Each ranker contributes this many candidates to a hybrid search.
const CANDIDATES: usize = 50;
/// Reciprocal-rank-fusion constant (the usual value from the RRF paper).
const RRF_K: f64 = 60.0;

#[derive(Debug)]
pub enum StoreError {
    Invalid(String),
    NotFound,
    Db(rusqlite::Error),
}

impl std::fmt::Display for StoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StoreError::Invalid(m) => write!(f, "{m}"),
            StoreError::NotFound => write!(f, "memory not found"),
            StoreError::Db(e) => write!(f, "database error: {e}"),
        }
    }
}

impl std::error::Error for StoreError {}

impl From<rusqlite::Error> for StoreError {
    fn from(e: rusqlite::Error) -> Self {
        StoreError::Db(e)
    }
}

pub type Result<T> = std::result::Result<T, StoreError>;

/// A vector and the id of the model that produced it.
#[derive(Debug, Clone, PartialEq)]
pub struct Embedding {
    pub model: String,
    pub values: Vec<f32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Visibility {
    #[default]
    Private,
    Shared,
}

impl Visibility {
    pub fn as_str(self) -> &'static str {
        match self {
            Visibility::Private => "private",
            Visibility::Shared => "shared",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s.trim() {
            "private" => Some(Visibility::Private),
            "shared" => Some(Visibility::Shared),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct NewMemory {
    pub name: Option<String>,
    pub title: String,
    pub body: String,
    pub description: Option<String>,
    pub kind: Option<String>,
    pub tags: Vec<String>,
    pub source: Option<String>,
    pub links: Vec<String>,
    pub embedding: Option<Embedding>,
    /// The person it belongs to; `None` takes the store's default owner.
    pub owner: Option<i64>,
    pub visibility: Visibility,
}

/// Fields a `memory_update` may change; `None` leaves a field alone.
#[derive(Debug, Clone, Default)]
pub struct MemoryPatch {
    pub title: Option<String>,
    pub body: Option<String>,
    pub description: Option<String>,
    pub tags: Option<Vec<String>>,
    pub source: Option<String>,
    pub embedding: Option<Embedding>,
    pub visibility: Option<Visibility>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Memory {
    pub id: i64,
    pub name: Option<String>,
    pub title: String,
    pub description: Option<String>,
    #[serde(rename = "type")]
    pub kind: Option<String>,
    pub body: String,
    pub tags: Vec<String>,
    pub source: Option<String>,
    pub links: Vec<String>,
    pub model: Option<String>,
    pub dim: Option<i64>,
    pub created_at: String,
    pub updated_at: String,
    pub visibility: String,
    /// Whether the viewer owns it (false when no viewer was given).
    pub mine: bool,
    #[serde(skip)]
    pub owner_person: Option<i64>,
}

/// A row in a list or search result: everything but the body.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Summary {
    pub id: i64,
    pub name: Option<String>,
    pub title: String,
    pub description: Option<String>,
    #[serde(rename = "type")]
    pub kind: Option<String>,
    pub tags: Vec<String>,
    pub updated_at: String,
    pub visibility: String,
    /// Whether the viewer owns it (false when no viewer was given).
    pub mine: bool,
    #[serde(skip)]
    pub owner_person: Option<i64>,
}

impl Summary {
    fn mark(mut self, viewer: Option<i64>) -> Self {
        self.mine = viewer.is_some() && self.owner_person == viewer;
        self
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Hit {
    #[serde(flatten)]
    pub summary: Summary,
    pub score: f64,
    /// Which rankers found it: "keyword", "vector" or both.
    pub matched: Vec<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub snippet: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct SearchQuery {
    pub query: String,
    pub k: usize,
    pub vector: Option<Vec<f32>>,
    pub model: Option<String>,
    pub tags: Vec<String>,
    /// Whose memories may be returned: their own and shared ones. `None` sees everything.
    pub viewer: Option<i64>,
    /// Only the memories this person wrote (among those the viewer may see). `None` is anyone's.
    pub owner: Option<i64>,
}

pub struct Store {
    conn: Connection,
    /// The owner given to a memory stored with none (the importer's person).
    default_owner: Option<i64>,
}

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS memories (
    id          INTEGER PRIMARY KEY,
    name        TEXT UNIQUE,
    title       TEXT NOT NULL,
    description TEXT NOT NULL DEFAULT '',
    kind        TEXT,
    body        TEXT NOT NULL,
    tags        TEXT NOT NULL DEFAULT '[]',
    source      TEXT,
    links       TEXT NOT NULL DEFAULT '[]',
    vector      BLOB,
    model       TEXT,
    dim         INTEGER,
    created_at  TEXT NOT NULL,
    updated_at  TEXT NOT NULL,
    owner_person INTEGER,
    visibility  TEXT NOT NULL DEFAULT 'private'
);
CREATE INDEX IF NOT EXISTS memories_updated ON memories(updated_at);
CREATE TABLE IF NOT EXISTS memory_tags (
    memory_id INTEGER NOT NULL REFERENCES memories(id) ON DELETE CASCADE,
    tag       TEXT NOT NULL,
    PRIMARY KEY (memory_id, tag)
);
CREATE INDEX IF NOT EXISTS memory_tags_tag ON memory_tags(tag);
CREATE VIRTUAL TABLE IF NOT EXISTS memories_fts USING fts5(
    title, description, body, tags,
    content='memories', content_rowid='id',
    tokenize='porter unicode61'
);
CREATE TRIGGER IF NOT EXISTS memories_ai AFTER INSERT ON memories BEGIN
    INSERT INTO memories_fts(rowid, title, description, body, tags)
    VALUES (new.id, new.title, new.description, new.body, new.tags);
END;
CREATE TRIGGER IF NOT EXISTS memories_ad AFTER DELETE ON memories BEGIN
    INSERT INTO memories_fts(memories_fts, rowid, title, description, body, tags)
    VALUES ('delete', old.id, old.title, old.description, old.body, old.tags);
END;
CREATE TRIGGER IF NOT EXISTS memories_au AFTER UPDATE OF title, description, body, tags ON memories BEGIN
    INSERT INTO memories_fts(memories_fts, rowid, title, description, body, tags)
    VALUES ('delete', old.id, old.title, old.description, old.body, old.tags);
    INSERT INTO memories_fts(rowid, title, description, body, tags)
    VALUES (new.id, new.title, new.description, new.body, new.tags);
END;
"#;

/// Whether `s` is `YYYY-MM-DD`, optionally followed by `T` (or a space) and `HH:MM[:SS[.fff]]` and
/// an optional `Z`. Text order of such a string is time order against `updated_at`, which is why
/// nothing looser is let through: `since: "yesterday"` would otherwise match every memory.
fn is_iso_since(s: &str) -> bool {
    let b = s.as_bytes();
    let digits = |from: usize, n: usize| -> Option<u32> {
        let part = b.get(from..from + n)?;
        part.iter()
            .all(u8::is_ascii_digit)
            .then(|| part.iter().fold(0, |acc, d| acc * 10 + u32::from(d - b'0')))
    };
    let (Some(_), Some(month), Some(day)) = (digits(0, 4), digits(5, 2), digits(8, 2)) else {
        return false;
    };
    if b.get(4) != Some(&b'-') || b.get(7) != Some(&b'-') || !(1..=12).contains(&month) {
        return false;
    }
    if !(1..=31).contains(&day) {
        return false;
    }
    if b.len() == 10 {
        return true;
    }
    if !matches!(b[10], b'T' | b' ') {
        return false;
    }
    let (Some(hour), Some(minute)) = (digits(11, 2), digits(14, 2)) else {
        return false;
    };
    if b.get(13) != Some(&b':') || hour > 23 || minute > 59 {
        return false;
    }
    let mut rest = &s[16..];
    if let Some(r) = rest.strip_prefix(':') {
        let Some(second) = digits(17, 2) else {
            return false;
        };
        if second > 60 {
            return false;
        }
        rest = &r[2..];
        if let Some(frac) = rest.strip_prefix('.') {
            let n = frac.bytes().take_while(u8::is_ascii_digit).count();
            if n == 0 {
                return false;
            }
            rest = &frac[n..];
        }
    }
    rest.is_empty() || rest == "Z"
}

const NOW: &str = "strftime('%Y-%m-%dT%H:%M:%fZ','now')";
const SUMMARY_COLS: &str =
    "m.id, m.name, m.title, m.description, m.kind, m.tags, m.updated_at, m.visibility, m.owner_person";

impl Store {
    pub fn open(path: &Path) -> Result<Self> {
        Self::init(Connection::open(path)?)
    }

    pub fn open_in_memory() -> Result<Self> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(conn: Connection) -> Result<Self> {
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        conn.pragma_update(None, "busy_timeout", 5000)?;
        // Keep the page cache small: the hub's RAM budget is tens of MB.
        conn.pragma_update(None, "cache_size", -2000)?;
        conn.execute_batch(SCHEMA)?;
        // A store made before owners existed gains the columns; its memories have no owner yet.
        let has_owner = conn
            .prepare("SELECT 1 FROM pragma_table_info('memories') WHERE name = 'owner_person'")?
            .exists([])?;
        if !has_owner {
            conn.execute_batch(
                "ALTER TABLE memories ADD COLUMN owner_person INTEGER;
                 ALTER TABLE memories ADD COLUMN visibility TEXT NOT NULL DEFAULT 'private';",
            )?;
        }
        conn.execute_batch(
            "CREATE INDEX IF NOT EXISTS memories_owner ON memories(owner_person, visibility);",
        )?;
        // The household's knowledge files share this database (see `knowledge.rs`).
        crate::knowledge::init(&conn)?;
        Ok(Store {
            conn,
            default_owner: None,
        })
    }

    /// The connection, for the knowledge files.
    pub(crate) fn conn(&self) -> &Connection {
        &self.conn
    }

    pub(crate) fn conn_mut(&mut self) -> &mut Connection {
        &mut self.conn
    }

    /// The owner a memory gets when its writer names none: the importer's person.
    pub fn set_default_owner(&mut self, person: Option<i64>) {
        self.default_owner = person;
    }

    /// Give every memory with no owner to `person`, private. Returns how many moved.
    pub fn adopt_orphans(&mut self, person: i64) -> Result<usize> {
        Ok(self.conn.execute(
            "UPDATE memories SET owner_person = ?1, visibility = 'private'
             WHERE owner_person IS NULL",
            [person],
        )?)
    }

    pub fn count(&self) -> Result<i64> {
        Ok(self
            .conn
            .query_row("SELECT count(*) FROM memories", [], |r| r.get(0))?)
    }

    pub fn insert(&mut self, m: NewMemory) -> Result<i64> {
        validate_text(&m.title, m.body.as_str())?;
        let tags = normalize_tags(&m.tags)?;
        let (blob, model, dim) = embedding_columns(m.embedding.as_ref())?;
        if let Some(name) = &m.name {
            if name.trim().is_empty() {
                return Err(StoreError::Invalid("name must not be empty".into()));
            }
        }
        let tx = self.conn.transaction()?;
        let exists: bool = match &m.name {
            Some(n) => tx
                .query_row("SELECT 1 FROM memories WHERE name = ?1", [n], |_| Ok(()))
                .optional()?
                .is_some(),
            None => false,
        };
        if exists {
            return Err(StoreError::Invalid(format!(
                "a memory named {:?} already exists",
                m.name.unwrap_or_default()
            )));
        }
        tx.execute(
            &format!(
                "INSERT INTO memories (name, title, description, kind, body, tags, source, links,
                 vector, model, dim, owner_person, visibility, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, {NOW}, {NOW})"
            ),
            params![
                m.name,
                m.title.trim(),
                m.description.unwrap_or_default(),
                m.kind,
                m.body,
                json_list(&tags),
                m.source,
                json_list(&m.links),
                blob,
                model,
                dim,
                m.owner.or(self.default_owner),
                m.visibility.as_str(),
            ],
        )?;
        let id = tx.last_insert_rowid();
        write_tags(&tx, id, &tags)?;
        tx.commit()?;
        Ok(id)
    }

    pub fn get(&self, id: i64) -> Result<Memory> {
        self.get_where("id = ?1", rusqlite::types::Value::Integer(id))
    }

    pub fn get_by_name(&self, name: &str) -> Result<Memory> {
        self.get_where("name = ?1", rusqlite::types::Value::Text(name.to_owned()))
    }

    /// One memory as `viewer` may see it: their own or a shared one, else "not found" (the same
    /// answer for a memory that does not exist, so nothing says what others hold).
    pub fn get_visible(&self, id: i64, viewer: i64) -> Result<Memory> {
        self.get(id).and_then(|m| visible_to(m, viewer))
    }

    /// [`Store::get_visible`], by name.
    pub fn get_by_name_visible(&self, name: &str, viewer: i64) -> Result<Memory> {
        self.get_by_name(name).and_then(|m| visible_to(m, viewer))
    }

    fn get_where(&self, cond: &str, arg: rusqlite::types::Value) -> Result<Memory> {
        self.conn
            .query_row(
                &format!(
                    "SELECT id, name, title, description, kind, body, tags, source, links,
                     model, dim, created_at, updated_at, visibility, owner_person
                     FROM memories WHERE {cond}"
                ),
                [arg],
                |r| {
                    Ok(Memory {
                        id: r.get(0)?,
                        name: r.get(1)?,
                        title: r.get(2)?,
                        description: non_empty(r.get(3)?),
                        kind: r.get(4)?,
                        body: r.get(5)?,
                        tags: parse_list(&r.get::<_, String>(6)?),
                        source: r.get(7)?,
                        links: parse_list(&r.get::<_, String>(8)?),
                        model: r.get(9)?,
                        dim: r.get(10)?,
                        created_at: r.get(11)?,
                        updated_at: r.get(12)?,
                        visibility: r.get(13)?,
                        mine: false,
                        owner_person: r.get(14)?,
                    })
                },
            )
            .optional()?
            .ok_or(StoreError::NotFound)
    }

    /// Newest first. `since` is an ISO-8601 date or timestamp compared against
    /// `updated_at` (both are UTC ISO strings, so text order is time order).
    pub fn list(
        &self,
        tag: Option<&str>,
        since: Option<&str>,
        limit: usize,
    ) -> Result<Vec<Summary>> {
        self.list_for(None, tag, since, limit)
    }

    /// [`Store::list`] as `viewer` sees it: their own memories and shared ones.
    pub fn list_for(
        &self,
        viewer: Option<i64>,
        tag: Option<&str>,
        since: Option<&str>,
        limit: usize,
    ) -> Result<Vec<Summary>> {
        self.list_by(viewer, None, tag, since, limit)
    }

    /// [`Store::list_for`], narrowed to the memories `owner` wrote (that the viewer may see).
    pub fn list_by(
        &self,
        viewer: Option<i64>,
        owner: Option<i64>,
        tag: Option<&str>,
        since: Option<&str>,
        limit: usize,
    ) -> Result<Vec<Summary>> {
        let mut sql = format!("SELECT {SUMMARY_COLS} FROM memories m WHERE 1=1");
        let mut args: Vec<rusqlite::types::Value> = Vec::new();
        sql.push_str(&visible_filter(viewer, &mut args));
        sql.push_str(&owner_filter(owner, &mut args));
        if let Some(t) = tag {
            args.push(t.trim().to_lowercase().into());
            sql.push_str(&format!(
                " AND m.id IN (SELECT memory_id FROM memory_tags WHERE tag = ?{})",
                args.len()
            ));
        }
        if let Some(s) = since {
            let s = s.trim();
            if !is_iso_since(s) {
                return Err(StoreError::Invalid(format!(
                    "since must be an ISO-8601 date or UTC timestamp such as 2026-11-01 or \
                     2026-11-01T09:30:00Z, not {s:?}"
                )));
            }
            args.push(s.to_owned().into());
            sql.push_str(&format!(" AND m.updated_at >= ?{}", args.len()));
        }
        args.push((limit.clamp(1, 1000) as i64).into());
        sql.push_str(&format!(
            " ORDER BY m.updated_at DESC, m.id DESC LIMIT ?{}",
            args.len()
        ));
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(params_from_iter(args), |r| {
            Ok(summary_from_row(r)?.mark(viewer))
        })?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }

    pub fn search(&self, q: &SearchQuery) -> Result<Vec<Hit>> {
        let k = q.k.clamp(1, MAX_K);
        let tags = normalize_tags(&q.tags)?;
        let fts = self.keyword_search(
            &q.query,
            &tags,
            q.viewer,
            q.owner,
            if q.vector.is_some() { CANDIDATES } else { k },
        )?;
        let Some(vector) = &q.vector else {
            return Ok(fts.into_iter().take(k).collect());
        };
        check_vector(vector)?;
        let vec_hits = self.vector_search(
            vector,
            q.model.as_deref(),
            &tags,
            q.viewer,
            q.owner,
            CANDIDATES,
        )?;

        // Reciprocal-rank fusion: robust to the two rankers' scores living on
        // unrelated scales, and needs no tuning.
        let mut fused: HashMap<i64, Hit> = HashMap::new();
        for (rank, mut hit) in fts.into_iter().enumerate() {
            hit.score = 1.0 / (RRF_K + rank as f64 + 1.0);
            fused.insert(hit.summary.id, hit);
        }
        for (rank, (summary, _cos)) in vec_hits.into_iter().enumerate() {
            let add = 1.0 / (RRF_K + rank as f64 + 1.0);
            fused
                .entry(summary.id)
                .and_modify(|h| {
                    h.score += add;
                    h.matched.push("vector");
                })
                .or_insert(Hit {
                    summary,
                    score: add,
                    matched: vec!["vector"],
                    snippet: None,
                });
        }
        let mut hits: Vec<Hit> = fused.into_values().collect();
        hits.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then(a.summary.id.cmp(&b.summary.id))
        });
        hits.truncate(k);
        Ok(hits)
    }

    fn keyword_search(
        &self,
        query: &str,
        tags: &[String],
        viewer: Option<i64>,
        owner: Option<i64>,
        limit: usize,
    ) -> Result<Vec<Hit>> {
        let Some(match_expr) = fts_query(query) else {
            return Ok(Vec::new());
        };
        let mut args: Vec<rusqlite::types::Value> = vec![match_expr.into()];
        let mut sql = format!(
            "SELECT {SUMMARY_COLS}, bm25(memories_fts, 10.0, 5.0, 1.0, 3.0) AS rank,
             snippet(memories_fts, 2, '[', ']', '…', 16)
             FROM memories_fts JOIN memories m ON m.id = memories_fts.rowid
             WHERE memories_fts MATCH ?1"
        );
        sql.push_str(&tag_filter(tags, &mut args));
        sql.push_str(&visible_filter(viewer, &mut args));
        sql.push_str(&owner_filter(owner, &mut args));
        args.push((limit as i64).into());
        sql.push_str(&format!(" ORDER BY rank LIMIT ?{}", args.len()));
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(params_from_iter(args), |r| {
            let bm25: f64 = r.get(9)?;
            Ok(Hit {
                summary: summary_from_row(r)?.mark(viewer),
                // bm25() is lower-is-better; flip it so higher is better everywhere.
                score: -bm25,
                matched: vec!["keyword"],
                snippet: Some(r.get(10)?),
            })
        })?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }

    fn vector_search(
        &self,
        query: &[f32],
        model: Option<&str>,
        tags: &[String],
        viewer: Option<i64>,
        owner: Option<i64>,
        limit: usize,
    ) -> Result<Vec<(Summary, f64)>> {
        let qnorm = norm(query);
        if qnorm == 0.0 {
            return Err(StoreError::Invalid("vector must not be all zeros".into()));
        }
        let mut args: Vec<rusqlite::types::Value> = vec![(query.len() as i64).into()];
        let mut sql = format!(
            "SELECT {SUMMARY_COLS}, m.vector FROM memories m
             WHERE m.vector IS NOT NULL AND m.dim = ?1"
        );
        if let Some(model) = model {
            args.push(model.to_owned().into());
            sql.push_str(&format!(" AND m.model = ?{}", args.len()));
        }
        sql.push_str(&tag_filter(tags, &mut args));
        sql.push_str(&visible_filter(viewer, &mut args));
        sql.push_str(&owner_filter(owner, &mut args));
        let mut stmt = self.conn.prepare(&sql)?;
        let mut rows = stmt.query(params_from_iter(args))?;
        let mut scored: Vec<(Summary, f64)> = Vec::new();
        while let Some(r) = rows.next()? {
            let blob: Vec<u8> = r.get(9)?;
            let v = decode_vector(&blob);
            let vn = norm(&v);
            if vn == 0.0 {
                continue;
            }
            let dot: f64 = query
                .iter()
                .zip(&v)
                .map(|(a, b)| *a as f64 * *b as f64)
                .sum();
            scored.push((summary_from_row(r)?.mark(viewer), dot / (qnorm * vn)));
        }
        scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        scored.truncate(limit);
        Ok(scored)
    }

    pub fn update(&mut self, id: i64, p: MemoryPatch) -> Result<Memory> {
        let cur = self.get(id)?;
        let title = p.title.unwrap_or(cur.title);
        let body = p.body.unwrap_or(cur.body.clone());
        validate_text(&title, &body)?;
        let tags = match p.tags {
            Some(t) => normalize_tags(&t)?,
            None => cur.tags.clone(),
        };
        let tx = self.conn.transaction()?;
        tx.execute(
            &format!(
                "UPDATE memories SET title = ?2, body = ?3, description = ?4, tags = ?5,
                 source = ?6, updated_at = {NOW} WHERE id = ?1"
            ),
            params![
                id,
                title.trim(),
                body,
                p.description.or(cur.description).unwrap_or_default(),
                json_list(&tags),
                p.source.or(cur.source),
            ],
        )?;
        if let Some(e) = p.embedding.as_ref() {
            let (blob, model, dim) = embedding_columns(Some(e))?;
            tx.execute(
                "UPDATE memories SET vector = ?2, model = ?3, dim = ?4 WHERE id = ?1",
                params![id, blob, model, dim],
            )?;
        } else if body != cur.body {
            // The stored vector described the old body.
            tx.execute(
                "UPDATE memories SET vector = NULL, model = NULL, dim = NULL WHERE id = ?1",
                [id],
            )?;
        }
        if let Some(v) = p.visibility {
            tx.execute(
                "UPDATE memories SET visibility = ?2 WHERE id = ?1",
                params![id, v.as_str()],
            )?;
        }
        write_tags(&tx, id, &tags)?;
        tx.commit()?;
        self.get(id)
    }

    pub fn delete(&mut self, id: i64) -> Result<()> {
        let n = self
            .conn
            .execute("DELETE FROM memories WHERE id = ?1", [id])?;
        if n == 0 {
            return Err(StoreError::NotFound);
        }
        Ok(())
    }

    /// `(id, name, source)` of every memory whose source starts with `prefix` (case-sensitive,
    /// no wildcards), for the importer's prune.
    pub fn with_source_prefix(&self, prefix: &str) -> Result<Vec<(i64, Option<String>, String)>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, name, source FROM memories
             WHERE source IS NOT NULL AND substr(source, 1, length(?1)) = ?1 ORDER BY id",
        )?;
        let rows = stmt.query_map([prefix], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }

    /// Insert or refresh a memory keyed on `name`, for the importer.
    /// Returns what happened. A changed body drops the stored vector.
    pub fn upsert_named(&mut self, m: &NewMemory) -> Result<Upsert> {
        let name = m
            .name
            .as_deref()
            .ok_or_else(|| StoreError::Invalid("upsert needs a name".into()))?;
        let existing = match self.get_by_name(name) {
            Ok(e) => Some(e),
            Err(StoreError::NotFound) => None,
            Err(e) => return Err(e),
        };
        let Some(cur) = existing else {
            self.insert(m.clone())?;
            return Ok(Upsert::Inserted);
        };
        let tags = normalize_tags(&m.tags)?;
        let description = m.description.clone().filter(|d| !d.is_empty());
        if cur.title == m.title.trim()
            && cur.body == m.body
            && cur.description == description
            && cur.kind == m.kind
            && cur.tags == tags
            && cur.source == m.source
            && cur.links == m.links
        {
            return Ok(Upsert::Unchanged);
        }
        validate_text(&m.title, &m.body)?;
        let tx = self.conn.transaction()?;
        tx.execute(
            &format!(
                "UPDATE memories SET title = ?2, body = ?3, description = ?4, kind = ?5,
                 tags = ?6, source = ?7, links = ?8, updated_at = {NOW} WHERE id = ?1"
            ),
            params![
                cur.id,
                m.title.trim(),
                m.body,
                description.unwrap_or_default(),
                m.kind,
                json_list(&tags),
                m.source,
                json_list(&m.links),
            ],
        )?;
        if cur.body != m.body {
            tx.execute(
                "UPDATE memories SET vector = NULL, model = NULL, dim = NULL WHERE id = ?1",
                [cur.id],
            )?;
        }
        write_tags(&tx, cur.id, &tags)?;
        tx.commit()?;
        Ok(Upsert::Updated)
    }

    pub fn has_vector(&self, id: i64) -> Result<bool> {
        Ok(self.conn.query_row(
            "SELECT vector IS NOT NULL FROM memories WHERE id = ?1",
            [id],
            |r| r.get(0),
        )?)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Upsert {
    Inserted,
    Updated,
    Unchanged,
}

fn summary_from_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<Summary> {
    Ok(Summary {
        id: r.get(0)?,
        name: r.get(1)?,
        title: r.get(2)?,
        description: non_empty(r.get(3)?),
        kind: r.get(4)?,
        tags: parse_list(&r.get::<_, String>(5)?),
        updated_at: r.get(6)?,
        visibility: r.get(7)?,
        mine: false,
        owner_person: r.get(8)?,
    })
}

/// The memory if `viewer` owns it or it is shared, else "not found".
fn visible_to(mut m: Memory, viewer: i64) -> Result<Memory> {
    if m.owner_person == Some(viewer) {
        m.mine = true;
        Ok(m)
    } else if m.visibility == Visibility::Shared.as_str() {
        Ok(m)
    } else {
        Err(StoreError::NotFound)
    }
}

/// Restrict to the viewer's own memories and shared ones.
fn visible_filter(viewer: Option<i64>, args: &mut Vec<rusqlite::types::Value>) -> String {
    let Some(v) = viewer else {
        return String::new();
    };
    args.push(v.into());
    format!(
        " AND (m.owner_person = ?{} OR m.visibility = 'shared')",
        args.len()
    )
}

/// Restrict to the memories one person wrote (the viewer filter still applies).
fn owner_filter(owner: Option<i64>, args: &mut Vec<rusqlite::types::Value>) -> String {
    let Some(o) = owner else {
        return String::new();
    };
    args.push(o.into());
    format!(" AND m.owner_person = ?{}", args.len())
}

/// Restrict to memories carrying every one of `tags`.
fn tag_filter(tags: &[String], args: &mut Vec<rusqlite::types::Value>) -> String {
    if tags.is_empty() {
        return String::new();
    }
    let mut placeholders = Vec::new();
    for t in tags {
        args.push(t.clone().into());
        placeholders.push(format!("?{}", args.len()));
    }
    format!(
        " AND m.id IN (SELECT memory_id FROM memory_tags WHERE tag IN ({}) GROUP BY memory_id
           HAVING count(DISTINCT tag) = {})",
        placeholders.join(", "),
        tags.len()
    )
}

/// Turns free text into a safe FTS5 expression: every word becomes a quoted
/// term and the terms are OR-ed, so bm25 ranks rows matching more (and rarer)
/// words first. Quoting means user input can never be parsed as FTS syntax.
pub fn fts_query(text: &str) -> Option<String> {
    let terms: Vec<String> = text
        .split(|c: char| !(c.is_alphanumeric() || c == '_'))
        .filter(|w| !w.is_empty())
        .take(32)
        .map(|w| format!("\"{w}\""))
        .collect();
    if terms.is_empty() {
        None
    } else {
        Some(terms.join(" OR "))
    }
}

fn validate_text(title: &str, body: &str) -> Result<()> {
    if title.trim().is_empty() {
        return Err(StoreError::Invalid("title must not be empty".into()));
    }
    if title.chars().count() > MAX_TITLE {
        return Err(StoreError::Invalid(format!(
            "title is longer than {MAX_TITLE} characters"
        )));
    }
    if body.len() > MAX_BODY {
        return Err(StoreError::Invalid(format!(
            "body is larger than {MAX_BODY} bytes"
        )));
    }
    Ok(())
}

pub fn normalize_tags(tags: &[String]) -> Result<Vec<String>> {
    let mut out: Vec<String> = Vec::new();
    for t in tags {
        let t = t.trim().to_lowercase();
        if !t.is_empty() && !out.contains(&t) {
            out.push(t);
        }
    }
    if out.len() > MAX_TAGS {
        return Err(StoreError::Invalid(format!("at most {MAX_TAGS} tags")));
    }
    Ok(out)
}

fn check_vector(v: &[f32]) -> Result<()> {
    if v.is_empty() || v.len() > MAX_DIM {
        return Err(StoreError::Invalid(format!(
            "vector must have 1 to {MAX_DIM} values"
        )));
    }
    if v.iter().any(|x| !x.is_finite()) {
        return Err(StoreError::Invalid(
            "vector values must be finite numbers".into(),
        ));
    }
    Ok(())
}

type EmbeddingCols = (Option<Vec<u8>>, Option<String>, Option<i64>);

fn embedding_columns(e: Option<&Embedding>) -> Result<EmbeddingCols> {
    let Some(e) = e else {
        return Ok((None, None, None));
    };
    check_vector(&e.values)?;
    if e.model.trim().is_empty() {
        return Err(StoreError::Invalid(
            "a vector needs the id of the model that made it".into(),
        ));
    }
    Ok((
        Some(encode_vector(&e.values)),
        Some(e.model.trim().to_owned()),
        Some(e.values.len() as i64),
    ))
}

pub fn encode_vector(v: &[f32]) -> Vec<u8> {
    v.iter().flat_map(|x| x.to_le_bytes()).collect()
}

pub fn decode_vector(b: &[u8]) -> Vec<f32> {
    b.as_chunks::<4>()
        .0
        .iter()
        .map(|c| f32::from_le_bytes(*c))
        .collect()
}

fn norm(v: &[f32]) -> f64 {
    v.iter()
        .map(|x| (*x as f64) * (*x as f64))
        .sum::<f64>()
        .sqrt()
}

fn write_tags(tx: &rusqlite::Transaction<'_>, id: i64, tags: &[String]) -> Result<()> {
    tx.execute("DELETE FROM memory_tags WHERE memory_id = ?1", [id])?;
    for t in tags {
        tx.execute(
            "INSERT INTO memory_tags (memory_id, tag) VALUES (?1, ?2)",
            params![id, t],
        )?;
    }
    Ok(())
}

fn json_list(v: &[String]) -> String {
    serde_json::to_string(v).unwrap_or_else(|_| "[]".into())
}

fn parse_list(s: &str) -> Vec<String> {
    serde_json::from_str(s).unwrap_or_default()
}

fn non_empty(s: String) -> Option<String> {
    if s.is_empty() {
        None
    } else {
        Some(s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mem(title: &str, body: &str, tags: &[&str]) -> NewMemory {
        NewMemory {
            title: title.into(),
            body: body.into(),
            tags: tags.iter().map(|s| s.to_string()).collect(),
            ..Default::default()
        }
    }

    fn with_vec(mut m: NewMemory, v: &[f32]) -> NewMemory {
        m.embedding = Some(Embedding {
            model: "test-model".into(),
            values: v.to_vec(),
        });
        m
    }

    #[test]
    fn keyword_search_finds_and_ranks() {
        let mut s = Store::open_in_memory().unwrap();
        let a = s
            .insert(mem(
                "Gateway restarts",
                "the gateway restarts on config change",
                &[],
            ))
            .unwrap();
        let b = s
            .insert(mem(
                "Plex",
                "media server notes, nothing about gateways",
                &[],
            ))
            .unwrap();
        s.insert(mem("Cooking", "a recipe for jollof rice", &[]))
            .unwrap();
        let hits = s
            .search(&SearchQuery {
                query: "gateway restart".into(),
                k: 10,
                ..Default::default()
            })
            .unwrap();
        let ids: Vec<i64> = hits.iter().map(|h| h.summary.id).collect();
        // Porter stemming matches "restarts" and "gateways"; the title hit ranks first.
        assert_eq!(ids, vec![a, b]);
        assert!(hits[0].snippet.as_deref().unwrap().contains('['));
        assert_eq!(hits[0].matched, vec!["keyword"]);
    }

    #[test]
    fn fts_syntax_in_user_input_is_inert() {
        let mut s = Store::open_in_memory().unwrap();
        s.insert(mem("Quotes", "he said NEAR the end", &[]))
            .unwrap();
        for q in ["\"unbalanced", "NEAR(", "a AND OR", "*", "title:x", "-"] {
            s.search(&SearchQuery {
                query: q.into(),
                k: 5,
                ..Default::default()
            })
            .unwrap();
        }
        assert!(fts_query("  ...  ").is_none());
    }

    #[test]
    fn tag_filter_requires_every_tag() {
        let mut s = Store::open_in_memory().unwrap();
        let both = s.insert(mem("one", "shared word", &["a", "B"])).unwrap();
        s.insert(mem("two", "shared word", &["a"])).unwrap();
        let hits = s
            .search(&SearchQuery {
                query: "shared".into(),
                k: 10,
                tags: vec!["a".into(), "b".into()],
                ..Default::default()
            })
            .unwrap();
        assert_eq!(
            hits.iter().map(|h| h.summary.id).collect::<Vec<_>>(),
            vec![both]
        );
        assert_eq!(s.list(Some("b"), None, 10).unwrap().len(), 1);
        assert_eq!(s.list(Some("a"), None, 10).unwrap().len(), 2);
    }

    #[test]
    fn vector_search_orders_by_cosine_and_fuses_with_keywords() {
        let mut s = Store::open_in_memory().unwrap();
        let near = s
            .insert(with_vec(
                mem("alpha", "unrelated words", &[]),
                &[1.0, 0.0, 0.0],
            ))
            .unwrap();
        let far = s
            .insert(with_vec(
                mem("beta", "unrelated words", &[]),
                &[0.0, 1.0, 0.0],
            ))
            .unwrap();
        let mid = s
            .insert(with_vec(
                mem("gamma", "unrelated words", &[]),
                &[0.7, 0.7, 0.0],
            ))
            .unwrap();
        // Different dimension: never compared.
        s.insert(with_vec(mem("delta", "unrelated", &[]), &[1.0, 0.0]))
            .unwrap();
        // No vector at all: keyword-only.
        let kw = s.insert(mem("zebra facts", "zebra", &[])).unwrap();

        let hits = s
            .search(&SearchQuery {
                query: "".into(),
                k: 10,
                vector: Some(vec![0.9, 0.1, 0.0]),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(
            hits.iter().map(|h| h.summary.id).collect::<Vec<_>>(),
            vec![near, mid, far]
        );

        let hybrid = s
            .search(&SearchQuery {
                query: "zebra gamma".into(),
                k: 10,
                vector: Some(vec![0.9, 0.1, 0.0]),
                ..Default::default()
            })
            .unwrap();
        // gamma is found by both rankers, so it outranks everything else.
        assert_eq!(hybrid[0].summary.id, mid);
        assert_eq!(hybrid[0].matched, vec!["keyword", "vector"]);
        assert!(hybrid.iter().any(|h| h.summary.id == kw));
    }

    #[test]
    fn vector_model_filter_and_validation() {
        let mut s = Store::open_in_memory().unwrap();
        let a = s.insert(with_vec(mem("a", "x", &[]), &[1.0, 0.0])).unwrap();
        let mut other = mem("b", "x", &[]);
        other.embedding = Some(Embedding {
            model: "other".into(),
            values: vec![1.0, 0.0],
        });
        s.insert(other).unwrap();
        let hits = s
            .search(&SearchQuery {
                query: String::new(),
                k: 10,
                vector: Some(vec![1.0, 0.0]),
                model: Some("test-model".into()),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(
            hits.iter().map(|h| h.summary.id).collect::<Vec<_>>(),
            vec![a]
        );

        let bad = with_vec(mem("c", "x", &[]), &[f32::NAN]);
        assert!(matches!(s.insert(bad), Err(StoreError::Invalid(_))));
        let mut no_model = mem("d", "x", &[]);
        no_model.embedding = Some(Embedding {
            model: " ".into(),
            values: vec![1.0],
        });
        assert!(matches!(s.insert(no_model), Err(StoreError::Invalid(_))));
        assert!(s
            .search(&SearchQuery {
                vector: Some(vec![0.0, 0.0]),
                k: 1,
                ..Default::default()
            })
            .is_err());
    }

    #[test]
    fn vector_roundtrip_is_exact() {
        let v = vec![0.1f32, -2.5, 3.25e-7];
        assert_eq!(decode_vector(&encode_vector(&v)), v);
    }

    #[test]
    fn update_reindexes_and_drops_stale_vector() {
        let mut s = Store::open_in_memory().unwrap();
        let id = s
            .insert(with_vec(mem("t", "old words", &["x"]), &[1.0]))
            .unwrap();
        assert!(s.has_vector(id).unwrap());
        s.update(
            id,
            MemoryPatch {
                body: Some("new words".into()),
                ..Default::default()
            },
        )
        .unwrap();
        assert!(!s.has_vector(id).unwrap());
        let q = |s: &Store, w: &str| {
            s.search(&SearchQuery {
                query: w.into(),
                k: 5,
                ..Default::default()
            })
            .unwrap()
            .len()
        };
        assert_eq!(q(&s, "old"), 0);
        assert_eq!(q(&s, "new"), 1);
        // Tags are kept when the patch leaves them out.
        assert_eq!(s.get(id).unwrap().tags, vec!["x"]);
        s.delete(id).unwrap();
        assert_eq!(q(&s, "new"), 0);
        assert!(matches!(s.get(id), Err(StoreError::NotFound)));
        assert!(matches!(s.delete(id), Err(StoreError::NotFound)));
    }

    #[test]
    fn list_since_and_limit() {
        let mut s = Store::open_in_memory().unwrap();
        for i in 0..5 {
            s.insert(mem(&format!("m{i}"), "b", &[])).unwrap();
        }
        assert_eq!(s.list(None, None, 3).unwrap().len(), 3);
        assert_eq!(s.list(None, Some("2000-01-01"), 100).unwrap().len(), 5);
        assert_eq!(s.list(None, Some("2999-01-01"), 100).unwrap().len(), 0);
        // A date or UTC timestamp in any of the ISO shapes is read; anything else is refused
        // rather than compared as text (where "yesterday" sorts after every real date).
        for ok in [
            "2000-01-01T00:00",
            "2000-01-01T00:00:00Z",
            "2000-01-01 12:30:59.123Z",
            " 2000-01-01 ",
        ] {
            assert!(s.list(None, Some(ok), 100).is_ok(), "{ok:?}");
        }
        for bad in [
            "yesterday",
            "",
            "2000-1-1",
            "2000-13-01",
            "2000-01-32",
            "2000-01-01T24:00",
            "2000-01-01T10",
            "2000-01-01T10:00:00.Z",
            "2000-01-01T10:00:00+01:00",
            "2000-01-01x",
        ] {
            assert!(
                matches!(s.list(None, Some(bad), 100), Err(StoreError::Invalid(_))),
                "{bad:?}"
            );
        }
        // Newest first.
        assert_eq!(s.list(None, None, 1).unwrap()[0].title, "m4");
    }

    #[test]
    fn upsert_keeps_vector_unless_the_body_changes() {
        let mut s = Store::open_in_memory().unwrap();
        let mut m = with_vec(mem("t", "body", &[]), &[1.0, 2.0]);
        m.name = Some("n".into());
        assert_eq!(s.upsert_named(&m).unwrap(), Upsert::Inserted);
        let id = s.get_by_name("n").unwrap().id;
        m.embedding = None;
        assert_eq!(s.upsert_named(&m).unwrap(), Upsert::Unchanged);
        m.title = "t2".into();
        assert_eq!(s.upsert_named(&m).unwrap(), Upsert::Updated);
        assert!(s.has_vector(id).unwrap(), "a title change keeps the vector");
        m.body = "new body".into();
        assert_eq!(s.upsert_named(&m).unwrap(), Upsert::Updated);
        assert!(
            !s.has_vector(id).unwrap(),
            "a body change drops the stale vector"
        );
    }

    #[test]
    fn owners_see_their_own_and_shared_memories_and_old_ones_wait_for_adoption() {
        let mut s = Store::open_in_memory().unwrap();
        // A memory stored with no owner is invisible to every viewer until someone adopts it.
        let old = s.insert(mem("old", "legacy words", &[])).unwrap();
        let q = |s: &Store, viewer| {
            s.search(&SearchQuery {
                query: "words".into(),
                k: 10,
                viewer,
                ..Default::default()
            })
            .unwrap()
            .len()
        };
        assert_eq!((q(&s, None), q(&s, Some(1)), q(&s, Some(2))), (1, 0, 0));
        assert_eq!(s.adopt_orphans(1).unwrap(), 1);
        assert_eq!((q(&s, Some(1)), q(&s, Some(2))), (1, 0));

        // Shared is seen by everyone, and marked as theirs only for its owner.
        let mut m = mem("shared one", "more words", &[]);
        m.owner = Some(2);
        m.visibility = Visibility::Shared;
        let shared = s.insert(m).unwrap();
        assert_eq!((q(&s, Some(1)), q(&s, Some(2))), (2, 1));
        assert!(matches!(s.get_visible(old, 2), Err(StoreError::NotFound)));
        assert!(!s.get_visible(shared, 1).unwrap().mine);
        assert!(s.get_visible(shared, 2).unwrap().mine);
        let listed = s.list_for(Some(1), None, None, 10).unwrap();
        assert_eq!(listed.iter().filter(|m| m.mine).count(), 1);

        // A store default fills in the owner for writers that name none.
        s.set_default_owner(Some(3));
        let id = s.insert(mem("d", "words", &[])).unwrap();
        assert_eq!(s.get(id).unwrap().owner_person, Some(3));
    }

    #[test]
    fn a_store_from_before_owners_gains_the_columns_and_keeps_its_rows() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE memories (
                id INTEGER PRIMARY KEY, name TEXT UNIQUE, title TEXT NOT NULL,
                description TEXT NOT NULL DEFAULT '', kind TEXT, body TEXT NOT NULL,
                tags TEXT NOT NULL DEFAULT '[]', source TEXT, links TEXT NOT NULL DEFAULT '[]',
                vector BLOB, model TEXT, dim INTEGER, created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL);
             INSERT INTO memories (title, body, created_at, updated_at)
                VALUES ('kept', 'body', '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z');",
        )
        .unwrap();
        let mut s = Store::init(conn).unwrap();
        assert_eq!(s.adopt_orphans(7).unwrap(), 1);
        let m = s.get_visible(1, 7).unwrap();
        assert_eq!(
            (m.title.as_str(), m.visibility.as_str()),
            ("kept", "private")
        );
        assert!(matches!(s.get_visible(1, 8), Err(StoreError::NotFound)));
    }

    #[test]
    fn duplicate_name_is_refused() {
        let mut s = Store::open_in_memory().unwrap();
        let mut m = mem("t", "b", &[]);
        m.name = Some("n".into());
        s.insert(m.clone()).unwrap();
        assert!(matches!(s.insert(m), Err(StoreError::Invalid(_))));
    }
}
