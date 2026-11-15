//! The readers' other half: finding what a source holds, deciding who may see it, and handing
//! the indexer docs ready to send. The pure text work stays in [`crate::sources`].
//!
//! A [`Feed`] lists its things cheaply (`key` plus a `stamp` that changes when the thing does),
//! and builds one doc only when the indexer finds the stamp moved. A doc longer than a put may
//! carry is cut into parts here, so the rest of the indexer sees only sendable docs.

mod chatlogs;
mod claudecode;
mod folder;
mod helmchats;

use crate::model::{Kind, Scope, Source};
use crate::sources::{ChunkOut, DocOut};
use helm_core::db::Database;
use helm_core::secrets::looks_like_secret;
use helm_profiles::store as profiles;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// The most chunks the hub takes in one put; a longer doc is sent as parts.
pub const MAX_DOC_CHUNKS: usize = 200;

/// One thing a source holds, and what changes when it does.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub key: String,
    pub stamp: String,
}

/// What a source holds right now. `complete` is false when part of it could not be read (a folder
/// that is missing, an unreadable subfolder): nothing may be concluded from what is absent.
#[derive(Clone, Debug, Default)]
pub struct Listing {
    pub entries: Vec<Entry>,
    pub complete: bool,
    pub note: Option<String>,
}

/// A doc as the hub is sent it.
#[derive(Clone, Debug, PartialEq)]
pub struct PutDoc {
    pub key: String,
    /// The key of the whole thing this was cut from (the key itself when it was not cut).
    pub parent: String,
    /// The content hash with the audience folded in, so a change of owner or readers is a change.
    pub hash: String,
    pub title: String,
    pub locator: String,
    /// The hub person who owns it; `None` is the person this PC's line acts as.
    pub owner: Option<String>,
    /// `me`, `everyone` or `some`.
    pub scope: &'static str,
    pub readers: Vec<String>,
    pub occurred_at: i64,
    pub tier: &'static str,
    pub chunks: Vec<ChunkOut>,
    /// How many passages were held back because they look like a credential (the first part
    /// carries the whole doc's count).
    pub withheld: usize,
}

/// What building one entry gave.
pub enum Built {
    Docs(Vec<PutDoc>),
    /// Not indexed, and why (a person the hub does not know, nothing worth searching).
    Skip(String),
}

pub trait Feed: Send {
    fn list(&self) -> Result<Listing, String>;
    fn build(&self, key: &str) -> Result<Built, String>;
}

/// The places and people a feed reads. Tests point the folders at temporary ones.
pub struct Env {
    pub db: Arc<Database>,
    /// `<home>/.claude/projects`.
    pub claude_projects: Option<PathBuf>,
    /// The profile's `sessions` folder.
    pub sessions: PathBuf,
    /// Chat folders' session ids by session file, keyed by the file's stamp.
    pub(crate) native_cache: Mutex<HashMap<PathBuf, (String, Vec<String>)>>,
}

impl Env {
    pub fn new(db: Arc<Database>, claude_projects: Option<PathBuf>, sessions: PathBuf) -> Self {
        Env {
            db,
            claude_projects,
            sessions,
            native_cache: Mutex::new(HashMap::new()),
        }
    }

    fn profiles(&self) -> Vec<profiles::Profile> {
        self.db
            .snapshot(|tx| profiles::list(tx))
            .unwrap_or_default()
    }

    /// The profile with this id; an empty id is the home profile.
    pub fn profile(&self, id: &str) -> Option<profiles::Profile> {
        let all = self.profiles();
        if id.is_empty() {
            return all.into_iter().find(|p| p.is_home);
        }
        all.into_iter().find(|p| p.id == id)
    }

    pub fn home(&self) -> Option<profiles::Profile> {
        self.profile("")
    }

    /// The hub person a profile's docs are filed under. `Ok(None)` is the person this PC's line
    /// acts as (the home profile); a profile the hub does not know cannot own anything.
    pub fn owner_of(&self, p: &profiles::Profile) -> Result<Option<String>, String> {
        if p.is_home {
            return Ok(None);
        }
        match p.hub_person_id.as_deref().filter(|s| !s.is_empty()) {
            Some(id) => Ok(Some(id.to_owned())),
            None => Err(format!("{} has no person on the hub", p.name)),
        }
    }
}

/// Who may find a doc.
#[derive(Clone, Debug, Default)]
pub struct Audience {
    pub owner: Option<String>,
    pub scope: &'static str,
    pub readers: Vec<String>,
}

impl Audience {
    pub fn private(owner: Option<String>) -> Self {
        Audience {
            owner,
            scope: "me",
            readers: Vec::new(),
        }
    }

    pub fn from_scope(owner: Option<String>, scope: Scope) -> Self {
        Audience {
            owner,
            scope: if scope == Scope::Everyone {
                "everyone"
            } else {
                "me"
            },
            readers: Vec::new(),
        }
    }
}

fn wire_hash(content: &str, a: &Audience, locator: &str, tier: &str) -> String {
    let mut h = Sha256::new();
    for part in [
        content,
        a.owner.as_deref().unwrap_or(""),
        a.scope,
        &a.readers.join(","),
        locator,
        tier,
    ] {
        h.update(part.as_bytes());
        h.update([0]);
    }
    format!("{:x}", h.finalize())
}

/// Turn a built doc into what is sent: passages that look like a credential held back, the
/// audience folded into its hash, and the doc cut into parts of at most [`MAX_DOC_CHUNKS`] chunks
/// when it is longer. The first part keeps the doc's key. A doc with nothing left to send is
/// skipped.
pub fn finish(mut doc: DocOut, a: &Audience, tier: &'static str, locator: &str) -> Built {
    let before = doc.chunks.len();
    doc.chunks
        .retain(|c| !looks_like_secret(&c.text) && !looks_like_secret(&c.heading));
    let withheld = before - doc.chunks.len();
    if doc.chunks.is_empty() {
        return Built::Skip(if withheld > 0 {
            "everything in it looks like a credential".into()
        } else {
            "empty".into()
        });
    }
    if looks_like_secret(&doc.title) {
        doc.title = doc.key.clone();
    }
    let total = doc.chunks.len().div_ceil(MAX_DOC_CHUNKS).max(1);
    let key = doc.key.clone();
    let mut out = Vec::with_capacity(total);
    for (i, group) in doc.chunks.chunks(MAX_DOC_CHUNKS).enumerate() {
        let mut chunks = group.to_vec();
        for (n, c) in chunks.iter_mut().enumerate() {
            c.ord = n;
        }
        let (part_key, title) = if i == 0 {
            (key.clone(), doc.title.clone())
        } else {
            (
                format!("{key}#{}", i + 1),
                format!("{} (part {})", doc.title, i + 1),
            )
        };
        let content = if total == 1 && withheld == 0 {
            doc.hash.clone()
        } else {
            format!("{}:{i}:{withheld}", doc.hash)
        };
        let at = chunks.first().map_or(doc.occurred_from, |c| c.at);
        out.push(PutDoc {
            hash: wire_hash(&content, a, locator, tier),
            key: part_key,
            parent: key.clone(),
            title,
            locator: locator.to_owned(),
            owner: a.owner.clone(),
            scope: a.scope,
            readers: a.readers.clone(),
            occurred_at: if i == 0 { doc.occurred_to.max(at) } else { at },
            tier,
            chunks,
            withheld: if i == 0 { withheld } else { 0 },
        });
    }
    Built::Docs(out)
}

/// `mtime:size` of a file, the cheap fingerprint of it.
pub(crate) fn file_stamp(meta: &std::fs::Metadata) -> String {
    let mtime = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_millis());
    format!("{mtime}:{}", meta.len())
}

pub(crate) fn mtime_ms(meta: &std::fs::Metadata) -> i64 {
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_millis() as i64)
}

/// The feed that reads `src`.
pub fn feed_for(src: &Source, env: &Arc<Env>) -> Box<dyn Feed> {
    match src.kind {
        Kind::Folder => Box::new(folder::FolderFeed::new(src.clone(), env.clone())),
        Kind::HelmChats => Box::new(helmchats::HelmChatsFeed::new(env.clone())),
        Kind::ClaudeCode => Box::new(claudecode::ClaudeCodeFeed::new(env.clone())),
        Kind::ChatLogs => Box::new(chatlogs::ChatLogsFeed::new(src.clone(), env.clone())),
    }
}

// ---- globs ---------------------------------------------------------------------------------

/// `*` (not across `/`), `**` (across it) and `?` over a `/`-separated relative path.
fn glob(pat: &[char], text: &[char]) -> bool {
    match pat.split_first() {
        None => text.is_empty(),
        Some(('*', rest)) => {
            if rest.first() == Some(&'*') {
                let rest = &rest[1..];
                // `**/` also matches nothing at all.
                let rest = rest.strip_prefix(&['/']).unwrap_or(rest);
                (0..=text.len()).any(|i| glob(rest, &text[i..]))
            } else {
                (0..=text.len())
                    .take_while(|i| *i == 0 || text[*i - 1] != '/')
                    .any(|i| glob(rest, &text[i..]))
            }
        }
        Some(('?', rest)) => text
            .split_first()
            .is_some_and(|(c, t)| *c != '/' && glob(rest, t)),
        Some((c, rest)) => text
            .split_first()
            .is_some_and(|(t, tail)| t == c && glob(rest, tail)),
    }
}

/// Whether `rel` (a `/`-separated path under the folder) matches `pattern`. A pattern with no
/// `/` matches a file or folder of that name anywhere, and a pattern that matches a folder takes
/// everything under it.
pub fn glob_match(pattern: &str, rel: &str) -> bool {
    let pattern = pattern.trim().trim_end_matches('/');
    if pattern.is_empty() {
        return false;
    }
    let text: Vec<char> = rel.chars().collect();
    let hit = |p: &str| glob(&p.chars().collect::<Vec<_>>(), &text);
    if pattern.contains('/') {
        let pattern = pattern.trim_start_matches('/');
        return hit(pattern) || hit(&format!("{pattern}/**"));
    }
    let parts: Vec<&str> = rel.split('/').collect();
    parts.iter().any(|part| {
        glob(
            &pattern.chars().collect::<Vec<_>>(),
            &part.chars().collect::<Vec<_>>(),
        )
    })
}

pub(crate) fn rel_path(root: &Path, p: &Path) -> Option<String> {
    let rel = p.strip_prefix(root).ok()?;
    let parts: Vec<String> = rel
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect();
    Some(parts.join("/"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn globs_match_the_way_a_gitignore_does() {
        let rows = [
            ("*.md", "a.md", true),
            ("*.md", "sub/a.md", true),
            ("sub/*.md", "sub/a.md", true),
            ("sub/*.md", "sub/deep/a.md", false),
            ("sub/**", "sub/deep/a.md", true),
            ("**/a.md", "a.md", true),
            ("Study Kit", "Study Kit/x/y.md", true),
            ("Study Kit/", "Notes/Study Kit/y.md", true),
            ("node_modules", "a/node_modules/b.md", true),
            ("draft?.md", "draft1.md", true),
            ("draft?.md", "draft12.md", false),
            ("", "a.md", false),
        ];
        for (pat, rel, want) in rows {
            assert_eq!(glob_match(pat, rel), want, "{pat} on {rel}");
        }
    }

    #[test]
    fn a_long_doc_is_sent_as_parts_and_the_audience_is_part_of_the_hash() {
        let chunks = |n: usize| -> Vec<ChunkOut> {
            (0..n)
                .map(|i| ChunkOut {
                    ord: i,
                    heading: String::new(),
                    text: format!("t{i}"),
                    at: 1000 + i as i64,
                    anchor: None,
                })
                .collect()
        };
        let doc = |n| DocOut {
            key: "k".into(),
            hash: "h".into(),
            title: "T".into(),
            locator: "k".into(),
            occurred_from: 1000,
            occurred_to: 2000,
            chunks: chunks(n),
        };
        let a = Audience::private(None);
        let docs = |b: Built| match b {
            Built::Docs(d) => d,
            Built::Skip(why) => panic!("skipped: {why}"),
        };
        let one = docs(finish(doc(5), &a, "note", "loc"));
        assert_eq!(one.len(), 1);
        assert_eq!((one[0].key.as_str(), one[0].parent.as_str()), ("k", "k"));
        let parts = docs(finish(doc(450), &a, "note", "loc"));
        let keys: Vec<_> = parts.iter().map(|p| p.key.as_str()).collect();
        assert_eq!(keys, ["k", "k#2", "k#3"]);
        assert!(parts
            .iter()
            .all(|p| p.parent == "k" && p.chunks.len() <= 200));
        assert_eq!(parts[2].title, "T (part 3)");
        assert_eq!(parts[2].chunks[0].ord, 0);
        let shared = Audience::from_scope(None, Scope::Everyone);
        assert_ne!(
            one[0].hash,
            docs(finish(doc(5), &shared, "note", "loc"))[0].hash,
            "a change of audience is a change"
        );
    }
}
