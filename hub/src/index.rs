//! The search index: documents made of chunks, beside the memories table.
//!
//! Every searchable thing is a *doc* (a note file, a chat or coding session, a memory) made of
//! *chunks*. Curated memories stay in `memories` and are mirrored here as docs of the `memory`
//! source, in the same transaction as every write, so one search runs over chunks only. All the
//! tables are prefixed `ix_`; opening a store creates them and mirrors any memory that has no
//! mirror yet, so an older database upgrades in place and opening it twice changes nothing.
//!
//! **Privacy.** The household shares one memory: a doc is shared with everyone unless its writer
//! made it private on purpose. A viewer sees the docs they own, the ones shared with everyone, and
//! the ones that list them as a reader (`some`, in `ix_readers`). The filter is part of every
//! candidate query, keyword and vector alike, so a private doc is never a candidate for anyone but
//! its owner, let alone ranked. Who asks only changes the order (a person's own material ranks
//! first, see `rank.rs`), never what is found.
//!
//! **Vectors.** A chunk's vector is stored as int8 (a scale plus one signed byte per dimension)
//! in `ix_vectors`, keyed by the model's normalised name. At start the store builds a table of
//! sign-bit codes in RAM (one bit per dimension); a search scans the codes of the allowed docs by
//! Hamming distance, then re-scores the closest 200 exactly from the int8 vectors. Nothing here
//! computes an embedding: the PC embeds documents and `embed.rs` embeds queries.
//!
//! **Keywords.** `ix_chunks_fts` is a contentless FTS5 table filled by this module, not by
//! triggers: its columns are the doc's title, the chunk's heading and its text, weighted 8, 4 and
//! 1. A contentless table cannot cut snippets, so snippets are cut from the chunk text.
//!
//! **Machines write through frames** (`ix_hello`, `ix_manifest`, `ix_put`, `ix_delete`,
//! `ix_pending`, `ix_vectors`; see `relay.rs`). A source belongs to the first machine that names
//! it; a doc's owner must be the machine's own person or one it may act for. A put replaces all of
//! a doc's chunks in one transaction, keeping the vector of any chunk whose text is unchanged.

use crate::rank::{self, Cand, Context};
use crate::store::{fts_query, Store};
use base64::Engine as _;
use rusqlite::types::Value as Sql;
use rusqlite::{params, params_from_iter, Connection, OptionalExtension};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

pub const MEMORY_SOURCE: &str = "memory";
/// What the welcome frame says this hub can do.
pub const FEATURES: &[&str] = &["index"];
/// The most chunks one `ix_put` carries, and the most bytes of chunk text.
pub const MAX_PUT_CHUNKS: usize = 200;
pub const MAX_PUT_BYTES: usize = 2 * 1024 * 1024;
/// A chunk's text is cut by the machine that sends it at about 2 KB; this is the refusal point.
pub const MAX_CHUNK_TEXT: usize = 8 * 1024;
const MAX_KEY: usize = 512;
const MAX_LOCATOR: usize = 1024;
const MAX_TITLE: usize = 500;
const MAX_VECTOR_DIM: usize = 4096;
/// Candidates each ranker passes on to the fusion.
const CANDIDATES: usize = 100;
/// Hamming-closest codes that are re-scored exactly.
const RESCORE: usize = 200;
/// A search with fewer matching docs than this widens its date window once.
const MIN_DOCS: usize = 3;
const SNIPPET_CHARS: usize = 300;
const NOW: &str = "strftime('%Y-%m-%dT%H:%M:%fZ','now')";

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS ix_sources (
  id TEXT PRIMARY KEY,
  kind TEXT NOT NULL,
  name TEXT NOT NULL,
  device_uid TEXT,
  owner_person INTEGER,
  updated_at TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS ix_docs (
  id INTEGER PRIMARY KEY,
  source_id TEXT NOT NULL REFERENCES ix_sources(id) ON DELETE CASCADE,
  key TEXT NOT NULL,
  hash TEXT NOT NULL,
  title TEXT NOT NULL,
  locator TEXT NOT NULL,
  tier TEXT NOT NULL,
  owner_person INTEGER,
  visibility TEXT NOT NULL DEFAULT 'private',
  occurred_from INTEGER,
  occurred_to INTEGER,
  updated_at TEXT NOT NULL,
  UNIQUE (source_id, key));
CREATE INDEX IF NOT EXISTS ix_docs_owner ON ix_docs(owner_person, visibility);
CREATE TABLE IF NOT EXISTS ix_readers (
  doc_id INTEGER NOT NULL REFERENCES ix_docs(id) ON DELETE CASCADE,
  person_id INTEGER NOT NULL,
  PRIMARY KEY (doc_id, person_id));
CREATE TABLE IF NOT EXISTS ix_chunks (
  id INTEGER PRIMARY KEY,
  doc_id INTEGER NOT NULL REFERENCES ix_docs(id) ON DELETE CASCADE,
  ord INTEGER NOT NULL,
  heading TEXT NOT NULL DEFAULT '',
  text TEXT NOT NULL,
  occurred_at INTEGER,
  anchor TEXT,
  UNIQUE (doc_id, ord));
CREATE INDEX IF NOT EXISTS ix_chunks_time ON ix_chunks(occurred_at);
CREATE VIRTUAL TABLE IF NOT EXISTS ix_chunks_fts USING fts5(
  title, heading, text, content='', contentless_delete=1, tokenize='porter unicode61');
CREATE TABLE IF NOT EXISTS ix_vectors (
  chunk_id INTEGER PRIMARY KEY REFERENCES ix_chunks(id) ON DELETE CASCADE,
  model TEXT NOT NULL,
  dim INTEGER NOT NULL,
  scale REAL NOT NULL,
  v BLOB NOT NULL);
";

/// Make the tables and mirror every memory that has no mirror yet.
pub fn init(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(SCHEMA)?;
    conn.execute(
        &format!(
            "INSERT OR IGNORE INTO ix_sources (id, kind, name, updated_at)
             VALUES ('{MEMORY_SOURCE}', 'memory', 'Memories', {NOW})"
        ),
        [],
    )?;
    let missing: Vec<i64> = conn
        .prepare(
            "SELECT m.id FROM memories m
             WHERE NOT EXISTS (SELECT 1 FROM ix_docs d
                 WHERE d.source_id = 'memory' AND d.key = CAST(m.id AS TEXT))
             ORDER BY m.id",
        )?
        .query_map([], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    if !missing.is_empty() {
        let tx = conn.unchecked_transaction()?;
        for id in &missing {
            mirror_memory(&tx, *id)?;
        }
        tx.commit()?;
    }
    Ok(())
}

// ---- vectors ----

/// A chunk's place in the RAM table.
#[derive(Debug, Clone)]
pub(crate) struct Code {
    doc: i64,
    /// When the passage happened, for the date window (0 when unknown).
    at: i64,
    dim: usize,
    model: Arc<str>,
    bits: Box<[u8]>,
}

/// The sign-bit codes of every vector, by chunk id.
#[derive(Default)]
pub struct Codes {
    map: HashMap<i64, Code>,
    models: HashMap<String, Arc<str>>,
}

/// What a write changed in the RAM table, applied once the transaction has committed.
#[derive(Default)]
pub struct Delta {
    removed: Vec<i64>,
    added: Vec<(i64, Code)>,
}

impl Codes {
    pub fn load(conn: &Connection) -> rusqlite::Result<Self> {
        let mut codes = Codes::default();
        let mut stmt = conn.prepare(
            "SELECT v.chunk_id, c.doc_id, COALESCE(c.occurred_at, d.occurred_to, 0), v.model, v.v
             FROM ix_vectors v JOIN ix_chunks c ON c.id = v.chunk_id JOIN ix_docs d ON d.id = c.doc_id",
        )?;
        let mut rows = stmt.query([])?;
        while let Some(r) = rows.next()? {
            let q: Vec<u8> = r.get(4)?;
            let code = codes.code(r.get(1)?, r.get(2)?, &r.get::<_, String>(3)?, &q);
            codes.map.insert(r.get(0)?, code);
        }
        Ok(codes)
    }

    fn code(&mut self, doc: i64, at: i64, model: &str, q: &[u8]) -> Code {
        let model = self
            .models
            .entry(model.to_owned())
            .or_insert_with(|| Arc::from(model))
            .clone();
        Code {
            doc,
            at,
            dim: q.len(),
            model,
            bits: sign_bits(q),
        }
    }

    pub fn apply(&mut self, d: Delta) {
        for id in d.removed {
            self.map.remove(&id);
        }
        for (id, code) in d.added {
            self.map.insert(id, code);
        }
    }

    pub fn len(&self) -> usize {
        self.map.len()
    }

    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }
}

/// The model id as stored and compared: the registry's `:tag` dropped (`nomic-embed-text:latest`
/// is `nomic-embed-text`).
pub fn normalise_model(model: &str) -> String {
    model.split(':').next().unwrap_or("").trim().to_owned()
}

/// Scale and signed bytes (as `u8`) for `v`, so that `byte as i8 * scale` is close to the value.
pub fn quantize(v: &[f32]) -> (f32, Vec<u8>) {
    let max = v.iter().fold(0f32, |m, x| m.max(x.abs()));
    if max == 0.0 {
        return (1.0, vec![0; v.len()]);
    }
    let scale = max / 127.0;
    (
        scale,
        v.iter()
            .map(|x| ((x / scale).round().clamp(-127.0, 127.0) as i8) as u8)
            .collect(),
    )
}

fn sign_bits(q: &[u8]) -> Box<[u8]> {
    let mut bits = vec![0u8; q.len().div_ceil(8)];
    for (i, b) in q.iter().enumerate() {
        if (*b as i8) > 0 {
            bits[i / 8] |= 1 << (i % 8);
        }
    }
    bits.into_boxed_slice()
}

fn query_bits(v: &[f32]) -> Box<[u8]> {
    let mut bits = vec![0u8; v.len().div_ceil(8)];
    for (i, x) in v.iter().enumerate() {
        if *x > 0.0 {
            bits[i / 8] |= 1 << (i % 8);
        }
    }
    bits.into_boxed_slice()
}

fn hamming(a: &[u8], b: &[u8]) -> u32 {
    a.iter().zip(b).map(|(x, y)| (x ^ y).count_ones()).sum()
}

fn decode_f32(blob: &[u8]) -> Vec<f32> {
    crate::store::decode_vector(blob)
}

/// The RAM code of a stored chunk vector, read back from the database.
fn code_of(conn: &Connection, codes: &mut Codes, chunk: i64) -> rusqlite::Result<Option<Code>> {
    let row: Option<(i64, i64, String, Vec<u8>)> = conn
        .query_row(
            "SELECT c.doc_id, COALESCE(c.occurred_at, d.occurred_to, 0), v.model, v.v
             FROM ix_vectors v JOIN ix_chunks c ON c.id = v.chunk_id JOIN ix_docs d ON d.id = c.doc_id
             WHERE v.chunk_id = ?1",
            [chunk],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .optional()?;
    Ok(row.map(|(doc, at, model, q)| codes.code(doc, at, &model, &q)))
}

// ---- writing docs ----

pub struct VecIn {
    pub model: String,
    pub scale: f32,
    pub q: Vec<u8>,
}

pub struct ChunkIn {
    pub ord: i64,
    pub heading: String,
    pub text: String,
    pub at: Option<i64>,
    pub anchor: Option<String>,
    pub vec: Option<VecIn>,
}

pub struct DocIn {
    pub source: String,
    pub key: String,
    pub hash: String,
    pub title: String,
    /// What the keyword index matches as the title (a memory's tags ride along).
    pub fts_title: String,
    pub locator: String,
    pub tier: String,
    pub owner: Option<i64>,
    pub visibility: String,
    pub readers: Vec<i64>,
    pub from: Option<i64>,
    pub to: Option<i64>,
    pub chunks: Vec<ChunkIn>,
}

fn fts_set(
    conn: &Connection,
    chunk: i64,
    fts_title: &str,
    heading: &str,
    text: &str,
) -> rusqlite::Result<()> {
    conn.execute("DELETE FROM ix_chunks_fts WHERE rowid = ?1", [chunk])?;
    conn.execute(
        "INSERT INTO ix_chunks_fts (rowid, title, heading, text) VALUES (?1, ?2, ?3, ?4)",
        params![chunk, fts_title, heading, text],
    )?;
    Ok(())
}

/// Store a doc, replacing the chunks it had. A chunk whose text is unchanged keeps its row and its
/// vector. Returns the doc's id and what changed in the RAM table; the caller applies that once
/// the surrounding transaction has committed.
pub fn write_doc(
    conn: &Connection,
    codes: &mut Codes,
    d: &DocIn,
) -> rusqlite::Result<(i64, Delta)> {
    let doc: i64 = conn.query_row(
        &format!(
            "INSERT INTO ix_docs (source_id, key, hash, title, locator, tier, owner_person,
                 visibility, occurred_from, occurred_to, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, {NOW})
             ON CONFLICT(source_id, key) DO UPDATE SET hash = excluded.hash, title = excluded.title,
                 locator = excluded.locator, tier = excluded.tier,
                 owner_person = excluded.owner_person, visibility = excluded.visibility,
                 occurred_from = excluded.occurred_from, occurred_to = excluded.occurred_to,
                 updated_at = excluded.updated_at
             RETURNING id"
        ),
        params![
            d.source,
            d.key,
            d.hash,
            d.title,
            d.locator,
            d.tier,
            d.owner,
            d.visibility,
            d.from,
            d.to
        ],
        |r| r.get(0),
    )?;
    conn.execute("DELETE FROM ix_readers WHERE doc_id = ?1", [doc])?;
    for p in &d.readers {
        conn.execute(
            "INSERT OR IGNORE INTO ix_readers (doc_id, person_id) VALUES (?1, ?2)",
            params![doc, p],
        )?;
    }
    let mut old: HashMap<i64, (i64, String)> = conn
        .prepare("SELECT ord, id, text FROM ix_chunks WHERE doc_id = ?1")?
        .query_map([doc], |r| Ok((r.get(0)?, (r.get(1)?, r.get(2)?))))?
        .collect::<rusqlite::Result<_>>()?;
    let mut delta = Delta {
        removed: old.values().map(|(id, _)| *id).collect(),
        added: Vec::new(),
    };
    let mut live: Vec<i64> = Vec::new();
    for c in &d.chunks {
        let id = match old.remove(&c.ord) {
            Some((id, text)) if text == c.text => {
                conn.execute(
                    "UPDATE ix_chunks SET heading = ?2, occurred_at = ?3, anchor = ?4 WHERE id = ?1",
                    params![id, c.heading, c.at, c.anchor],
                )?;
                id
            }
            replaced => {
                if let Some((id, _)) = replaced {
                    conn.execute("DELETE FROM ix_chunks_fts WHERE rowid = ?1", [id])?;
                    conn.execute("DELETE FROM ix_chunks WHERE id = ?1", [id])?;
                }
                conn.execute(
                    "INSERT INTO ix_chunks (doc_id, ord, heading, text, occurred_at, anchor)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                    params![doc, c.ord, c.heading, c.text, c.at, c.anchor],
                )?;
                conn.last_insert_rowid()
            }
        };
        fts_set(conn, id, &d.fts_title, &c.heading, &c.text)?;
        if let Some(v) = &c.vec {
            conn.execute(
                "INSERT OR REPLACE INTO ix_vectors (chunk_id, model, dim, scale, v)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![id, v.model, v.q.len() as i64, f64::from(v.scale), v.q],
            )?;
        }
        live.push(id);
    }
    for (_, (id, _)) in old {
        conn.execute("DELETE FROM ix_chunks_fts WHERE rowid = ?1", [id])?;
        conn.execute("DELETE FROM ix_chunks WHERE id = ?1", [id])?;
    }
    for id in live {
        if let Some(code) = code_of(conn, codes, id)? {
            delta.added.push((id, code));
        }
    }
    Ok((doc, delta))
}

/// Remove a doc and everything of it.
pub fn delete_doc(conn: &Connection, doc: i64) -> rusqlite::Result<Delta> {
    let ids: Vec<i64> = conn
        .prepare("SELECT id FROM ix_chunks WHERE doc_id = ?1")?
        .query_map([doc], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    for id in &ids {
        conn.execute("DELETE FROM ix_chunks_fts WHERE rowid = ?1", [id])?;
    }
    conn.execute("DELETE FROM ix_docs WHERE id = ?1", [doc])?;
    Ok(Delta {
        removed: ids,
        added: Vec::new(),
    })
}

// ---- the memory mirror ----

const TARGET_CHUNK: usize = 1500;
const HARD_CHUNK: usize = 2000;

/// A markdown heading of level 1 to 3: its text.
fn heading_of(line: &str) -> Option<&str> {
    let hashes = line.bytes().take_while(|b| *b == b'#').count();
    let rest = line.get(hashes..)?;
    ((1..=3).contains(&hashes) && rest.starts_with(' ')).then(|| rest.trim())
}

/// Cut `body` (after the description) into chunks of about 1.5 KB, never over 2 KB, splitting at
/// headings and then at blank lines. Each chunk carries the heading it sits under.
pub fn chunk_text(description: &str, body: &str, fallback: &str) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    let (mut heading, mut cur) = (String::new(), String::new());
    let flush = |out: &mut Vec<(String, String)>, heading: &str, cur: &mut String| {
        let t = cur.trim();
        if !t.is_empty() {
            out.push((heading.to_owned(), t.to_owned()));
        }
        cur.clear();
    };
    let full = if description.is_empty() {
        body.to_owned()
    } else {
        format!("{description}\n\n{body}")
    };
    for line in full.lines() {
        if let Some(h) = heading_of(line) {
            flush(&mut out, &heading, &mut cur);
            heading = h.to_owned();
        } else if line.trim().is_empty() && cur.len() >= TARGET_CHUNK {
            flush(&mut out, &heading, &mut cur);
            continue;
        }
        let mut rest = line;
        while cur.len() + rest.len() + 1 > HARD_CHUNK {
            let room = HARD_CHUNK.saturating_sub(cur.len() + 1).max(1);
            let mut cut = room.min(rest.len());
            while !rest.is_char_boundary(cut) {
                cut -= 1;
            }
            if let Some(ws) = rest[..cut]
                .rfind(char::is_whitespace)
                .filter(|w| *w > cut / 2)
            {
                cut = ws;
            }
            if cut == 0 {
                flush(&mut out, &heading, &mut cur);
                cut = rest.chars().next().map_or(rest.len(), char::len_utf8);
            }
            cur.push_str(&rest[..cut]);
            cur.push('\n');
            flush(&mut out, &heading, &mut cur);
            rest = rest[cut..].trim_start();
        }
        cur.push_str(rest);
        cur.push('\n');
    }
    flush(&mut out, &heading, &mut cur);
    if out.is_empty() {
        out.push((String::new(), fallback.to_owned()));
    }
    out
}

/// A memory row as the mirror needs it: title, description, body, tags, owner,
/// visibility, vector, model, dim, and the update time in milliseconds.
type MemoryRow = (
    String,
    String,
    String,
    String,
    Option<i64>,
    String,
    Option<Vec<u8>>,
    Option<String>,
    Option<i64>,
    Option<i64>,
);

/// Write the mirror doc of memory `id` (or remove it, if the memory is gone).
pub fn mirror_memory(conn: &Connection, id: i64) -> rusqlite::Result<Delta> {
    let row: Option<MemoryRow> = conn
        .query_row(
            "SELECT title, description, body, tags, owner_person, visibility, vector, model, dim,
                    CAST(strftime('%s', updated_at) AS INTEGER) * 1000
             FROM memories WHERE id = ?1",
            [id],
            |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                    r.get(6)?,
                    r.get(7)?,
                    r.get(8)?,
                    r.get(9)?,
                ))
            },
        )
        .optional()?;
    let key = id.to_string();
    let Some((title, description, body, tags, owner, visibility, vector, model, dim, at)) = row
    else {
        let doc: Option<i64> = conn
            .query_row(
                "SELECT id FROM ix_docs WHERE source_id = 'memory' AND key = ?1",
                [&key],
                |r| r.get(0),
            )
            .optional()?;
        return doc.map_or(Ok(Delta::default()), |d| delete_doc(conn, d));
    };
    let tags: Vec<String> = serde_json::from_str(&tags).unwrap_or_default();
    let pieces = chunk_text(&description, &body, &title);
    // A vector a caller computed for the whole memory belongs to its one chunk.
    let legacy = match (&vector, &model, dim, pieces.len()) {
        (Some(blob), Some(model), Some(dim), 1) if dim > 0 && blob.len() == dim as usize * 4 => {
            let (scale, q) = quantize(&decode_f32(blob));
            Some(VecIn {
                model: normalise_model(model),
                scale,
                q,
            })
        }
        _ => None,
    };
    let mut legacy = legacy;
    let chunks = pieces
        .into_iter()
        .enumerate()
        .map(|(i, (heading, text))| ChunkIn {
            ord: i as i64,
            heading,
            text,
            at,
            anchor: None,
            vec: legacy.take(),
        })
        .collect();
    let hash = format!(
        "{:x}",
        Sha256::digest(format!("{title}\0{description}\0{body}\0{}", tags.join(",")).as_bytes())
    );
    // The codes are not touched here (a mirror write's vector, if any, is reloaded from the
    // database), so a throwaway table is enough for `write_doc` to build them from.
    let mut scratch = Codes::default();
    let (_, delta) = write_doc(
        conn,
        &mut scratch,
        &DocIn {
            source: MEMORY_SOURCE.into(),
            key,
            hash,
            fts_title: format!("{title} {}", tags.join(" ")),
            title,
            locator: format!("memory:{id}"),
            tier: "memory".into(),
            owner,
            visibility,
            readers: Vec::new(),
            from: at,
            to: at,
            chunks,
        },
    )?;
    Ok(delta)
}

/// Memory ownership moved (`person merge`) or was adopted: the mirror follows.
pub(crate) fn move_owner(conn: &Connection, from: i64, into: i64) -> rusqlite::Result<()> {
    let present: bool = conn
        .prepare("SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'ix_docs'")?
        .exists([])?;
    if !present {
        return Ok(());
    }
    conn.execute(
        "UPDATE ix_docs SET owner_person = ?2 WHERE owner_person = ?1",
        params![from, into],
    )?;
    conn.execute(
        "UPDATE ix_sources SET owner_person = ?2 WHERE owner_person = ?1",
        params![from, into],
    )?;
    conn.execute(
        "UPDATE OR IGNORE ix_readers SET person_id = ?2 WHERE person_id = ?1",
        params![from, into],
    )?;
    conn.execute("DELETE FROM ix_readers WHERE person_id = ?1", [from])?;
    Ok(())
}

// ---- search ----

/// A `?n` placeholder list that remembers its values.
#[derive(Default)]
struct Args(Vec<Sql>);

impl Args {
    fn push(&mut self, v: impl Into<Sql>) -> usize {
        self.0.push(v.into());
        self.0.len()
    }
}

/// `(d.owner_person IN (...) OR d.visibility = 'shared' OR a reader)`: the docs `persons` may read.
fn privacy(persons: &[i64], args: &mut Args) -> String {
    let marks: Vec<String> = persons
        .iter()
        .map(|p| format!("?{}", args.push(*p)))
        .collect();
    let list = marks.join(", ");
    format!(
        "(d.owner_person IN ({list}) OR d.visibility = 'shared' OR (d.visibility = 'some' AND \
         EXISTS (SELECT 1 FROM ix_readers r WHERE r.doc_id = d.id AND r.person_id IN ({list}))))"
    )
}

/// `(d.owner_person IN (...) OR d.source_id IN <this machine's sources>)`: the docs a machine
/// may write vectors for. A shared doc someone else owns is readable by everyone, but only its
/// owner's machine (or the machine it came from) may change how it ranks.
fn embeddable(ctx: &Ctx, args: &mut Args) -> String {
    let marks: Vec<String> = ctx
        .acts_for
        .iter()
        .map(|p| format!("?{}", args.push(*p)))
        .collect();
    let dev = args.push(ctx.device.clone());
    format!(
        "(d.owner_person IN ({}) OR d.source_id IN (SELECT id FROM ix_sources WHERE device_uid = ?{dev}))",
        marks.join(", ")
    )
}

/// What a search asks for. `viewer` is who is asking: of anyone else's private docs only the
/// ones of a person it names (`person`) are seen.
#[derive(Debug, Clone, Default)]
pub struct Query {
    pub viewer: i64,
    pub text: String,
    pub k: usize,
    pub vector: Option<Vec<f32>>,
    pub model: Option<String>,
    pub tags: Vec<String>,
    /// Only this person's docs (among those the viewer may see).
    pub person: Option<i64>,
    /// Whose records the words of the query ask for ("my flight", "Ben's flight"); ranking only.
    pub focus: Option<crate::who::Focus>,
    /// Which tiers: any of `memory`, `note`, `conversation`.
    pub tiers: Vec<&'static str>,
    /// A source id or name.
    pub folder: Option<String>,
    /// One doc: `d:<id>`, a bare id, or the doc's own key.
    pub chat: Option<String>,
    /// A date window in milliseconds, both ends included.
    pub window: Option<(i64, i64)>,
    pub now_ms: i64,
}

#[derive(Debug, Clone)]
pub struct Hit {
    /// `memory`, `note` or `chat`.
    pub kind: &'static str,
    pub doc: i64,
    pub ord: i64,
    pub source: String,
    pub key: String,
    pub title: String,
    pub locator: String,
    pub owner: Option<i64>,
    pub at: Option<i64>,
    pub snippet: String,
    pub score: f64,
    pub keyword: bool,
    pub vector: bool,
}

impl Hit {
    /// `m:<memory id>` for a memory, else `d:<doc>#<chunk number>`.
    pub fn reference(&self) -> String {
        if self.source == MEMORY_SOURCE {
            format!("m:{}", self.key)
        } else {
            format!("d:{}#{}", self.doc, self.ord)
        }
    }

    pub fn memory_id(&self) -> Option<i64> {
        (self.source == MEMORY_SOURCE)
            .then(|| self.key.parse().ok())
            .flatten()
    }
}

#[derive(Debug, Default)]
pub struct Found {
    pub hits: Vec<Hit>,
    /// The date window was widened because too few docs matched.
    pub widened: bool,
}

/// The conditions every candidate query shares, as ` AND ...` (the doc table is `d`).
fn doc_conditions(q: &Query, args: &mut Args) -> String {
    // Naming another person reaches what they wrote, private docs included.
    let mut readers = vec![q.viewer];
    readers.extend(q.person.filter(|p| *p != q.viewer));
    let mut sql = format!(" AND {}", privacy(&readers, args));
    if !q.tiers.is_empty() {
        let marks: Vec<String> = q
            .tiers
            .iter()
            .map(|t| format!("?{}", args.push((*t).to_owned())))
            .collect();
        sql.push_str(&format!(" AND d.tier IN ({})", marks.join(", ")));
    }
    if let Some(f) = &q.folder {
        let n = args.push(f.clone());
        sql.push_str(&format!(
            " AND (d.source_id = ?{n} OR d.source_id IN (SELECT id FROM ix_sources WHERE name = ?{n}))"
        ));
    }
    if let Some(c) = &q.chat {
        let raw = c.strip_prefix("d:").unwrap_or(c);
        let raw = raw.split('#').next().unwrap_or(raw);
        match raw.parse::<i64>() {
            Ok(id) => sql.push_str(&format!(" AND d.id = ?{}", args.push(id))),
            Err(_) => sql.push_str(&format!(" AND d.key = ?{}", args.push(raw.to_owned()))),
        }
    }
    if let Some(p) = q.person {
        sql.push_str(&format!(" AND d.owner_person = ?{}", args.push(p)));
    }
    if !q.tags.is_empty() {
        let marks: Vec<String> = q
            .tags
            .iter()
            .map(|t| format!("?{}", args.push(t.clone())))
            .collect();
        sql.push_str(&format!(
            " AND d.source_id = 'memory' AND CAST(d.key AS INTEGER) IN (SELECT memory_id
               FROM memory_tags WHERE tag IN ({}) GROUP BY memory_id
               HAVING count(DISTINCT tag) = {})",
            marks.join(", "),
            q.tags.len()
        ));
    }
    sql
}

/// Keyword candidates, best first: `(chunk, doc)`.
fn keyword_candidates(
    conn: &Connection,
    q: &Query,
    window: Option<(i64, i64)>,
) -> rusqlite::Result<Vec<(i64, i64)>> {
    let Some(expr) = fts_query(&q.text) else {
        return Ok(Vec::new());
    };
    let mut args = Args::default();
    args.push(expr);
    let mut sql = String::from(
        "SELECT c.id, c.doc_id FROM ix_chunks_fts
         JOIN ix_chunks c ON c.id = ix_chunks_fts.rowid JOIN ix_docs d ON d.id = c.doc_id
         WHERE ix_chunks_fts MATCH ?1",
    );
    sql.push_str(&doc_conditions(q, &mut args));
    if let Some((lo, hi)) = window {
        let (a, b) = (args.push(lo), args.push(hi));
        sql.push_str(&format!(
            " AND COALESCE(c.occurred_at, d.occurred_to, 0) BETWEEN ?{a} AND ?{b}"
        ));
    }
    sql.push_str(&format!(
        " ORDER BY bm25(ix_chunks_fts, 8.0, 4.0, 1.0) LIMIT {CANDIDATES}"
    ));
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(params_from_iter(args.0), |r| Ok((r.get(0)?, r.get(1)?)))?;
    rows.collect()
}

/// Vector candidates, best first: `(chunk, cosine, unit vector)`.
fn vector_candidates(
    store: &Store,
    q: &Query,
    vector: &[f32],
    window: Option<(i64, i64)>,
) -> rusqlite::Result<Vec<(i64, f64, Vec<f32>)>> {
    let qnorm = vector
        .iter()
        .map(|x| f64::from(*x) * f64::from(*x))
        .sum::<f64>()
        .sqrt();
    if qnorm == 0.0 || vector.iter().any(|x| !x.is_finite()) {
        return Ok(Vec::new());
    }
    let conn = store.conn();
    // The docs this viewer may search: the filter is applied before any code is looked at.
    let mut args = Args::default();
    let sql = format!(
        "SELECT d.id FROM ix_docs d WHERE 1 = 1{}",
        doc_conditions(q, &mut args)
    );
    let allowed: HashSet<i64> = conn
        .prepare(&sql)?
        .query_map(params_from_iter(args.0), |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    let model = q.model.as_deref().map(normalise_model);
    let bits = query_bits(vector);
    let mut near: Vec<(u32, i64)> = store
        .codes()
        .map
        .iter()
        .filter(|(_, c)| {
            c.dim == vector.len()
                && allowed.contains(&c.doc)
                && model.as_deref().is_none_or(|m| *c.model == *m)
                && window.is_none_or(|(lo, hi)| (lo..=hi).contains(&c.at))
        })
        .map(|(id, c)| (hamming(&bits, &c.bits), *id))
        .collect();
    if near.len() > RESCORE {
        near.select_nth_unstable(RESCORE - 1);
        near.truncate(RESCORE);
    }
    if near.is_empty() {
        return Ok(Vec::new());
    }
    let marks = vec!["?"; near.len()].join(", ");
    let mut stmt = conn.prepare(&format!(
        "SELECT chunk_id, v FROM ix_vectors WHERE chunk_id IN ({marks})"
    ))?;
    let rows = stmt.query_map(params_from_iter(near.iter().map(|(_, id)| *id)), |r| {
        Ok((r.get::<_, i64>(0)?, r.get::<_, Vec<u8>>(1)?))
    })?;
    let mut scored: Vec<(i64, f64, Vec<f32>)> = Vec::new();
    for row in rows {
        let (id, q8) = row?;
        let v: Vec<f32> = q8.iter().map(|b| f32::from(*b as i8)).collect();
        if let Some(u) = rank::unit(&v) {
            scored.push((id, rank::dot(&u, vector) / qnorm, u));
        }
    }
    scored.sort_by(|a, b| {
        b.1.partial_cmp(&a.1)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(a.0.cmp(&b.0))
    });
    scored.truncate(CANDIDATES);
    Ok(scored)
}

struct Row {
    chunk: i64,
    doc: i64,
    ord: i64,
    text: String,
    at: Option<i64>,
    tier: String,
    owner: Option<i64>,
    source: String,
    key: String,
    title: String,
    locator: String,
}

fn load_rows(conn: &Connection, ids: &[i64]) -> rusqlite::Result<HashMap<i64, Row>> {
    let marks = vec!["?"; ids.len()].join(", ");
    let mut stmt = conn.prepare(&format!(
        "SELECT c.id, c.doc_id, c.ord, c.text, COALESCE(c.occurred_at, d.occurred_to), d.tier,
                d.owner_person, d.source_id, d.key, d.title, d.locator
         FROM ix_chunks c JOIN ix_docs d ON d.id = c.doc_id WHERE c.id IN ({marks})"
    ))?;
    let rows = stmt.query_map(params_from_iter(ids.iter()), |r| {
        Ok(Row {
            chunk: r.get(0)?,
            doc: r.get(1)?,
            ord: r.get(2)?,
            text: r.get(3)?,
            at: r.get(4)?,
            tier: r.get(5)?,
            owner: r.get(6)?,
            source: r.get(7)?,
            key: r.get(8)?,
            title: r.get(9)?,
            locator: r.get(10)?,
        })
    })?;
    rows.map(|r| r.map(|row| (row.chunk, row))).collect()
}

/// About 300 characters of `text`, starting a little before the first query word it holds.
fn snippet_of(text: &str, terms: &[String]) -> String {
    let lower = text.to_ascii_lowercase();
    let hit = terms.iter().filter_map(|t| lower.find(t.as_str())).min();
    let mut start = hit.map_or(0, |h| h.saturating_sub(60));
    while !text.is_char_boundary(start) {
        start -= 1;
    }
    let body: String = text[start..].chars().take(SNIPPET_CHARS).collect();
    let more = text[start..].chars().count() > SNIPPET_CHARS;
    format!(
        "{}{}{}",
        if start > 0 { "…" } else { "" },
        body.trim(),
        if more { "…" } else { "" }
    )
}

fn pass(store: &Store, q: &Query, window: Option<(i64, i64)>) -> rusqlite::Result<Vec<Hit>> {
    let conn = store.conn();
    let keyword = keyword_candidates(conn, q, window)?;
    let vectors = match &q.vector {
        Some(v) => vector_candidates(store, q, v, window)?,
        None => Vec::new(),
    };
    let mut fused: HashMap<i64, (f64, bool, bool)> = HashMap::new();
    for (rank, (chunk, _)) in keyword.iter().enumerate() {
        fused.entry(*chunk).or_default().0 += 1.0 / (rank::RRF_K + rank as f64 + 1.0);
        fused.entry(*chunk).or_default().1 = true;
    }
    let mut units: HashMap<i64, Vec<f32>> = HashMap::new();
    for (rank, (chunk, _, unit)) in vectors.into_iter().enumerate() {
        let e = fused.entry(chunk).or_default();
        e.0 += 1.0 / (rank::RRF_K + rank as f64 + 1.0);
        e.2 = true;
        units.insert(chunk, unit);
    }
    if fused.is_empty() {
        return Ok(Vec::new());
    }
    let ids: Vec<i64> = fused.keys().copied().collect();
    let rows = load_rows(conn, &ids)?;
    let mut cands: Vec<Cand> = fused
        .iter()
        .filter_map(|(chunk, (rrf, kw, vc))| {
            let r = rows.get(chunk)?;
            Some(Cand {
                chunk: *chunk,
                doc: r.doc,
                tier: r.tier.clone(),
                owner: r.owner,
                at: r.at,
                about: q.focus.as_ref().is_some_and(|f| {
                    crate::who::mentions(&r.title, &f.names)
                        || crate::who::mentions(&r.text, &f.names)
                }),
                rrf: *rrf,
                score: 0.0,
                keyword: *kw,
                vector: *vc,
                unit: units.get(chunk).cloned(),
            })
        })
        .collect();
    cands.sort_by_key(|c| c.chunk);
    rank::apply(
        &mut cands,
        &Context {
            viewer: q.viewer,
            person: q.person,
            focus: q.focus.as_ref().map(|f| f.person),
            now_ms: q.now_ms,
            windowed: window.is_some(),
        },
    );
    let mut groups = rank::group(cands);
    groups.truncate((q.k * 2).max(10));
    // The near-duplicate check needs a vector for every chunk it compares.
    let missing: Vec<i64> = groups
        .iter()
        .flat_map(|g| &g.chunks)
        .filter(|c| c.unit.is_none())
        .map(|c| c.chunk)
        .collect();
    if !missing.is_empty() {
        let marks = vec!["?"; missing.len()].join(", ");
        let mut stmt = conn.prepare(&format!(
            "SELECT chunk_id, v FROM ix_vectors WHERE chunk_id IN ({marks})"
        ))?;
        let found = stmt.query_map(params_from_iter(missing.iter()), |r| {
            Ok((r.get::<_, i64>(0)?, r.get::<_, Vec<u8>>(1)?))
        })?;
        for f in found {
            let (id, q8) = f?;
            let v: Vec<f32> = q8.iter().map(|b| f32::from(*b as i8)).collect();
            if let Some(u) = rank::unit(&v) {
                units.insert(id, u);
            }
        }
        for c in groups.iter_mut().flat_map(|g| g.chunks.iter_mut()) {
            if c.unit.is_none() {
                c.unit = units.get(&c.chunk).cloned();
            }
        }
    }
    rank::collapse(&mut groups);
    let terms: Vec<String> = q
        .text
        .split(|c: char| !(c.is_alphanumeric() || c == '_'))
        .filter(|w| !w.is_empty())
        .map(str::to_ascii_lowercase)
        .collect();
    let mut hits = Vec::new();
    for g in &groups {
        for c in &g.chunks {
            let Some(r) = rows.get(&c.chunk) else {
                continue;
            };
            hits.push(Hit {
                kind: match r.tier.as_str() {
                    "memory" => "memory",
                    "note" => "note",
                    _ => "chat",
                },
                doc: r.doc,
                ord: r.ord,
                source: r.source.clone(),
                key: r.key.clone(),
                title: r.title.clone(),
                locator: r.locator.clone(),
                owner: r.owner,
                at: r.at,
                snippet: snippet_of(&r.text, &terms),
                score: c.score,
                keyword: c.keyword,
                vector: c.vector,
            });
        }
    }
    Ok(hits)
}

/// Search the index as `q.viewer`. With a date window and fewer than three matching docs, the
/// window is widened by half once and the answer says so.
pub fn search(store: &Store, q: &Query) -> rusqlite::Result<Found> {
    let distinct = |hits: &[Hit]| hits.iter().map(|h| h.doc).collect::<HashSet<_>>().len();
    let mut found = Found {
        hits: pass(store, q, q.window)?,
        widened: false,
    };
    if let Some((lo, hi)) = q.window {
        if distinct(&found.hits) < MIN_DOCS {
            let pad = (hi - lo) / 4;
            let wider = pass(store, q, Some((lo - pad, hi + pad)))?;
            if distinct(&wider) > distinct(&found.hits) {
                found = Found {
                    hits: wider,
                    widened: true,
                };
            }
        }
    }
    found.hits.truncate(q.k.max(1));
    Ok(found)
}

/// A doc's chunk with the ones beside it, as `viewer` may read them.
#[derive(Debug)]
pub struct Passage {
    pub doc: i64,
    pub kind: &'static str,
    pub title: String,
    pub locator: String,
    pub owner: Option<i64>,
    pub chunks: Vec<(i64, String, Option<i64>, Option<String>)>,
    pub truncated: bool,
}

/// Read doc `doc`: with `ord`, that chunk and `around` on each side; without it, the whole doc up
/// to 50 chunks. `None` when the viewer may not read it (the same answer as for a missing doc).
pub fn read_doc(
    conn: &Connection,
    viewer: i64,
    doc: i64,
    ord: Option<i64>,
    around: i64,
) -> rusqlite::Result<Option<Passage>> {
    let mut args = Args::default();
    let id = args.push(doc);
    let sql = format!(
        "SELECT d.title, d.locator, d.tier, d.owner_person FROM ix_docs d WHERE d.id = ?{id} AND {}",
        privacy(&[viewer], &mut args)
    );
    let head: Option<(String, String, String, Option<i64>)> = conn
        .query_row(&sql, params_from_iter(args.0), |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
        })
        .optional()?;
    let Some((title, locator, tier, owner)) = head else {
        return Ok(None);
    };
    let (lo, hi) = match ord {
        Some(o) => (o - around.max(0), o + around.max(0)),
        None => (i64::MIN, i64::MAX),
    };
    let mut stmt = conn.prepare(
        "SELECT ord, text, occurred_at, anchor FROM ix_chunks
         WHERE doc_id = ?1 AND ord BETWEEN ?2 AND ?3 ORDER BY ord LIMIT 51",
    )?;
    let mut chunks: Vec<_> = stmt
        .query_map(params![doc, lo, hi], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
        })?
        .collect::<rusqlite::Result<_>>()?;
    let truncated = chunks.len() > 50;
    chunks.truncate(50);
    Ok(Some(Passage {
        doc,
        kind: match tier.as_str() {
            "memory" => "memory",
            "note" => "note",
            _ => "chat",
        },
        title,
        locator,
        owner,
        chunks,
        truncated,
    }))
}

/// What a trace question needs to know about the chat it names.
#[derive(Debug)]
pub struct TraceTarget {
    pub doc: i64,
    pub source: String,
    pub kind: String,
    pub key: String,
    pub title: String,
    pub device_uid: Option<String>,
    /// The person the doc is filed under.
    pub owner: Option<i64>,
}

/// The chat `chat` names (`d:<id>[#n]`, a bare id or a key), if `viewer` may read it.
pub fn trace_target(
    conn: &Connection,
    viewer: i64,
    chat: &str,
) -> rusqlite::Result<Option<TraceTarget>> {
    let q = Query {
        viewer,
        chat: Some(chat.to_owned()),
        ..Default::default()
    };
    let mut args = Args::default();
    let sql = format!(
        "SELECT d.id, d.source_id, s.kind, d.key, d.title, s.device_uid, d.owner_person
         FROM ix_docs d JOIN ix_sources s ON s.id = d.source_id
         WHERE d.tier = 'conversation'{} ORDER BY d.id LIMIT 1",
        doc_conditions(&q, &mut args)
    );
    conn.query_row(&sql, params_from_iter(args.0), |r| {
        Ok(TraceTarget {
            doc: r.get(0)?,
            source: r.get(1)?,
            kind: r.get(2)?,
            key: r.get(3)?,
            title: r.get(4)?,
            device_uid: r.get(5)?,
            owner: r.get(6)?,
        })
    })
    .optional()
}

// ---- the machines' frames ----

/// Who a frame comes from, as the registry knows it.
#[derive(Debug, Clone)]
pub struct Ctx {
    /// The machine's own person.
    pub person: i64,
    /// Its device uid, or `client:<id>` before it announced one.
    pub device: String,
    /// The people it may write for and read the private text of, its own person included.
    pub acts_for: Vec<i64>,
    /// Every person the hub knows.
    pub people: Vec<i64>,
}

fn id_arg(v: &Value) -> Option<i64> {
    v.as_i64().or_else(|| v.as_str()?.trim().parse().ok())
}

fn digest_of(conn: &Connection, source: &str) -> rusqlite::Result<(i64, String)> {
    let mut stmt =
        conn.prepare("SELECT key, hash FROM ix_docs WHERE source_id = ?1 ORDER BY key")?;
    let rows = stmt.query_map([source], |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
    })?;
    let (mut n, mut hasher) = (0, Sha256::new());
    for row in rows {
        let (key, hash) = row?;
        if n > 0 {
            hasher.update(b"\n");
        }
        hasher.update(format!("{key}:{hash}").as_bytes());
        n += 1;
    }
    Ok((n, format!("{:x}", hasher.finalize())))
}

/// The source `id` as this machine may use it: made if new, refused when another machine has it
/// (and `memory` is the hub's own).
fn claim_source(
    conn: &Connection,
    ctx: &Ctx,
    id: &str,
    kind: &str,
    name: &str,
) -> Result<(), String> {
    if id.is_empty() || id.len() > 200 || id == MEMORY_SOURCE {
        return Err("that is not a source name this machine may use".into());
    }
    let existing: Option<Option<String>> = conn
        .query_row(
            "SELECT device_uid FROM ix_sources WHERE id = ?1",
            [id],
            |r| r.get(0),
        )
        .optional()
        .map_err(|e| e.to_string())?;
    match existing {
        Some(owner) if owner.as_deref() != Some(ctx.device.as_str()) => {
            Err("that source belongs to another machine".into())
        }
        Some(_) => conn
            .execute(
                &format!("UPDATE ix_sources SET updated_at = {NOW}, name = ?2 WHERE id = ?1"),
                params![id, name],
            )
            .map(|_| ())
            .map_err(|e| e.to_string()),
        None => {
            if !["folder", "helm_chats", "claude_code", "chat_logs"].contains(&kind) {
                return Err("a source is a folder, helm_chats, claude_code or chat_logs".into());
            }
            conn.execute(
                &format!(
                    "INSERT INTO ix_sources (id, kind, name, device_uid, owner_person, updated_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, {NOW})"
                ),
                params![id, kind, name, ctx.device, ctx.person],
            )
            .map(|_| ())
            .map_err(|e| e.to_string())
        }
    }
}

/// Whether this machine owns the source (it exists and carries the machine's device).
fn source_is_mine(conn: &Connection, ctx: &Ctx, id: &str) -> bool {
    conn.query_row(
        "SELECT 1 FROM ix_sources WHERE id = ?1 AND device_uid = ?2",
        params![id, ctx.device],
        |_| Ok(()),
    )
    .optional()
    .ok()
    .flatten()
    .is_some()
}

fn failed(kind: &str, id: &Value, why: &str) -> Value {
    json!({ "type": kind, "id": id, "ok": false, "error": why })
}

fn source_kind(conn: &Connection, id: &str) -> Option<String> {
    conn.query_row("SELECT kind FROM ix_sources WHERE id = ?1", [id], |r| {
        r.get(0)
    })
    .ok()
}

fn scope_of(s: Option<&str>) -> Option<&'static str> {
    match s.unwrap_or("everyone") {
        "me" | "private" => Some("private"),
        "everyone" | "shared" => Some("shared"),
        "some" => Some("some"),
        _ => None,
    }
}

fn b64(s: &str) -> Option<Vec<u8>> {
    base64::engine::general_purpose::STANDARD.decode(s).ok()
}

/// One doc of an `ix_put`, checked: the wire shape, or why it is refused.
fn parse_doc(
    d: &Value,
    ctx: &Ctx,
    source: &str,
    kind: &str,
    frame_model: Option<&str>,
) -> Result<DocIn, String> {
    let text = |k: &str| d.get(k).and_then(Value::as_str);
    let key = text("key").filter(|k| !k.is_empty() && k.len() <= MAX_KEY);
    let hash = text("hash").filter(|h| !h.is_empty() && h.len() <= 128);
    let (Some(key), Some(hash)) = (key, hash) else {
        return Err("a doc needs a key and a hash".into());
    };
    let title = text("title").unwrap_or(key);
    if title.chars().count() > MAX_TITLE {
        return Err("a title is at most 500 characters".into());
    }
    let tier = match text("tier") {
        Some(t @ ("note" | "conversation")) => t,
        None if kind == "folder" => "note",
        None => "conversation",
        Some(_) => return Err("a doc's tier is note or conversation".into()),
    };
    let owner = match d.get("owner") {
        None | Some(Value::Null) => ctx.person,
        Some(v) => id_arg(v).ok_or("the owner must be a person id")?,
    };
    if !ctx.acts_for.contains(&owner) || !ctx.people.contains(&owner) {
        return Err("this machine may not store for that person".into());
    }
    let visibility = scope_of(text("scope")).ok_or("scope is me, everyone or some")?;
    let readers: Vec<i64> = d
        .get("readers")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(id_arg).collect())
        .unwrap_or_default();
    // A reader the hub does not know cannot be told apart from nobody: the doc stays private.
    let mut readers: Vec<i64> = readers
        .into_iter()
        .filter(|p| *p != owner && ctx.people.contains(p))
        .collect();
    readers.sort_unstable();
    readers.dedup();
    let visibility = match (visibility, readers.is_empty()) {
        ("some", true) => "private",
        (v, _) => v,
    };
    let doc_model = text("model").or(frame_model);
    let Some(chunks) = d.get("chunks").and_then(Value::as_array) else {
        return Err("a doc needs its chunks".into());
    };
    let mut out: Vec<ChunkIn> = Vec::new();
    let mut ords = HashSet::new();
    for (i, c) in chunks.iter().enumerate() {
        let ord = c.get("ord").and_then(Value::as_i64).unwrap_or(i as i64);
        let body = c.get("text").and_then(Value::as_str).unwrap_or("");
        if body.trim().is_empty() || body.len() > MAX_CHUNK_TEXT || !ords.insert(ord) {
            return Err("a chunk needs its own number and 1 byte to 8 KB of text".into());
        }
        let vec = match c.get("vec").and_then(Value::as_str) {
            None => None,
            Some(v) => {
                let q = b64(v).filter(|q| (1..=MAX_VECTOR_DIM).contains(&q.len()));
                let scale = c
                    .get("scale")
                    .and_then(Value::as_f64)
                    .filter(|s| s.is_finite() && *s > 0.0);
                let model = c.get("model").and_then(Value::as_str).or(doc_model);
                match (
                    q,
                    scale,
                    model.map(normalise_model).filter(|m| !m.is_empty()),
                ) {
                    (Some(q), Some(scale), Some(model)) => Some(VecIn {
                        model,
                        scale: scale as f32,
                        q,
                    }),
                    _ => return Err("a vector needs its bytes, a scale and a model".into()),
                }
            }
        };
        out.push(ChunkIn {
            ord,
            heading: c
                .get("heading")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned(),
            text: body.to_owned(),
            at: c.get("at").and_then(Value::as_i64),
            anchor: c.get("anchor").and_then(Value::as_str).map(str::to_owned),
            vec,
        });
    }
    if out.is_empty() {
        return Err("a doc needs at least one chunk".into());
    }
    let at_min = out.iter().filter_map(|c| c.at).min();
    let at_max = out.iter().filter_map(|c| c.at).max();
    let one = d.get("occurredAt").and_then(Value::as_i64);
    Ok(DocIn {
        source: source.to_owned(),
        key: key.to_owned(),
        hash: hash.to_owned(),
        title: title.to_owned(),
        fts_title: title.to_owned(),
        locator: text("locator")
            .unwrap_or("")
            .chars()
            .take(MAX_LOCATOR)
            .collect(),
        tier: tier.to_owned(),
        owner: Some(owner),
        visibility: visibility.to_owned(),
        readers,
        from: d
            .get("occurredFrom")
            .and_then(Value::as_i64)
            .or(at_min)
            .or(one),
        to: d
            .get("occurredTo")
            .and_then(Value::as_i64)
            .or(at_max)
            .or(one),
        chunks: out,
    })
}

/// Whether the stored doc is the one `d` would write (same content hash, owner and audience).
fn same_doc(conn: &Connection, d: &DocIn) -> rusqlite::Result<bool> {
    let row: Option<(i64, String, Option<i64>, String)> = conn
        .query_row(
            "SELECT id, hash, owner_person, visibility FROM ix_docs WHERE source_id = ?1 AND key = ?2",
            params![d.source, d.key],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .optional()?;
    let Some((id, hash, owner, visibility)) = row else {
        return Ok(false);
    };
    if hash != d.hash || owner != d.owner || visibility != d.visibility {
        return Ok(false);
    }
    let readers: Vec<i64> = conn
        .prepare("SELECT person_id FROM ix_readers WHERE doc_id = ?1 ORDER BY person_id")?
        .query_map([id], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    Ok(readers == d.readers)
}

/// `ix_hello`: say which sources this machine has, and answer with what the hub holds of each.
fn hello(store: &mut Store, ctx: &Ctx, id: &Value, frame: &Value) -> Value {
    let Some(list) = frame.get("sources").and_then(Value::as_array) else {
        return failed("ix_state", id, "ix_hello needs its sources");
    };
    let conn = store.conn();
    let mut out = Vec::new();
    for s in list.iter().take(64) {
        let sid = s.get("id").and_then(Value::as_str).unwrap_or("");
        let kind = s.get("kind").and_then(Value::as_str).unwrap_or("folder");
        let name = s.get("name").and_then(Value::as_str).unwrap_or(sid);
        match claim_source(
            conn,
            ctx,
            sid,
            kind,
            &name.chars().take(100).collect::<String>(),
        )
        .and_then(|()| digest_of(conn, sid).map_err(|e| e.to_string()))
        {
            Ok((docs, digest)) => out.push(json!({ "id": sid, "docs": docs, "digest": digest })),
            Err(why) => out.push(json!({ "id": sid, "error": why })),
        }
    }
    json!({ "type": "ix_state", "id": id, "ok": true, "sources": out, "features": FEATURES })
}

/// `ix_manifest`: a page of `{key, hash}` for one source, after `afterKey`.
fn manifest(store: &Store, ctx: &Ctx, id: &Value, frame: &Value) -> Value {
    let conn = store.conn();
    let source = frame.get("source").and_then(Value::as_str).unwrap_or("");
    if !source_is_mine(conn, ctx, source) {
        return failed(
            "ix_manifest_page",
            id,
            "that is not one of this machine's sources",
        );
    }
    let after = frame.get("afterKey").and_then(Value::as_str).unwrap_or("");
    let limit = frame
        .get("limit")
        .and_then(Value::as_u64)
        .map_or(1000, |n| n.clamp(1, 1000)) as usize;
    let page: rusqlite::Result<Vec<(String, String)>> = conn
        .prepare(
            "SELECT key, hash FROM ix_docs WHERE source_id = ?1 AND key > ?2 ORDER BY key LIMIT ?3",
        )
        .and_then(|mut s| {
            s.query_map(params![source, after, limit as i64 + 1], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })?
            .collect()
        });
    match page {
        Ok(mut items) => {
            let more = items.len() > limit;
            items.truncate(limit);
            let items: Vec<Value> = items
                .into_iter()
                .map(|(k, h)| json!({ "key": k, "hash": h }))
                .collect();
            json!({ "type": "ix_manifest_page", "id": id, "ok": true, "items": items, "more": more })
        }
        Err(e) => failed("ix_manifest_page", id, &format!("the index failed: {e}")),
    }
}

/// `ix_put`: store docs. Each doc is its own transaction, so one refusal leaves the rest in.
fn put(store: &mut Store, ctx: &Ctx, id: &Value, frame: &Value) -> Value {
    let Some(source) = frame.get("source").and_then(Value::as_str) else {
        return failed("ix_put_result", id, "ix_put needs its source");
    };
    let Some(docs) = frame.get("docs").and_then(Value::as_array) else {
        return failed("ix_put_result", id, "ix_put needs its docs");
    };
    let (chunks, bytes) = docs.iter().fold((0, 0), |(n, b), d| {
        let cs = d
            .get("chunks")
            .and_then(Value::as_array)
            .map_or(&[][..], |a| a.as_slice());
        (
            n + cs.len(),
            b + cs
                .iter()
                .map(|c| c.get("text").and_then(Value::as_str).map_or(0, str::len))
                .sum::<usize>(),
        )
    });
    if chunks > MAX_PUT_CHUNKS || bytes > MAX_PUT_BYTES {
        return failed(
            "ix_put_result",
            id,
            "one put carries at most 200 chunks and 2 MiB of text",
        );
    }
    // A source this machine has not announced is made from the put's own `kind` (folder if none).
    let kind = frame
        .get("kind")
        .and_then(Value::as_str)
        .unwrap_or("folder");
    if let Err(why) = claim_source(store.conn(), ctx, source, kind, source) {
        return failed("ix_put_result", id, &why);
    }
    let kind = source_kind(store.conn(), source).unwrap_or_else(|| kind.to_owned());
    let frame_model = frame.get("model").and_then(Value::as_str);
    let mut results = Vec::new();
    for d in docs {
        let key = d
            .get("key")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned();
        // Archived and backup material is never indexed, whatever machine sends it.
        let status = match parse_doc(d, ctx, source, &kind, frame_model) {
            _ if kind == "folder" && crate::import::retired_path(&key) => json!({
                "key": key, "status": "refused",
                "reason": "archived and backup notes are not indexed"
            }),
            Err(why) => json!({ "key": key, "status": "refused", "reason": why }),
            // The host holds credential-looking passages back before it sends; this is the
            // hub's own check for a machine that does not.
            // An older copy of the doc, from before it held one, goes too.
            Ok(doc) if doc_looks_like_secret(&doc) => {
                let (conn, codes) = store.parts_mut();
                drop_doc(conn, codes, ctx, source, &key);
                json!({
                    "key": key, "status": "refused",
                    "reason": "it looks like it holds a credential, so it is not indexed"
                })
            }
            Ok(doc) => match write_one(store, &doc) {
                Ok(s) => json!({ "key": key, "status": s }),
                Err(e) => {
                    json!({ "key": key, "status": "refused", "reason": format!("the index failed: {e}") })
                }
            },
        };
        results.push(status);
    }
    json!({ "type": "ix_put_result", "id": id, "ok": true, "results": results })
}

/// Whether any part of a doc that is about to be indexed looks like a credential.
fn doc_looks_like_secret(doc: &DocIn) -> bool {
    use crate::secrets::looks_like_secret;
    looks_like_secret(&doc.title)
        || doc
            .chunks
            .iter()
            .any(|c| looks_like_secret(&c.text) || looks_like_secret(&c.heading))
}

fn write_one(store: &mut Store, doc: &DocIn) -> rusqlite::Result<&'static str> {
    let (conn, codes) = store.parts_mut();
    if same_doc(conn, doc)? {
        return Ok("unchanged");
    }
    let existed = conn
        .query_row(
            "SELECT 1 FROM ix_docs WHERE source_id = ?1 AND key = ?2",
            params![doc.source, doc.key],
            |_| Ok(()),
        )
        .optional()?
        .is_some();
    let tx = conn.transaction()?;
    let (_, delta) = write_doc(&tx, codes, doc)?;
    tx.commit()?;
    codes.apply(delta);
    Ok(if existed { "replaced" } else { "inserted" })
}

/// Remove one doc of a source, when it is there and this machine may act for its owner.
fn drop_doc(conn: &mut Connection, codes: &mut Codes, ctx: &Ctx, source: &str, key: &str) -> bool {
    let doc: rusqlite::Result<Option<(i64, Option<i64>)>> = conn
        .query_row(
            "SELECT id, owner_person FROM ix_docs WHERE source_id = ?1 AND key = ?2",
            params![source, key],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional();
    let Ok(Some((doc, owner))) = doc else {
        return false;
    };
    if owner.is_some_and(|o| !ctx.acts_for.contains(&o)) {
        return false;
    }
    let done = conn.transaction().and_then(|tx| {
        let delta = delete_doc(&tx, doc)?;
        tx.commit()?;
        Ok(delta)
    });
    match done {
        Ok(delta) => {
            codes.apply(delta);
            true
        }
        Err(_) => false,
    }
}

/// `ix_delete`: remove docs of one of this machine's sources, for the people it may act for.
fn delete(store: &mut Store, ctx: &Ctx, id: &Value, frame: &Value) -> Value {
    let source = frame.get("source").and_then(Value::as_str).unwrap_or("");
    let Some(keys) = frame.get("keys").and_then(Value::as_array) else {
        return failed("ix_delete_result", id, "ix_delete needs its keys");
    };
    if !source_is_mine(store.conn(), ctx, source) {
        return failed(
            "ix_delete_result",
            id,
            "that is not one of this machine's sources",
        );
    }
    let (conn, codes) = store.parts_mut();
    let mut deleted = 0;
    for key in keys.iter().take(1000).filter_map(Value::as_str) {
        if drop_doc(conn, codes, ctx, source, key) {
            deleted += 1;
        }
    }
    json!({ "type": "ix_delete_result", "id": id, "ok": true, "deleted": deleted })
}

/// `ix_pending`: chunks with no vector yet for `model`, of the docs this machine's people own or
/// this machine sent.
fn pending(store: &Store, ctx: &Ctx, id: &Value, frame: &Value, model: &str) -> Value {
    let limit = frame
        .get("limit")
        .and_then(Value::as_u64)
        .map_or(200, |n| n.clamp(1, 200));
    let mut args = Args::default();
    let m = args.push(model.to_owned());
    let sql = format!(
        "SELECT c.id,
                CASE WHEN d.source_id = 'memory' THEN d.title || char(10) || char(10) || c.text
                     ELSE c.text END
         FROM ix_chunks c JOIN ix_docs d ON d.id = c.doc_id
         WHERE NOT EXISTS (SELECT 1 FROM ix_vectors v WHERE v.chunk_id = c.id AND v.model = ?{m})
           AND {} AND {} ORDER BY c.id LIMIT {limit}",
        privacy(&ctx.acts_for, &mut args),
        embeddable(ctx, &mut args)
    );
    let rows: rusqlite::Result<Vec<Value>> = store.conn().prepare(&sql).and_then(|mut s| {
        s.query_map(params_from_iter(args.0), |r| {
            Ok(json!({ "chunk": r.get::<_, i64>(0)?, "text": r.get::<_, String>(1)? }))
        })?
        .collect()
    });
    match rows {
        Ok(chunks) => {
            json!({ "type": "ix_pending_list", "id": id, "ok": true, "chunks": chunks, "model": model })
        }
        Err(e) => failed("ix_pending_list", id, &format!("the index failed: {e}")),
    }
}

/// `ix_vectors`: store the vectors a machine computed, for chunks of docs its people own or it
/// sent.
fn vectors(store: &mut Store, ctx: &Ctx, id: &Value, frame: &Value) -> Value {
    let Some(items) = frame.get("items").and_then(Value::as_array) else {
        return failed("ix_vectors_result", id, "ix_vectors needs its items");
    };
    let frame_model = frame.get("model").and_then(Value::as_str);
    let (conn, codes) = store.parts_mut();
    let mut stored = 0;
    let Ok(tx) = conn.transaction() else {
        return failed("ix_vectors_result", id, "the index is busy");
    };
    let mut delta = Delta::default();
    for it in items.iter().take(MAX_PUT_CHUNKS) {
        let chunk = it.get("chunk").and_then(id_arg);
        let q = it.get("vec").and_then(Value::as_str).and_then(b64);
        let scale = it
            .get("scale")
            .and_then(Value::as_f64)
            .filter(|s| s.is_finite() && *s > 0.0);
        let model = it
            .get("model")
            .and_then(Value::as_str)
            .or(frame_model)
            .map(normalise_model)
            .filter(|m| !m.is_empty());
        let (Some(chunk), Some(q), Some(scale), Some(model)) = (chunk, q, scale, model) else {
            continue;
        };
        if q.is_empty() || q.len() > MAX_VECTOR_DIM {
            continue;
        }
        let mut args = Args::default();
        let c = args.push(chunk);
        let sql = format!(
            "SELECT 1 FROM ix_chunks c JOIN ix_docs d ON d.id = c.doc_id WHERE c.id = ?{c} AND {} AND {}",
            privacy(&ctx.acts_for, &mut args),
            embeddable(ctx, &mut args)
        );
        if !tx
            .query_row(&sql, params_from_iter(args.0), |_| Ok(()))
            .optional()
            .is_ok_and(|r| r.is_some())
        {
            continue;
        }
        let done = tx.execute(
            "INSERT OR REPLACE INTO ix_vectors (chunk_id, model, dim, scale, v)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![chunk, model, q.len() as i64, scale, q],
        );
        if done.is_ok() {
            stored += 1;
            delta.removed.push(chunk);
            if let Ok(Some(code)) = code_of(&tx, codes, chunk) {
                delta.added.push((chunk, code));
            }
        }
    }
    if tx.commit().is_err() {
        return failed("ix_vectors_result", id, "the index is busy");
    }
    codes.apply(delta);
    json!({ "type": "ix_vectors_result", "id": id, "ok": true, "stored": stored })
}

/// The reply to one `ix_*` frame. `model` is the hub's embedding model (what `ix_pending` lists
/// chunks for).
pub fn answer(
    store: &mut Store,
    ctx: &Ctx,
    kind: &str,
    id: &Value,
    frame: &Value,
    model: &str,
) -> Value {
    match kind {
        "ix_hello" => hello(store, ctx, id, frame),
        "ix_manifest" => manifest(store, ctx, id, frame),
        "ix_put" => put(store, ctx, id, frame),
        "ix_delete" => delete(store, ctx, id, frame),
        "ix_pending" => pending(store, ctx, id, frame, model),
        "ix_vectors" => vectors(store, ctx, id, frame),
        _ => failed("ix_state", id, "the hub does not know that frame"),
    }
}

/// Answer one `ix_*` frame from the machine `client_id`: check what it may do, then do the
/// database work on the blocking pool so the hub's one thread keeps serving Telegram and the
/// lines.
pub async fn answer_frame(
    lines: &crate::lines::Lines,
    client_id: i64,
    kind: &str,
    id: &Value,
    frame: Value,
    size: usize,
) -> Value {
    let reply = reply_type(kind);
    if !lines.is_peer(client_id) {
        return failed(reply, id, "This machine's token cannot use the household.");
    }
    if size > MAX_PUT_BYTES + MAX_PUT_BYTES / 2 {
        return failed(reply, id, "That frame is too large.");
    }
    let Some(state) = lines.app_state() else {
        return failed(reply, id, "The hub's index is unavailable.");
    };
    let ctx = {
        let Ok(r) = lines.registry().lock() else {
            return failed(reply, id, "The hub's index is unavailable.");
        };
        let Ok(Some(me)) = r.client_by_id(client_id) else {
            return failed(reply, id, "This machine is not known to the hub.");
        };
        let people: Vec<i64> = r
            .people()
            .unwrap_or_default()
            .iter()
            .map(|p| p.id)
            .collect();
        Ctx {
            person: me.person_id,
            device: me
                .device_uid
                .unwrap_or_else(|| format!("client:{client_id}")),
            acts_for: people
                .iter()
                .copied()
                .filter(|p| r.may_act_for(client_id, *p))
                .collect(),
            people,
        }
    };
    let model = state.embedder.model().to_owned();
    let (kind, id2) = (kind.to_owned(), id.clone());
    let done = tokio::task::spawn_blocking(move || {
        let mut store = state.store.lock().map_err(|_| ())?;
        Ok::<_, ()>(answer(&mut store, &ctx, &kind, &id2, &frame, &model))
    })
    .await;
    match done {
        Ok(Ok(v)) => v,
        _ => failed(reply, id, "The hub's index is unavailable."),
    }
}

/// The type of the reply to `kind`, for a refusal made before the frame is read.
pub fn reply_type(kind: &str) -> &'static str {
    match kind {
        "ix_hello" => "ix_state",
        "ix_manifest" => "ix_manifest_page",
        "ix_put" => "ix_put_result",
        "ix_delete" => "ix_delete_result",
        "ix_pending" => "ix_pending_list",
        _ => "ix_vectors_result",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn memories_are_cut_into_chunks_that_fit() {
        let long = "word ".repeat(900);
        let body = format!("# One\nfirst part\n\n## Two\n{long}\n\n### Three\nlast part");
        let chunks = chunk_text("about it", &body, "title");
        assert!(
            chunks.iter().all(|(_, t)| t.len() <= HARD_CHUNK),
            "{:?}",
            chunks.iter().map(|c| c.1.len()).collect::<Vec<_>>()
        );
        assert!(chunks[0].1.starts_with("about it"));
        assert!(chunks.iter().any(|(h, _)| h == "Two"));
        assert_eq!(chunks.last().map(|c| c.0.as_str()), Some("Three"));
        // Nothing is lost.
        let joined: String = chunks
            .iter()
            .map(|c| c.1.as_str())
            .collect::<Vec<_>>()
            .join(" ");
        assert_eq!(joined.matches("word").count(), 900);
        assert_eq!(
            chunk_text("", "", "just a title"),
            vec![(String::new(), "just a title".to_owned())]
        );
        assert_eq!(chunk_text("", &"x".repeat(5000), "t").len(), 3);
    }

    #[test]
    fn quantised_vectors_keep_their_direction_and_codes_their_signs() {
        let v = [0.9f32, -0.4, 0.0, 0.1, -0.9, 0.3, 0.2, -0.1, 0.5];
        let (scale, q) = quantize(&v);
        for (x, b) in v.iter().zip(&q) {
            assert!((x - f32::from(*b as i8) * scale).abs() < scale, "{x}");
        }
        assert_eq!(&*sign_bits(&q), &*query_bits(&v));
        assert_eq!(hamming(&query_bits(&v), &query_bits(&v.map(|x| -x))), 8);
        assert_eq!(quantize(&[0.0, 0.0]).1, vec![0, 0]);
        assert_eq!(
            normalise_model("nomic-embed-text:latest"),
            "nomic-embed-text"
        );
    }

    // ---- the privacy and ranking tables ----

    const MODEL: &str = "nomic-embed-text";

    fn ctx(person: i64, acts_for: &[i64]) -> Ctx {
        Ctx {
            person,
            device: format!("device-{person}"),
            acts_for: acts_for.to_vec(),
            people: vec![1, 2, 3],
        }
    }

    fn vec_b64(v: &[f32]) -> (String, f32) {
        let (scale, q) = quantize(v);
        (base64::engine::general_purpose::STANDARD.encode(q), scale)
    }

    /// One doc over the wire shape: chunk 0 carries `vector` (when given), chunk 1 never does.
    #[allow(clippy::too_many_arguments)]
    fn put_doc(
        store: &mut Store,
        c: &Ctx,
        key: &str,
        tier: &str,
        owner: i64,
        scope: &str,
        readers: &[i64],
        text: &str,
        at: i64,
        vector: Option<&[f32]>,
    ) -> Value {
        let mut first = json!({ "ord": 0, "text": text, "at": at });
        if let Some(v) = vector {
            let (b, scale) = vec_b64(v);
            first["vec"] = json!(b);
            first["scale"] = json!(scale);
            first["model"] = json!(MODEL);
        }
        let doc = json!({
            "key": key, "hash": format!("h-{key}-{text}"), "title": key, "locator": format!("{key}.md"),
            "tier": tier, "owner": owner.to_string(), "scope": scope,
            "readers": readers.iter().map(|r| r.to_string()).collect::<Vec<_>>(),
            "chunks": [first, { "ord": 1, "text": format!("{text} second part"), "at": at }],
        });
        let mut doc = doc;
        if scope == "-" {
            // No scope named: the household's default.
            doc.as_object_mut().unwrap().remove("scope");
        }
        let frame = json!({ "type": "ix_put", "id": 1, "source": format!("{}:notes", c.device),
            "docs": [doc] });
        answer(store, c, "ix_put", &json!(1), &frame, MODEL)
    }

    fn keys(store: &Store, viewer: i64, text: &str, vector: Option<&[f32]>) -> Vec<String> {
        let found = search(
            store,
            &Query {
                viewer,
                text: text.into(),
                k: 50,
                vector: vector.map(<[f32]>::to_vec),
                ..Default::default()
            },
        )
        .unwrap();
        let mut out: Vec<String> = found.hits.iter().map(|h| h.key.clone()).collect();
        out.sort();
        out.dedup();
        out
    }

    #[test]
    fn each_viewer_sees_exactly_their_own_shared_and_listed_docs() {
        let mut store = Store::open_in_memory().unwrap();
        let (ann, bea, third) = (ctx(1, &[1]), ctx(2, &[2]), ctx(3, &[3]));
        let query = vec![1.0f32; 8];
        for (i, (c, key, scope, readers)) in [
            (&ann, "ann-private", "me", vec![]),
            (&bea, "bea-private", "me", vec![]),
            (&bea, "bea-shared", "everyone", vec![]),
            (&bea, "bea-some", "some", vec![3]),
            (&third, "third-private", "me", vec![]),
            // A `some` doc whose only reader is unknown is private.
            (&bea, "bea-some-nobody", "some", vec![99]),
            // A doc that names no scope is the household's: everyone finds it.
            (&bea, "bea-unspecified", "-", vec![]),
        ]
        .into_iter()
        .enumerate()
        {
            // Each doc's vector points its own way, so none is a near copy of another.
            let v: Vec<f32> = (0..8)
                .map(|j| if j == 0 { 0.3 } else { f32::from(j == i + 1) })
                .collect();
            let r = put_doc(
                &mut store,
                c,
                key,
                "note",
                c.person,
                scope,
                &readers,
                "zebra herd",
                1000,
                Some(&v),
            );
            assert_eq!(r["results"][0]["status"], "inserted", "{r}");
        }
        // Memories are in the same index: Bea's private one (chosen on purpose), and Ann's, which
        // names no visibility and so is the household's (ids 1 and 2).
        let mut m = crate::store::NewMemory {
            title: "zebra memory private".into(),
            body: "zebra herd".into(),
            owner: Some(2),
            visibility: crate::store::Visibility::Private,
            ..Default::default()
        };
        store.insert(m.clone()).unwrap();
        m.title = "zebra memory shared".into();
        m.owner = Some(1);
        m.visibility = crate::store::Visibility::default();
        store.insert(m).unwrap();

        // Memory 1 is Bea's private, memory 2 is Ann's shared.
        let want: [(i64, &[&str]); 3] = [
            (1, &["2", "ann-private", "bea-shared", "bea-unspecified"]),
            (
                2,
                &[
                    "1",
                    "2",
                    "bea-private",
                    "bea-shared",
                    "bea-some",
                    "bea-some-nobody",
                    "bea-unspecified",
                ],
            ),
            (
                3,
                &[
                    "2",
                    "bea-shared",
                    "bea-some",
                    "bea-unspecified",
                    "third-private",
                ],
            ),
        ];
        for (viewer, expect) in want {
            let expect: Vec<String> = expect.iter().map(|s| (*s).to_owned()).collect();
            assert_eq!(
                keys(&store, viewer, "zebra", None),
                expect,
                "viewer {viewer} by keyword"
            );
            // The vector alone (no words) sees the same docs that have a vector (not the memories).
            let with_vec: Vec<String> = expect.iter().filter(|k| k.len() > 1).cloned().collect();
            assert_eq!(
                keys(&store, viewer, "", Some(&query)),
                with_vec,
                "viewer {viewer} by vector"
            );
        }
        // Reading by id follows the same rule.
        let doc_of = |key: &str| -> i64 {
            store
                .conn()
                .query_row("SELECT id FROM ix_docs WHERE key = ?1", [key], |r| r.get(0))
                .unwrap()
        };
        for (viewer, key, readable) in [
            (1, "ann-private", true),
            (1, "bea-private", false),
            (1, "bea-shared", true),
            (1, "bea-unspecified", true),
            (1, "bea-some", false),
            (3, "bea-some", true),
            (3, "bea-some-nobody", false),
            (2, "bea-some-nobody", true),
        ] {
            let got = read_doc(store.conn(), viewer, doc_of(key), Some(0), 1).unwrap();
            assert_eq!(got.is_some(), readable, "viewer {viewer} reads {key}");
        }
        // Chunks waiting for a vector go only to a machine whose people own them, or that sent
        // them: a doc that is only readable to a machine is not its to embed. Chunk 1 of every
        // doc here has none.
        let waiting = |c: &Ctx| -> Vec<String> {
            let r = pending(&store, c, &json!(1), &json!({ "limit": 200 }), MODEL);
            let mut out: Vec<String> = r["chunks"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|c| c["text"].as_str().unwrap().ends_with("second part"))
                .map(|c| {
                    store
                        .conn()
                        .query_row(
                            "SELECT d.key FROM ix_chunks c JOIN ix_docs d ON d.id = c.doc_id WHERE c.id = ?1",
                            [c["chunk"].as_i64().unwrap()],
                            |r| r.get(0),
                        )
                        .unwrap()
                })
                .collect();
            out.sort();
            out
        };
        assert_eq!(waiting(&ann), ["ann-private"]);
        assert_eq!(waiting(&third), ["third-private"]);
        // A machine of Ann's that may act for Bea also gets hers; one that may not never does.
        assert_eq!(
            waiting(&ctx(1, &[1, 2])),
            [
                "ann-private",
                "bea-private",
                "bea-shared",
                "bea-some",
                "bea-some-nobody",
                "bea-unspecified"
            ]
        );
        // Vectors are stored only for such chunks: a shared or listed doc another person owns
        // cannot be made to rank differently by a machine that merely reads it.
        let store_for = |store: &mut Store, c: &Ctx, key: &str| -> i64 {
            let chunk: i64 = store
                .conn()
                .query_row(
                    "SELECT c.id FROM ix_chunks c JOIN ix_docs d ON d.id = c.doc_id
                     WHERE d.key = ?1 AND c.ord = 1",
                    [key],
                    |r| r.get(0),
                )
                .unwrap();
            let (b, scale) = vec_b64(&[0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0]);
            let frame = json!({ "type": "ix_vectors", "id": 1, "model": MODEL,
                "items": [{ "chunk": chunk, "vec": b, "scale": scale }] });
            answer(store, c, "ix_vectors", &json!(1), &frame, MODEL)["stored"]
                .as_i64()
                .unwrap()
        };
        for (c, key, stored) in [
            (&ann, "bea-shared", 0),
            (&third, "bea-some", 0),
            (&ann, "ann-private", 1),
            (&ctx(1, &[1, 2]), "bea-shared", 1),
        ] {
            assert_eq!(store_for(&mut store, c, key), stored, "{key}");
        }
        // A machine cannot store for someone it does not act for, nor take another's source.
        let r = put_doc(
            &mut store,
            &ann,
            "forged",
            "note",
            2,
            "me",
            &[],
            "zebra",
            1,
            None,
        );
        assert_eq!(r["results"][0]["status"], "refused", "{r}");
        // Archived and backup notes are refused at the door, whoever sends them.
        let r = put_doc(
            &mut store,
            &ann,
            "archived/old.md",
            "note",
            1,
            "me",
            &[],
            "zebra",
            1,
            None,
        );
        assert_eq!(r["results"][0]["status"], "refused", "{r}");
        assert!(
            r["results"][0]["reason"]
                .as_str()
                .unwrap()
                .contains("not indexed"),
            "{r}"
        );
        let steal = json!({ "source": "device-1:notes", "docs": [] });
        let r = answer(&mut store, &bea, "ix_put", &json!(1), &steal, MODEL);
        assert_eq!(r["ok"], false, "{r}");
    }

    #[test]
    fn a_direct_hit_outranks_a_memory_that_only_sits_nearby() {
        let mut store = Store::open_in_memory().unwrap();
        let bea = ctx(2, &[2]);
        let axis =
            |v: &[f32]| -> Vec<f32> { (0..8).map(|j| v.get(j).copied().unwrap_or(0.0)).collect() };
        let query = axis(&[1.0]);
        store
            .insert(crate::store::NewMemory {
                title: "tea shelf".into(),
                body: "where the tea is kept".into(),
                owner: Some(1),
                embedding: Some(crate::store::Embedding {
                    model: MODEL.into(),
                    values: axis(&[0.9, 0.4]),
                }),
                ..Default::default()
            })
            .unwrap();
        // Someone else's shared note and a chat of the viewer's both match the words and the meaning.
        put_doc(
            &mut store,
            &bea,
            "bea-beans",
            "note",
            2,
            "everyone",
            &[],
            "trellis beans",
            1000,
            Some(&axis(&[0.8, 0.0, 0.5])),
        );
        put_doc(
            &mut store,
            &ctx(1, &[1]),
            "own-chat",
            "conversation",
            1,
            "me",
            &[],
            "trellis beans",
            1000,
            Some(&axis(&[0.7, 0.0, 0.0, 0.6])),
        );
        // A private doc of hers that matches just as well is never shown to him, however it ranks.
        put_doc(
            &mut store,
            &bea,
            "bea-secret",
            "note",
            2,
            "me",
            &[],
            "trellis beans",
            1000,
            Some(&axis(&[0.75, 0.0, 0.0, 0.0, 0.6])),
        );
        let found = search(
            &store,
            &Query {
                viewer: 1,
                text: "trellis beans".into(),
                k: 10,
                vector: Some(query),
                model: Some(MODEL.into()),
                now_ms: 1000,
                ..Default::default()
            },
        )
        .unwrap();
        let mut keys: Vec<&str> = found.hits.iter().map(|h| h.key.as_str()).collect();
        keys.dedup();
        assert_eq!(keys, ["own-chat", "bea-beans", "1"]);
    }

    #[test]
    fn ranking_prefers_memories_own_items_and_the_window_and_widens_once() {
        let mut store = Store::open_in_memory().unwrap();
        let (ann, bea) = (ctx(1, &[1]), ctx(2, &[2]));
        let day = 86_400_000i64;
        let now = 1_800_000_000_000i64;
        store
            .insert(crate::store::NewMemory {
                title: "budget plan".into(),
                body: "budget planning notes".into(),
                owner: Some(1),
                ..Default::default()
            })
            .unwrap();
        put_doc(
            &mut store,
            &ann,
            "chat-budget",
            "conversation",
            1,
            "me",
            &[],
            "budget planning notes",
            now,
            None,
        );
        put_doc(
            &mut store,
            &ann,
            "note-garden",
            "note",
            1,
            "me",
            &[],
            "garden tomatoes",
            now,
            None,
        );
        put_doc(
            &mut store,
            &bea,
            "bea-garden",
            "note",
            2,
            "everyone",
            &[],
            "garden tomatoes",
            now,
            None,
        );
        put_doc(
            &mut store,
            &bea,
            "bea-only",
            "note",
            2,
            "everyone",
            &[],
            "quokka sanctuary",
            now,
            None,
        );
        let q = |text: &str| Query {
            viewer: 1,
            text: text.into(),
            k: 10,
            now_ms: now,
            ..Default::default()
        };
        let order = |q: &Query| -> Vec<String> {
            let mut keys: Vec<String> = search(&store, q)
                .unwrap()
                .hits
                .iter()
                .map(|h| h.key.clone())
                .collect();
            keys.dedup();
            keys
        };
        // A memory beats a chat with the same words.
        assert_eq!(order(&q("budget planning")), ["1", "chat-budget"]);
        // Her shared note ranks below his own with the same words, and shows when it is alone.
        assert_eq!(order(&q("garden tomatoes")), ["note-garden", "bea-garden"]);
        assert_eq!(order(&q("quokka")), ["bea-only"]);
        // Naming her lifts the down-weight and narrows to her.
        let plain = search(&store, &q("garden tomatoes")).unwrap().hits;
        let named = search(
            &store,
            &Query {
                person: Some(2),
                ..q("garden tomatoes")
            },
        )
        .unwrap()
        .hits;
        assert!(named.iter().all(|h| h.key == "bea-garden"));
        // Named, it is the top keyword hit with no down-weight: 0.9 / 61. Unnamed it is the
        // second hit and carries the softened 0.3.
        let before = plain.iter().find(|h| h.key == "bea-garden").unwrap().score;
        assert!(
            (named[0].score - 0.9 / 61.0).abs() < 1e-9,
            "{}",
            named[0].score
        );
        assert!((before - 0.65 * 0.9 / 62.0).abs() < 1e-9, "{before}");
        // The date window is a hard filter; with under three docs it widens by half, once.
        for (k, days_back) in [
            ("old-a", 100),
            ("old-b", 110),
            ("old-c", 118),
            ("old-d", 140),
        ] {
            let at = now - days_back * day;
            put_doc(
                &mut store,
                &ann,
                k,
                "conversation",
                1,
                "me",
                &[],
                "sailing trip",
                at,
                None,
            );
        }
        let win = |lo: i64, hi: i64| Query {
            window: Some((lo, hi)),
            ..q("sailing")
        };
        fn docs(f: &Found) -> HashSet<&str> {
            f.hits.iter().map(|h| h.key.as_str()).collect()
        }
        let f = search(&store, &win(now - 135 * day, now - 95 * day)).unwrap();
        assert_eq!(
            (docs(&f).len(), f.widened),
            (3, false),
            "three docs inside: no widening"
        );
        // Two docs inside (100 and 110 days back): the window grows by a quarter of its span
        // each side, taking in 118 days back but not 140.
        let f = search(&store, &win(now - 112 * day, now - 88 * day)).unwrap();
        assert!(f.widened);
        assert_eq!(docs(&f), HashSet::from(["old-a", "old-b", "old-c"]));
        let f = search(&store, &win(now - 300 * day, now - 200 * day)).unwrap();
        assert!(f.hits.is_empty() && !f.widened);
    }
}
