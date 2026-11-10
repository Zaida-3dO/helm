//! Knowledge sync: each layer's files, kept equal with the household hub.
//!
//! The Everyone layer and every linked person's layer are folders (`rules/`, `skills/<name>/…`,
//! `agents/`, see [`crate::layers`]). The hub keeps them file by file, each change with a number
//! from one hub-wide counter (`seq`). [`sync_once`] makes one pass:
//!
//! 1. **Push.** A local file whose digest differs from the one the last pass recorded is sent with
//!    the `seq` it was based on. A file gone since then is sent as a deletion. If the hub's copy
//!    moved on in the meantime the hub keeps its copy and says so: the local one is parked under
//!    `<profile>/knowledge/conflicts/<layer>/<time>/…` and the hub's copy takes its place, so a stale
//!    edit never silently loads and never silently vanishes.
//! 2. **Pull.** Everything the hub changed after the recorded cursor is written (atomically) or
//!    removed. A local copy edited since the last pass is parked first, never overwritten.
//!
//! A hub layer is `everyone` or `person:<id>`. A local profile layer takes part only when its
//! profile is linked to a hub person ([`links`]); the rest stay on this PC. Linked external skill
//! folders, `.trash/`, `.migrated` and the conflicts folder are never synced, and neither is
//! anything whose name starts with a dot. The book of what was synced is
//! `<profile>/knowledge/.sync.json`; losing it only costs a pass that treats every file as new.
//!
//! The hub is reached through [`HubKnowledge`], which the relay implements over the line.

use crate::layers::{self, Layer};
use helm_ipc::AppEvent;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Mutex;

pub type BoxFut<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// The most one file may hold, here and at the hub.
pub const MAX_FILE: u64 = 2 * 1024 * 1024;
/// How many files one pull asks for.
pub const PULL_LIMIT: usize = 200;
/// The book of what was synced, inside the knowledge folder.
pub const STATE_FILE: &str = ".sync.json";
/// Where the local copy of a file goes when the hub's copy wins.
pub const CONFLICTS: &str = "conflicts";

const MAX_PATH: usize = 200;
const MAX_DEPTH: usize = 8;
const MAX_FILES: usize = 5000;
const KINDS: [&str; 3] = ["rules", "skills", "agents"];

/// One file as the hub sends it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HubFile {
    /// `everyone` or `person:<id>`.
    pub layer: String,
    pub path: String,
    pub seq: i64,
    pub sha256: String,
    pub deleted: bool,
    /// The decoded content; `None` for a deletion.
    pub body: Option<Vec<u8>>,
}

/// The answer to a pull.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pulled {
    pub files: Vec<HubFile>,
    /// The last `seq` sent; ask again from it while `more` is true.
    pub cursor: i64,
    pub more: bool,
}

/// A file (or its deletion) for the hub to keep.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PutRequest {
    pub layer: String,
    pub path: String,
    /// The `seq` this change is based on; 0 for a file believed to be new.
    pub base_seq: i64,
    pub sha256: String,
    pub deleted: bool,
    pub body: Option<Vec<u8>>,
}

/// What the hub said to a put.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PutReply {
    Saved {
        seq: i64,
    },
    /// The stored copy changed since `base_seq` and was kept.
    Conflict {
        seq: i64,
        sha256: String,
        deleted: bool,
    },
    /// The hub will not store it; the sentence says why.
    Refused(String),
}

/// The hub's knowledge store as the sync sees it. `Err` means the hub could not be reached or
/// answered: the pass stops and tries again later.
pub trait HubKnowledge: Send + Sync {
    fn pull(&self, since: i64, limit: usize) -> BoxFut<'_, Result<Pulled, String>>;
    fn put(&self, req: PutRequest) -> BoxFut<'_, Result<PutReply, String>>;
}

/// A local layer and the hub layer it is kept equal with.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LayerLink {
    pub local: Layer,
    pub hub: String,
}

/// The layers that take part: Everyone, and each profile linked to a hub person. `profiles` is
/// `(local profile id, hub person id)`; an unlinked profile, or one whose person id is not the
/// hub's plain number, is left out.
pub fn links(profiles: &[(String, Option<String>)]) -> Vec<LayerLink> {
    let mut out = vec![LayerLink {
        local: Layer::Everyone,
        hub: "everyone".to_owned(),
    }];
    let mut seen = BTreeSet::new();
    for (id, person) in profiles {
        let Some(person) = person.as_deref() else {
            continue;
        };
        let plain = !person.is_empty()
            && person.bytes().all(|c| c.is_ascii_digit())
            && !person.starts_with('0');
        if !plain || !layers::valid_profile_id(id) || !seen.insert(person.to_owned()) {
            continue;
        }
        out.push(LayerLink {
            local: Layer::Profile(id.clone()),
            hub: format!("person:{person}"),
        });
    }
    out
}

/// What one pass did.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Report {
    pub pushed: usize,
    pub pulled: usize,
    pub removed: usize,
    /// Local copies parked in the conflicts folder, as `layer/path`.
    pub parked: Vec<String>,
    /// Local layers whose files changed here, for `knowledge-changed`.
    pub changed: Vec<Layer>,
    /// Files skipped, each with a plain sentence.
    pub errors: Vec<String>,
}

impl Report {
    fn touched(&mut self, layer: &Layer) {
        if !self.changed.contains(layer) {
            self.changed.push(layer.clone());
        }
    }
}

/// Whether `path` may be stored at the hub: relative with `/` separators, under `rules/`,
/// `skills/<name>/` or `agents/`, no segment starting with a dot, and nothing a Windows disk
/// cannot hold. The hub applies the same rule.
pub fn valid_path(path: &str) -> bool {
    if path.is_empty() || path.len() > MAX_PATH {
        return false;
    }
    let segments: Vec<&str> = path.split('/').collect();
    if segments.len() > MAX_DEPTH {
        return false;
    }
    for s in &segments {
        let bad_char = |c: char| c.is_control() || "\\:*?\"<>|".contains(c);
        if s.is_empty()
            || s.starts_with('.')
            || s.ends_with('.')
            || s.ends_with(' ')
            || s.chars().any(bad_char)
            || reserved_name(s)
        {
            return false;
        }
    }
    let shallowest = match segments[0] {
        "rules" | "agents" => 2,
        "skills" => 3,
        _ => return false,
    };
    segments.len() >= shallowest
}

fn reserved_name(segment: &str) -> bool {
    let stem = segment.split('.').next().unwrap_or("").to_ascii_lowercase();
    matches!(stem.as_str(), "con" | "prn" | "aux" | "nul")
        || ["com", "lpt"].iter().any(|p| {
            stem.strip_prefix(p)
                .is_some_and(|n| matches!(n.as_bytes(), [b'1'..=b'9']))
        })
}

// ---- the book ---------------------------------------------------------------------------------

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
struct Known {
    seq: i64,
    sha256: String,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct State {
    #[serde(default)]
    version: u32,
    /// The last hub `seq` pulled.
    #[serde(default)]
    cursor: i64,
    /// The hub layers the cursor covers; one that is new to this list is pulled from the start.
    #[serde(default)]
    layers: Vec<String>,
    /// `<hub layer>/<path>` → what the hub had when it was last in step with this file.
    #[serde(default)]
    files: BTreeMap<String, Known>,
}

fn state_path(profile_dir: &Path) -> PathBuf {
    layers::knowledge_dir(profile_dir).join(STATE_FILE)
}

impl State {
    fn load(profile_dir: &Path) -> State {
        helm_core::privfs::read_capped(&state_path(profile_dir), 16 * 1024 * 1024)
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default()
    }

    fn save(&mut self, profile_dir: &Path) {
        self.version = 1;
        let Ok(bytes) = serde_json::to_vec(self) else {
            return;
        };
        let write = helm_core::privfs::create_private_dir_all(&layers::knowledge_dir(profile_dir))
            .and_then(|()| helm_core::privfs::write_atomic(&state_path(profile_dir), &bytes));
        if let Err(e) = write {
            log::warn!("knowledge sync: the book could not be saved: {e}");
        }
    }
}

fn key(hub: &str, path: &str) -> String {
    format!("{hub}/{path}")
}

// ---- local files ------------------------------------------------------------------------------

struct Local {
    rel: String,
    abs: PathBuf,
    sha: String,
}

#[derive(Default)]
struct Scan {
    files: Vec<Local>,
    /// Files that exist but could not be read or are too big: neither pushed nor treated as gone.
    unsure: BTreeSet<String>,
    /// A folder could not be listed, so a missing file may only look missing.
    partial: bool,
}

fn scan(root: &Path) -> Scan {
    let mut out = Scan::default();
    for kind in KINDS {
        walk(&root.join(kind), kind, 1, &mut out);
    }
    out
}

fn walk(dir: &Path, rel: &str, depth: usize, out: &mut Scan) {
    let read = match fs::read_dir(dir) {
        Ok(r) => r,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return,
        Err(_) => {
            out.partial = true;
            return;
        }
    };
    for e in read.flatten() {
        let Some(name) = e.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        if name.starts_with('.') {
            continue;
        }
        let Ok(meta) = fs::symlink_metadata(e.path()) else {
            continue;
        };
        if helm_platform::fs::is_link_like(&meta) {
            continue;
        }
        let here = format!("{rel}/{name}");
        if meta.is_dir() {
            if depth < MAX_DEPTH {
                walk(&e.path(), &here, depth + 1, out);
            }
        } else if meta.is_file() {
            if !valid_path(&here) {
                continue;
            }
            if out.files.len() >= MAX_FILES {
                out.partial = true;
                return;
            }
            match helm_core::privfs::read_capped(&e.path(), MAX_FILE) {
                Ok(bytes) => out.files.push(Local {
                    rel: here,
                    abs: e.path(),
                    sha: helm_core::sha256_hex(&bytes),
                }),
                Err(_) => {
                    out.unsure.insert(here);
                }
            }
        }
    }
}

/// Where `rel` goes under `root`, refusing a path that passes through a link.
fn safe_target(root: &Path, rel: &str) -> Option<PathBuf> {
    let mut p = root.to_path_buf();
    for seg in rel.split('/') {
        p.push(seg);
        if let Ok(meta) = fs::symlink_metadata(&p) {
            if helm_platform::fs::is_link_like(&meta) {
                return None;
            }
        }
    }
    Some(p)
}

/// Remove folders left empty by a deleted file, up to (not including) the `rules`, `skills` or
/// `agents` folder.
fn prune(abs: &Path, kind_dir: &Path) {
    let mut cur = abs.parent();
    while let Some(d) = cur {
        if d == kind_dir || !d.starts_with(kind_dir) || fs::remove_dir(d).is_err() {
            break;
        }
        cur = d.parent();
    }
}

fn layer_dir_name(hub: &str) -> String {
    hub.replace(':', "-")
}

// ---- one pass ---------------------------------------------------------------------------------

struct Pass<'a> {
    dir: &'a Path,
    links: &'a [LayerLink],
    hub: &'a dyn HubKnowledge,
    st: State,
    rep: Report,
    /// The lowest `seq` (minus one) the pull must start from to fetch a copy the hub kept.
    rewind: Option<i64>,
}

/// Make one pass over `links` against `hub`. `Err` only when the hub could not be reached; what
/// was done up to then stays done and is recorded.
pub async fn sync_once(
    profile_dir: &Path,
    links: &[LayerLink],
    hub: &dyn HubKnowledge,
) -> Result<Report, String> {
    let mut pass = Pass {
        dir: profile_dir,
        links,
        hub,
        st: State::load(profile_dir),
        rep: Report::default(),
        rewind: None,
    };
    let result = pass.run().await;
    pass.st.save(profile_dir);
    result.map(|()| pass.rep)
}

impl Pass<'_> {
    async fn run(&mut self) -> Result<(), String> {
        // A first pass (nothing in the book yet: a PC that just joined) takes in what the
        // household has before it offers anything, so an example this PC just copied in does not
        // bring back one the household removed.
        if self.st.cursor == 0 && self.st.files.is_empty() {
            self.pull().await?;
        }
        for link in self.links {
            self.push(link).await?;
        }
        self.pull().await
    }

    fn park(&mut self, link: &LayerLink, rel: &str, bytes: &[u8]) {
        let dest = layers::knowledge_dir(self.dir)
            .join(CONFLICTS)
            .join(layer_dir_name(&link.hub))
            .join(helm_core::now_ms().to_string())
            .join(rel);
        let saved = dest
            .parent()
            .map(helm_core::privfs::create_private_dir_all)
            .unwrap_or(Ok(()))
            .and_then(|()| helm_core::privfs::write_atomic(&dest, bytes));
        match saved {
            Ok(()) => self
                .rep
                .parked
                .push(format!("{}/{rel}", layer_dir_name(&link.hub))),
            Err(e) => self.rep.errors.push(format!(
                "A changed copy of {rel} could not be set aside: {e}"
            )),
        }
    }

    fn rewind_to(&mut self, seq: i64) {
        let from = (seq - 1).max(0);
        self.rewind = Some(self.rewind.map_or(from, |r| r.min(from)));
    }

    async fn push(&mut self, link: &LayerLink) -> Result<(), String> {
        let root = link.local.root(self.dir);
        let found = scan(&root);
        let mut present: BTreeSet<String> = found.unsure.clone();
        for lf in &found.files {
            present.insert(lf.rel.clone());
            let k = key(&link.hub, &lf.rel);
            let base = match self.st.files.get(&k) {
                Some(known) if known.sha256 == lf.sha => continue,
                Some(known) => known.seq,
                None => 0,
            };
            let Ok(bytes) = helm_core::privfs::read_capped(&lf.abs, MAX_FILE) else {
                continue;
            };
            // Read again for the digest: the file may have changed since the scan.
            let sha = helm_core::sha256_hex(&bytes);
            let reply = self
                .hub
                .put(PutRequest {
                    layer: link.hub.clone(),
                    path: lf.rel.clone(),
                    base_seq: base,
                    sha256: sha.clone(),
                    deleted: false,
                    body: Some(bytes.clone()),
                })
                .await?;
            match reply {
                PutReply::Saved { seq } => {
                    self.st.files.insert(k, Known { seq, sha256: sha });
                    self.rep.pushed += 1;
                }
                PutReply::Conflict {
                    seq,
                    sha256,
                    deleted,
                } => {
                    if !deleted && sha256.eq_ignore_ascii_case(&sha) {
                        self.st.files.insert(k, Known { seq, sha256: sha });
                        continue;
                    }
                    // The hub's copy stays; ours is set aside and the hub's takes its place.
                    self.park(link, &lf.rel, &bytes);
                    if fs::remove_file(&lf.abs).is_ok() {
                        let kind = lf.rel.split('/').next().unwrap_or("");
                        prune(&lf.abs, &root.join(kind));
                        self.rep.touched(&link.local);
                    }
                    self.st.files.remove(&k);
                    self.rewind_to(seq);
                }
                PutReply::Refused(why) => self.rep.errors.push(format!("{}: {why}", lf.rel)),
            }
        }

        // Files that were synced and are gone now were deleted here.
        if found.partial || !root.is_dir() {
            return Ok(());
        }
        let prefix = format!("{}/", link.hub);
        let gone: Vec<(String, i64)> = self
            .st
            .files
            .iter()
            .filter_map(|(k, known)| {
                let rel = k.strip_prefix(&prefix)?;
                (!present.contains(rel)).then(|| (rel.to_owned(), known.seq))
            })
            .collect();
        for (rel, base) in gone {
            let reply = self
                .hub
                .put(PutRequest {
                    layer: link.hub.clone(),
                    path: rel.clone(),
                    base_seq: base,
                    sha256: String::new(),
                    deleted: true,
                    body: None,
                })
                .await?;
            let k = key(&link.hub, &rel);
            match reply {
                PutReply::Saved { .. } => {
                    self.st.files.remove(&k);
                    self.rep.pushed += 1;
                }
                // Someone changed it after this device last saw it: their copy comes back.
                PutReply::Conflict { seq, .. } => {
                    self.st.files.remove(&k);
                    self.rewind_to(seq);
                }
                PutReply::Refused(_) => {
                    self.st.files.remove(&k);
                }
            }
        }
        Ok(())
    }

    async fn pull(&mut self) -> Result<(), String> {
        let hubs: Vec<String> = self.links.iter().map(|l| l.hub.clone()).collect();
        // A layer this book has not covered yet may have files older than the cursor.
        if hubs.iter().any(|h| !self.st.layers.contains(h)) {
            self.st.cursor = 0;
        }
        if let Some(r) = self.rewind {
            self.st.cursor = self.st.cursor.min(r);
        }
        loop {
            let page = self.hub.pull(self.st.cursor, PULL_LIMIT).await?;
            for f in page.files {
                self.apply(f);
            }
            self.st.cursor = self.st.cursor.max(page.cursor);
            if !page.more {
                break;
            }
        }
        self.st.layers = hubs;
        Ok(())
    }

    /// A file this book already keeps in `layer` whose path differs from `path` only in letter
    /// case. Only where the disk ignores case (Windows).
    fn case_twin(&self, layer: &str, path: &str) -> Option<String> {
        if !cfg!(windows) {
            return None;
        }
        let prefix = format!("{layer}/");
        let lower = path.to_lowercase();
        self.st
            .files
            .keys()
            .filter_map(|k| k.strip_prefix(&prefix))
            .find(|p| *p != path && p.to_lowercase() == lower)
            .map(str::to_owned)
    }

    fn apply(&mut self, f: HubFile) {
        let Some(link) = self.links.iter().find(|l| l.hub == f.layer).cloned() else {
            return;
        };
        if !valid_path(&f.path) {
            self.rep
                .errors
                .push("The hub sent a file with a name that cannot be used here.".to_owned());
            return;
        }
        // Windows keeps one file for two names that differ only in letter case, so a second such
        // name from the hub would overwrite (or delete) the first.
        if let Some(twin) = self.case_twin(&f.layer, &f.path) {
            self.rep.errors.push(format!(
                "{} differs from {twin} only in letter case, which this PC cannot keep apart, so it was not written.",
                f.path
            ));
            return;
        }
        let root = link.local.root(self.dir);
        let Some(abs) = safe_target(&root, &f.path) else {
            self.rep.errors.push(format!(
                "{} passes through a link and was not written.",
                f.path
            ));
            return;
        };
        let kind_dir = root.join(f.path.split('/').next().unwrap_or(""));
        let k = key(&f.layer, &f.path);
        let known = self.st.files.get(&k).map(|x| x.sha256.clone());
        let exists = fs::symlink_metadata(&abs).is_ok();
        let local = if exists {
            match helm_core::privfs::read_capped(&abs, MAX_FILE) {
                Ok(b) => Some(b),
                Err(_) => {
                    self.rep.errors.push(format!(
                        "{} could not be read, so the hub's copy was not written over it.",
                        f.path
                    ));
                    return;
                }
            }
        } else {
            None
        };
        let local_sha = local.as_deref().map(helm_core::sha256_hex);
        // A copy edited since the last pass is set aside before the hub's takes over. An example
        // exactly as Helm ships it is nobody's edit.
        let edited = local_sha.is_some()
            && local_sha != known
            && !local
                .as_deref()
                .is_some_and(|b| crate::examples::is_shipped(&f.path, b));

        if f.deleted {
            if let Some(bytes) = &local {
                if edited {
                    self.park(&link, &f.path, bytes);
                }
                if fs::remove_file(&abs).is_ok() {
                    prune(&abs, &kind_dir);
                    self.rep.removed += 1;
                    self.rep.touched(&link.local);
                }
            }
            self.st.files.remove(&k);
            return;
        }

        let Some(body) = f.body else {
            self.rep
                .errors
                .push(format!("The hub sent {} without its content.", f.path));
            return;
        };
        let sha = helm_core::sha256_hex(&body);
        if body.len() as u64 > MAX_FILE
            || (!f.sha256.is_empty() && !f.sha256.eq_ignore_ascii_case(&sha))
        {
            self.rep
                .errors
                .push(format!("{} arrived damaged and was not written.", f.path));
            return;
        }
        // Rules, skills and agents are read by the model as instructions: text that hides
        // something with invisible or direction-changing characters is refused, as it is when
        // saved on this PC.
        if std::str::from_utf8(&body).is_ok_and(helm_core::hidden::hides) {
            self.rep.errors.push(format!(
                "{} holds invisible or direction-changing characters and was not written.",
                f.path
            ));
            return;
        }
        if local_sha.as_deref() == Some(sha.as_str()) {
            self.st.files.insert(
                k,
                Known {
                    seq: f.seq,
                    sha256: sha,
                },
            );
            return;
        }
        if let (true, Some(bytes)) = (edited, &local) {
            self.park(&link, &f.path, bytes);
        }
        let written = abs
            .parent()
            .map(helm_core::privfs::create_private_dir_all)
            .unwrap_or(Ok(()))
            .and_then(|()| helm_core::privfs::write_atomic(&abs, &body));
        match written {
            Ok(()) => {
                self.st.files.insert(
                    k,
                    Known {
                        seq: f.seq,
                        sha256: sha,
                    },
                );
                self.rep.pulled += 1;
                self.rep.touched(&link.local);
            }
            Err(e) => self
                .rep
                .errors
                .push(format!("{} could not be written: {e}", f.path)),
        }
    }
}

// ---- old parked copies ------------------------------------------------------------------------

/// How long a parked copy is kept before [`prune_conflicts`] removes it.
pub const CONFLICTS_KEPT_MS: i64 = 30 * 24 * 60 * 60 * 1000;

/// Remove parked copies older than [`CONFLICTS_KEPT_MS`]: every
/// `<knowledge>/conflicts/<layer>/<ms>/` folder whose `<ms>` is that old, then any layer folder
/// left empty. A link (or junction) is never followed: one found there is removed as a link, and
/// a folder whose name is not a time is left alone. Returns how many folders were removed.
pub fn prune_conflicts(profile_dir: &Path, now: i64) -> usize {
    let base = layers::knowledge_dir(profile_dir).join(CONFLICTS);
    let mut removed = 0;
    for layer in fs::read_dir(&base).into_iter().flatten().flatten() {
        let layer_dir = layer.path();
        let Ok(meta) = fs::symlink_metadata(&layer_dir) else {
            continue;
        };
        if helm_platform::fs::is_link_like(&meta) || !meta.is_dir() {
            continue;
        }
        for parked in fs::read_dir(&layer_dir).into_iter().flatten().flatten() {
            let at = parked
                .file_name()
                .to_str()
                .and_then(|n| n.parse::<i64>().ok());
            if at.is_some_and(|at| now - at > CONFLICTS_KEPT_MS) && remove_tree(&parked.path()) {
                removed += 1;
            }
        }
        // Gone only when empty.
        let _ = fs::remove_dir(&layer_dir);
    }
    removed
}

/// Remove a folder and what is in it without following a link: a link is removed itself.
fn remove_tree(p: &Path) -> bool {
    let Ok(meta) = fs::symlink_metadata(p) else {
        return false;
    };
    if helm_platform::fs::is_link_like(&meta) {
        return fs::remove_dir(p).is_ok() || fs::remove_file(p).is_ok();
    }
    if !meta.is_dir() {
        return fs::remove_file(p).is_ok();
    }
    for e in fs::read_dir(p).into_iter().flatten().flatten() {
        remove_tree(&e.path());
    }
    fs::remove_dir(p).is_ok()
}

// ---- what wakes the sync ------------------------------------------------------------------------

/// Told whenever a knowledge layer changes on this PC; the relay's sync loop hangs a hook on it.
/// The app's event tap feeds it ([`Trigger::observe`]).
#[derive(Default)]
pub struct Trigger {
    hooks: Mutex<Vec<Box<dyn Fn() + Send + Sync>>>,
}

impl Trigger {
    pub fn on_change(&self, hook: impl Fn() + Send + Sync + 'static) {
        self.hooks
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .push(Box::new(hook));
    }

    pub fn poke(&self) {
        for h in self.hooks.lock().unwrap_or_else(|p| p.into_inner()).iter() {
            h();
        }
    }

    pub fn observe(&self, ev: &AppEvent) {
        if ev.kind == crate::ops::KNOWLEDGE_EVENT {
            self.poke();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    /// A stand-in for the hub's store: one counter, a conflict on a stale base, tombstones.
    #[allow(clippy::type_complexity)]
    #[derive(Default)]
    struct FakeHub {
        files: Mutex<BTreeMap<(String, String), (i64, String, bool, Option<Vec<u8>>)>>,
        counter: Mutex<i64>,
    }

    impl FakeHub {
        /// A file the hub holds, ignoring its own rules (to test what a Helm refuses to write).
        fn inject(&self, layer: &str, path: &str, body: &[u8]) {
            let mut seq = self.counter.lock().unwrap();
            *seq += 1;
            self.files.lock().unwrap().insert(
                (layer.to_owned(), path.to_owned()),
                (
                    *seq,
                    helm_core::sha256_hex(body),
                    false,
                    Some(body.to_vec()),
                ),
            );
        }

        fn has(&self, layer: &str, path: &str) -> bool {
            self.files
                .lock()
                .unwrap()
                .get(&(layer.to_owned(), path.to_owned()))
                .is_some_and(|f| !f.2)
        }
    }

    impl HubKnowledge for FakeHub {
        fn pull(&self, since: i64, limit: usize) -> BoxFut<'_, Result<Pulled, String>> {
            let mut rows: Vec<HubFile> = self
                .files
                .lock()
                .unwrap()
                .iter()
                .filter(|(_, f)| f.0 > since)
                .map(|((layer, path), f)| HubFile {
                    layer: layer.clone(),
                    path: path.clone(),
                    seq: f.0,
                    sha256: f.1.clone(),
                    deleted: f.2,
                    body: f.3.clone(),
                })
                .collect();
            rows.sort_by_key(|f| f.seq);
            let more = rows.len() > limit;
            rows.truncate(limit);
            let cursor = rows.last().map_or(since, |f| f.seq);
            Box::pin(async move {
                Ok(Pulled {
                    files: rows,
                    cursor,
                    more,
                })
            })
        }

        fn put(&self, req: PutRequest) -> BoxFut<'_, Result<PutReply, String>> {
            let mut files = self.files.lock().unwrap();
            let k = (req.layer.clone(), req.path.clone());
            let sha = if req.deleted {
                String::new()
            } else {
                helm_core::sha256_hex(req.body.as_deref().unwrap_or_default())
            };
            let reply = match files.get(&k) {
                Some((seq, stored, deleted, _)) if *deleted == req.deleted && *stored == sha => {
                    PutReply::Saved { seq: *seq }
                }
                Some((seq, stored, deleted, _))
                    if req.base_seq != *seq && !(*deleted && req.base_seq == 0) =>
                {
                    PutReply::Conflict {
                        seq: *seq,
                        sha256: stored.clone(),
                        deleted: *deleted,
                    }
                }
                _ => {
                    let mut counter = self.counter.lock().unwrap();
                    *counter += 1;
                    files.insert(k, (*counter, sha, req.deleted, req.body));
                    PutReply::Saved { seq: *counter }
                }
            };
            Box::pin(async move { Ok(reply) })
        }
    }

    fn write(dir: &Path, layer: &Layer, rel: &str, text: &str) {
        let p = layer.root(dir).join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, text).unwrap();
    }

    fn read(dir: &Path, layer: &Layer, rel: &str) -> Option<String> {
        fs::read_to_string(layer.root(dir).join(rel)).ok()
    }

    fn parked(dir: &Path) -> Vec<String> {
        let base = layers::knowledge_dir(dir).join(CONFLICTS);
        let mut out = Vec::new();
        let mut stack = vec![base];
        while let Some(d) = stack.pop() {
            for e in fs::read_dir(&d).into_iter().flatten().flatten() {
                if e.path().is_dir() {
                    stack.push(e.path());
                } else {
                    out.push(fs::read_to_string(e.path()).unwrap());
                }
            }
        }
        out.sort();
        out
    }

    #[tokio::test]
    async fn two_hosts_keep_their_layers_equal_through_the_hub() {
        let hub = Arc::new(FakeHub::default());
        let (a, b) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let (a, b) = (a.path(), b.path());
        let ev = Layer::Everyone;
        // The same person is profile `pa` on host A and `pb` on host B; `local-only` has no link.
        let link_a = links(&[
            ("pa".into(), Some("7".into())),
            ("local-only".into(), None),
            ("odd".into(), Some("07".into())),
        ]);
        let link_b = links(&[("pb".into(), Some("7".into()))]);
        assert_eq!(
            link_a.iter().map(|l| l.hub.as_str()).collect::<Vec<_>>(),
            ["everyone", "person:7"],
            "an unlinked profile and an odd person id take no part"
        );
        let (pa, pb) = (Layer::Profile("pa".into()), Layer::Profile("pb".into()));

        // Push a new rule, a skill and a person's rule from A; B pulls them, into its own folder
        // for the same person.
        write(a, &ev, "rules/style.md", "# Style\nBe brief.");
        write(
            a,
            &ev,
            "skills/deploy/SKILL.md",
            "---\nname: deploy\n---\nsteps",
        );
        write(a, &ev, "skills/deploy/notes/extra.txt", "extra");
        write(a, &pa, "rules/me.md", "# Me\nPrefers short answers.");
        write(
            a,
            &Layer::Profile("local-only".into()),
            "rules/x.md",
            "# X\nstays",
        );
        write(a, &ev, ".trash/old/item", "never synced");
        let r = sync_once(a, &link_a, &*hub).await.unwrap();
        assert_eq!((r.pushed, r.pulled), (4, 0));
        assert!(!hub.has("everyone", ".trash/old/item") && !hub.has("person:7", "rules/x.md"));
        let r = sync_once(b, &link_b, &*hub).await.unwrap();
        assert_eq!((r.pulled, r.pushed), (4, 0));
        assert_eq!(r.changed, [ev.clone(), pb.clone()]);
        assert_eq!(
            read(b, &ev, "rules/style.md").unwrap(),
            "# Style\nBe brief."
        );
        assert_eq!(
            read(b, &ev, "skills/deploy/notes/extra.txt").unwrap(),
            "extra"
        );
        assert_eq!(
            read(b, &pb, "rules/me.md").unwrap(),
            "# Me\nPrefers short answers."
        );
        // A second pass finds nothing to do, and A hears nothing new of its own pushes.
        assert_eq!(
            sync_once(b, &link_b, &*hub).await.unwrap(),
            Report::default()
        );
        let r = sync_once(a, &link_a, &*hub).await.unwrap();
        assert_eq!((r.pushed, r.pulled, r.changed.len()), (0, 0, 0));

        // A stale edit: both change the rule; A lands first, so B's copy is parked and A's wins.
        write(a, &ev, "rules/style.md", "# Style\nBe brief, always.");
        write(b, &ev, "rules/style.md", "# Style\nBe long.");
        sync_once(a, &link_a, &*hub).await.unwrap();
        let r = sync_once(b, &link_b, &*hub).await.unwrap();
        assert_eq!(r.parked, ["everyone/rules/style.md"]);
        assert_eq!(
            read(b, &ev, "rules/style.md").unwrap(),
            "# Style\nBe brief, always."
        );
        assert_eq!(parked(b), ["# Style\nBe long."]);
        assert!(parked(a).is_empty());
        // Parked copies go after thirty days, and not before; a folder not named for a time and
        // the folders around them stay until they are empty.
        let now = helm_core::now_ms();
        let odd = layers::knowledge_dir(b)
            .join(CONFLICTS)
            .join("everyone")
            .join("keep");
        fs::create_dir_all(&odd).unwrap();
        assert_eq!(prune_conflicts(b, now), 0);
        assert_eq!(parked(b), ["# Style\nBe long."]);
        assert_eq!(prune_conflicts(b, now + CONFLICTS_KEPT_MS + 60_000), 1);
        assert!(parked(b).is_empty() && odd.is_dir());
        fs::remove_dir(&odd).unwrap();
        prune_conflicts(b, now);
        assert!(!layers::knowledge_dir(b)
            .join(CONFLICTS)
            .join("everyone")
            .exists());
        assert_eq!(
            sync_once(b, &link_b, &*hub).await.unwrap(),
            Report::default()
        );

        // A deletion travels, and the emptied skill folder goes with it.
        fs::remove_dir_all(ev.root(a).join("skills/deploy")).unwrap();
        let r = sync_once(a, &link_a, &*hub).await.unwrap();
        assert_eq!(r.pushed, 2);
        let r = sync_once(b, &link_b, &*hub).await.unwrap();
        assert_eq!(r.removed, 2);
        assert!(!ev.root(b).join("skills/deploy").exists());
        assert!(ev.root(b).join("rules/style.md").exists());

        // An edit on a file someone deleted: the deleter's wins, the edit is parked.
        write(a, &ev, "rules/gone.md", "# Gone\none");
        sync_once(a, &link_a, &*hub).await.unwrap();
        sync_once(b, &link_b, &*hub).await.unwrap();
        fs::remove_file(ev.root(a).join("rules/gone.md")).unwrap();
        sync_once(a, &link_a, &*hub).await.unwrap();
        write(b, &ev, "rules/gone.md", "# Gone\nedited late");
        let r = sync_once(b, &link_b, &*hub).await.unwrap();
        assert_eq!(r.parked, ["everyone/rules/gone.md"]);
        assert!(read(b, &ev, "rules/gone.md").is_none());
        assert!(parked(b).contains(&"# Gone\nedited late".to_owned()));

        // Names a Helm refuses to write, however the hub came by them.
        for bad in [
            "rules/../escape.md",
            "rules/.hidden.md",
            "skills/lonely.md",
            "rules/CON.md",
            "other/x.md",
            "rules/a:b.md",
            "rules\\back.md",
        ] {
            hub.inject("everyone", bad, b"nope");
        }
        // Text hiding something with invisible or direction-changing characters.
        hub.inject(
            "everyone",
            "rules/sneaky.md",
            "# Sneaky\n\u{202E}evil".as_bytes(),
        );
        // A name differing from a kept file's only in letter case: one file on Windows.
        hub.inject("everyone", "rules/Style.md", b"# Style\nother");
        let twin = usize::from(cfg!(windows));
        let r = sync_once(b, &link_b, &*hub).await.unwrap();
        assert_eq!(
            (r.pulled, r.errors.len()),
            (1 - twin, 8 + twin),
            "{:?}",
            r.errors
        );
        assert!(!layers::knowledge_dir(b).join("escape.md").exists());
        assert!(read(b, &ev, "rules/sneaky.md").is_none());
        if cfg!(windows) {
            assert_eq!(
                read(b, &ev, "rules/style.md").unwrap(),
                "# Style\nBe brief, always."
            );
        }

        // A profile linked later gets the person's earlier files, which the cursor had passed.
        let link_b2 = links(&[
            ("pb".into(), Some("7".into())),
            ("pc".into(), Some("9".into())),
        ]);
        hub.inject("person:9", "rules/sam.md", b"# Sam\nhi");
        let r = sync_once(b, &link_b2, &*hub).await.unwrap();
        assert_eq!(
            read(b, &Layer::Profile("pc".into()), "rules/sam.md").unwrap(),
            "# Sam\nhi"
        );
        assert_eq!(r.changed, [Layer::Profile("pc".into())]);

        // A PC that joins later, with the examples freshly copied in, does not bring back one the
        // household removed, and sets nothing aside for it.
        let coder = crate::examples::AGENTS
            .iter()
            .find(|(n, _)| *n == "coder")
            .unwrap()
            .1;
        write(a, &ev, "agents/coder.md", coder);
        sync_once(a, &link_a, &*hub).await.unwrap();
        fs::remove_file(ev.root(a).join("agents/coder.md")).unwrap();
        sync_once(a, &link_a, &*hub).await.unwrap();
        let c = tempfile::tempdir().unwrap();
        let c = c.path();
        crate::examples::seed(c);
        assert!(read(c, &ev, "agents/coder.md").is_some());
        let r = sync_once(c, &links(&[]), &*hub).await.unwrap();
        assert!(read(c, &ev, "agents/coder.md").is_none(), "{r:?}");
        assert!(!hub.has("everyone", "agents/coder.md"));
        assert!(parked(c).is_empty());
        assert!(
            read(c, &ev, "agents/planner.md").is_some() && hub.has("everyone", "agents/planner.md")
        );
    }

    #[test]
    fn paths_follow_the_hubs_rule() {
        for (path, ok) in [
            ("rules/a.md", true),
            ("agents/helper.md", true),
            ("skills/x/SKILL.md", true),
            ("skills/x/deep/er/file.txt", true),
            ("rules", false),
            ("skills/x", false),
            ("skills/SKILL.md", false),
            ("notes/a.md", false),
            ("rules//a.md", false),
            ("rules/a.md/", false),
            ("rules/.a.md", false),
            ("rules/a.", false),
            ("rules/NUL.md", false),
            ("rules/com1", false),
            ("rules/a*b.md", false),
        ] {
            assert_eq!(valid_path(path), ok, "{path}");
        }
    }

    #[test]
    fn a_layer_change_event_wakes_the_hooks() {
        let hits = Arc::new(Mutex::new(0));
        let t = Trigger::default();
        let h = hits.clone();
        t.on_change(move || *h.lock().unwrap() += 1);
        t.observe(&AppEvent::new("knowledge-changed"));
        t.observe(&AppEvent::new("skills-changed"));
        assert_eq!(*hits.lock().unwrap(), 1);
    }
}
