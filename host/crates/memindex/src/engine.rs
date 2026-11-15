//! One indexing pass: tell the hub what this PC has, bring it up to date, give new chunks their
//! vectors.
//!
//! A pass is a plain function over its inputs ([`Indexer::pass`]), so tests drive it directly and
//! the runner only decides when to call it. Nothing in it is kept only in memory that matters: a
//! doc is recorded in `memindex_docs` before it is sent, the hub's acknowledgement is recorded
//! after, and a restart carries on from the rows.
//!
//! **Text first, vectors after.** Docs go up without vectors, so everything can be found by its
//! words within minutes; the vector pass then embeds what the hub lists as pending, in batches,
//! and stops while the graphics card is reserved or a local chat is answering. Pushing text never
//! waits for either.
//!
//! **The prune guard.** A scan that finds the folder missing or unreadable, or empty when it had
//! docs, or that would delete more than 30% of at least ten docs, deletes nothing and says so.
//! Someone confirms with "Index now" (a forced pass). A half-synced folder can never empty the
//! hub's copy.

use crate::feeds::{feed_for, Built, Env, Listing, PutDoc};
use crate::hub::{HubStatus, IndexHub};
use crate::model::{normalise_model, Kind, Settings, Source};
use crate::store::{self, DocRow};
use base64::Engine as _;
use helm_core::db::Database;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// A put carries at most this many chunks and this many bytes of text.
const PUT_CHUNKS: usize = 200;
const PUT_BYTES: usize = 1_800_000;
/// Vectors are made this many chunks at a time.
pub const EMBED_BATCH: usize = 32;
const DELETE_BATCH: usize = 500;

/// Why embedding did not happen.
#[derive(Debug, PartialEq, Eq)]
pub enum EmbedErr {
    /// The graphics card is reserved.
    Locked,
    Failed(String),
}

/// Makes vectors on this PC.
pub trait Embedder: Send + Sync {
    fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, EmbedErr>;
    /// A chat on a local model is answering: background embedding waits.
    fn busy(&self) -> bool;
}

/// How fast a pass may go.
#[derive(Clone, Copy)]
pub struct Pacing {
    /// Between two frames during a backfill.
    pub between_frames: Duration,
}

impl Default for Pacing {
    fn default() -> Self {
        Pacing {
            between_frames: Duration::from_millis(250),
        }
    }
}

/// What the page is told about one source.
#[derive(Clone, Debug, Default)]
pub struct SourceState {
    pub status: String,
    pub last_scan: i64,
}

#[derive(Default)]
struct Mem {
    /// The line the hub was last greeted on.
    hello_epoch: Option<u64>,
    sources: HashMap<String, SourceState>,
    /// Keys the hub holds that this PC has no row for, by local source id (left by a reconcile).
    hub_only: HashMap<String, Vec<String>>,
    /// `(source, key)` to the stamp it was last skipped at.
    skipped: HashMap<(String, String), String>,
    /// `(source, key, hash)` the hub refused on this line.
    refused: HashSet<(String, String, String)>,
    pending: Option<u32>,
    note: String,
    /// The next whole pass looks at every doc again, whatever its fingerprint says.
    rescan: bool,
}

pub struct Indexer {
    pub(crate) db: Arc<Database>,
    hub: Arc<dyn IndexHub>,
    env: Arc<Env>,
    embedder: Arc<dyn Embedder>,
    pacing: Pacing,
    changed: Arc<dyn Fn() + Send + Sync>,
    mem: Mutex<Mem>,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

/// A frame the hub answered with `ok: false` is its sentence as an error.
fn accepted(answer: Value) -> Result<Value, String> {
    if answer.get("ok").and_then(Value::as_bool) == Some(false) {
        return Err(answer
            .get("error")
            .and_then(Value::as_str)
            .unwrap_or("The hub refused that.")
            .to_owned());
    }
    Ok(answer)
}

fn doc_json(d: &PutDoc) -> Value {
    let mut v = json!({
        "key": d.key,
        "hash": d.hash,
        "title": d.title,
        "locator": d.locator,
        "scope": d.scope,
        "readers": d.readers,
        "occurredAt": d.occurred_at,
        "tier": d.tier,
        "chunks": d.chunks.iter().map(|c| json!({
            "ord": c.ord, "heading": c.heading, "text": c.text, "at": c.at, "anchor": c.anchor,
        })).collect::<Vec<_>>(),
    });
    if let Some(o) = &d.owner {
        v["owner"] = json!(o);
    }
    v
}

/// The int8 form the hub stores: the vector scaled so its largest part is 127.
pub fn quantise(v: &[f32]) -> (Vec<u8>, f32) {
    let max = v.iter().fold(0f32, |m, x| m.max(x.abs()));
    let scale = if max > 0.0 { max / 127.0 } else { 1e-6 };
    let q = v
        .iter()
        .map(|x| ((x / scale).round().clamp(-127.0, 127.0) as i8) as u8)
        .collect();
    (q, scale)
}

impl Indexer {
    pub fn new(
        db: Arc<Database>,
        hub: Arc<dyn IndexHub>,
        env: Arc<Env>,
        embedder: Arc<dyn Embedder>,
        pacing: Pacing,
        changed: Arc<dyn Fn() + Send + Sync>,
    ) -> Self {
        Indexer {
            db,
            hub,
            env,
            embedder,
            pacing,
            changed,
            mem: Mutex::new(Mem::default()),
        }
    }

    pub fn settings(&self) -> Settings {
        self.db.snapshot(store::load_settings).unwrap_or_default()
    }

    pub fn source_state(&self, id: &str) -> SourceState {
        lock(&self.mem).sources.get(id).cloned().unwrap_or_default()
    }

    /// Make the next whole pass compare every doc with the hub's copy again (the nightly scan).
    pub fn full_rescan(&self) {
        let mut m = lock(&self.mem);
        m.rescan = true;
        m.hello_epoch = None;
    }

    pub fn note(&self) -> String {
        lock(&self.mem).note.clone()
    }

    pub fn pending(&self) -> Option<u32> {
        lock(&self.mem).pending
    }

    fn say(&self, source: &str, status: impl Into<String>) {
        let mut m = lock(&self.mem);
        let s = m.sources.entry(source.to_owned()).or_default();
        s.status = status.into();
        s.last_scan = helm_core::now_ms();
        drop(m);
        (self.changed)();
    }

    fn say_all(&self, note: impl Into<String>) {
        lock(&self.mem).note = note.into();
        (self.changed)();
    }

    /// The machine's name for its sources on the hub, made on first use.
    fn machine_id(&self) -> Result<String, String> {
        let mut s = self.settings();
        if s.machine_id.is_empty() {
            s.machine_id = uuid::Uuid::new_v4().simple().to_string()[..8].to_owned();
            self.db
                .write(|tx| store::save_settings(tx, &s))
                .map_err(|e| e.to_string())?;
        }
        Ok(s.machine_id)
    }

    fn hub_id(&self, machine: &str, src: &Source) -> String {
        format!("{machine}:{}", src.id)
    }

    /// One whole pass: sources in the order that makes things findable soonest (notes, logs,
    /// Helm chats, Claude Code), then the vectors. `only` limits it to some sources; `force`
    /// lets a held-back deletion through.
    pub fn pass(&self, only: Option<&HashSet<String>>, force: bool) {
        let settings = self.settings();
        if !settings.enabled {
            self.say_all("Memory indexing is off.");
            return;
        }
        match self.hub.status() {
            HubStatus::Down => {
                self.say_all("Waiting for the hub.");
                return;
            }
            HubStatus::TooOld => {
                self.say_all("The hub needs updating for search.");
                return;
            }
            HubStatus::Ready => {}
        }
        self.say_all("");
        let done = self.run(&settings, only, force);
        if only.is_none() && done.is_ok() {
            lock(&self.mem).rescan = false;
        }
        if let Err(why) = done {
            log::info!("memory indexing paused: {why}");
            self.say_all(format!("Paused: {why}"));
        }
    }

    fn run(
        &self,
        settings: &Settings,
        only: Option<&HashSet<String>>,
        force: bool,
    ) -> Result<(), String> {
        let machine = self.machine_id()?;
        let mut sources = self
            .db
            .snapshot(store::sources)
            .map_err(|e| e.to_string())?;
        sources.retain(|s| s.enabled);
        sources.sort_by_key(|s| {
            let rank = match s.kind {
                Kind::Folder => 0,
                Kind::ChatLogs => 1,
                Kind::HelmChats => 2,
                Kind::ClaudeCode => 3,
            };
            (rank, s.created_at)
        });
        self.hello(&machine, &sources)?;
        for src in &sources {
            if only.is_some_and(|o| !o.contains(&src.id)) {
                continue;
            }
            let res = self.sync_source(&machine, src, force);
            if let Err(why) = res {
                self.say(&src.id, format!("Stopped: {why}"));
                return Err(why);
            }
        }
        if self.embedder_wanted(settings) {
            self.embed_pass(settings)?;
        }
        Ok(())
    }

    // ---- greeting the hub --------------------------------------------------------------------

    /// On a new line: say what this PC holds of each source, and where the hub's copy differs,
    /// line up the rows so the differences are sent or removed.
    fn hello(&self, machine: &str, sources: &[Source]) -> Result<(), String> {
        let epoch = self.hub.epoch();
        if lock(&self.mem).hello_epoch == Some(epoch) {
            return Ok(());
        }
        let mut list = Vec::new();
        let mut local = HashMap::new();
        for s in sources {
            let docs = self
                .db
                .snapshot(|tx| store::docs(tx, &s.id))
                .map_err(|e| e.to_string())?;
            let (n, digest) = store::digest(&docs);
            list.push(json!({
                "id": self.hub_id(machine, s), "kind": s.kind.as_str(), "name": display_name(s),
                "docs": n, "digest": digest,
            }));
            local.insert(self.hub_id(machine, s), (s, docs, digest));
        }
        let state = accepted(self.hub.ask(json!({"type": "ix_hello", "sources": list}))?)?;
        {
            let mut m = lock(&self.mem);
            m.refused.clear();
            m.hub_only.clear();
        }
        let answers = state
            .get("sources")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        for a in answers {
            let Some(id) = a.get("id").and_then(Value::as_str) else {
                continue;
            };
            let Some((src, docs, digest)) = local.get(id) else {
                continue;
            };
            if let Some(why) = a.get("error").and_then(Value::as_str) {
                self.say(
                    &src.id,
                    format!("The hub would not take this source: {why}"),
                );
                continue;
            }
            if a.get("digest").and_then(Value::as_str) != Some(digest.as_str()) {
                self.reconcile(id, src, docs)?;
            }
        }
        lock(&self.mem).hello_epoch = Some(epoch);
        Ok(())
    }

    /// Ask the hub for its list of one source and mark every doc it does not hold exactly as we
    /// have it to be sent again. What it holds that we have no row for is remembered, for the
    /// next scan to delete when the thing is really gone.
    fn reconcile(
        &self,
        hub_id: &str,
        src: &Source,
        docs: &HashMap<String, DocRow>,
    ) -> Result<(), String> {
        let mut theirs: HashMap<String, String> = HashMap::new();
        let mut after = String::new();
        loop {
            let page = accepted(self.hub.ask(json!({
                "type": "ix_manifest", "source": hub_id, "afterKey": after, "limit": 1000,
            }))?)?;
            let items = page
                .get("items")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            for it in &items {
                if let (Some(k), Some(h)) = (
                    it.get("key").and_then(Value::as_str),
                    it.get("hash").and_then(Value::as_str),
                ) {
                    theirs.insert(k.to_owned(), h.to_owned());
                    after = k.to_owned();
                }
            }
            if page.get("more").and_then(Value::as_bool) != Some(true) || items.is_empty() {
                break;
            }
        }
        let stale: Vec<&String> = docs
            .iter()
            .filter(|(k, d)| d.is_pushed() && theirs.get(*k) != Some(&d.hash))
            .map(|(k, _)| k)
            .collect();
        self.db
            .write(|tx| {
                for k in stale {
                    store::forget_pushed(tx, &src.id, k)?;
                }
                Ok::<_, rusqlite::Error>(())
            })
            .map_err(|e| e.to_string())?;
        let only_theirs: Vec<String> = theirs
            .into_keys()
            .filter(|k| !docs.contains_key(k))
            .collect();
        lock(&self.mem).hub_only.insert(src.id.clone(), only_theirs);
        Ok(())
    }

    // ---- one source --------------------------------------------------------------------------

    fn sync_source(&self, machine: &str, src: &Source, force: bool) -> Result<(), String> {
        let hub_id = self.hub_id(machine, src);
        self.say(&src.id, "Scanning…");
        let feed = feed_for(src, &self.env);
        let listing = match feed.list() {
            Ok(l) => l,
            Err(e) => {
                self.say(&src.id, format!("Could not read: {e}"));
                return Ok(());
            }
        };
        let known = self
            .db
            .snapshot(|tx| store::docs(tx, &src.id))
            .map_err(|e| e.to_string())?;
        let mut by_parent: HashMap<&str, Vec<&String>> = HashMap::new();
        for (k, d) in &known {
            by_parent.entry(d.parent.as_str()).or_default().push(k);
        }
        let rescan = lock(&self.mem).rescan;
        let live: HashSet<&str> = listing.entries.iter().map(|e| e.key.as_str()).collect();

        let mut batch: Vec<PutDoc> = Vec::new();
        let (mut sent, mut skipped, mut refused) = (0usize, 0usize, 0usize);
        let mut first_refusal: Option<String> = None;
        let mut stale_keys: Vec<String> = Vec::new();

        for e in &listing.entries {
            let rows = by_parent.get(e.key.as_str());
            let all_pushed = rows.is_some_and(|r| r.iter().all(|k| known[*k].is_pushed()));
            let same_stamp = rows.is_some_and(|r| r.iter().all(|k| known[*k].stamp == e.stamp));
            if all_pushed && same_stamp && !rescan {
                continue;
            }
            let skip_id = (src.id.clone(), e.key.clone());
            if lock(&self.mem).skipped.get(&skip_id) == Some(&e.stamp) {
                skipped += 1;
                continue;
            }
            match feed.build(&e.key) {
                Err(why) => {
                    log::info!("memory indexing: {} was not read: {why}", e.key);
                    skipped += 1;
                }
                Ok(Built::Skip(_)) => {
                    skipped += 1;
                    lock(&self.mem).skipped.insert(skip_id, e.stamp.clone());
                    if let Some(r) = rows {
                        stale_keys.extend(r.iter().map(|k| (*k).clone()));
                    }
                }
                Ok(Built::Docs(docs)) => {
                    lock(&self.mem).skipped.remove(&skip_id);
                    let unchanged = rows.is_some_and(|r| {
                        r.len() == docs.len()
                            && docs
                                .iter()
                                .all(|d| known.get(&d.key).is_some_and(|k| k.hash == d.hash))
                    });
                    if unchanged && all_pushed {
                        self.db
                            .write(|tx| store::touch_stamp(tx, &src.id, &e.key, &e.stamp))
                            .map_err(|e| e.to_string())?;
                        continue;
                    }
                    let keys: HashSet<&str> = docs.iter().map(|d| d.key.as_str()).collect();
                    if let Some(r) = rows {
                        stale_keys.extend(
                            r.iter()
                                .filter(|k| !keys.contains(k.as_str()))
                                .map(|k| (*k).clone()),
                        );
                    }
                    let now = helm_core::now_ms();
                    self.db
                        .write(|tx| {
                            for d in &docs {
                                store::put_doc(
                                    tx, &src.id, &d.key, &d.parent, &e.stamp, &d.hash, now,
                                )?;
                            }
                            Ok::<_, rusqlite::Error>(())
                        })
                        .map_err(|e| e.to_string())?;
                    for d in docs {
                        let again = known
                            .get(&d.key)
                            .is_some_and(|k| k.is_pushed() && k.hash == d.hash);
                        let held = lock(&self.mem).refused.contains(&(
                            src.id.clone(),
                            d.key.clone(),
                            d.hash.clone(),
                        ));
                        if again || held {
                            continue;
                        }
                        let (chunks, bytes) = batch.iter().fold((0, 0), |(c, b), x| {
                            (
                                c + x.chunks.len(),
                                b + x.chunks.iter().map(|k| k.text.len()).sum::<usize>(),
                            )
                        });
                        let add_chunks = d.chunks.len();
                        let add_bytes: usize = d.chunks.iter().map(|k| k.text.len()).sum();
                        if !batch.is_empty()
                            && (chunks + add_chunks > PUT_CHUNKS || bytes + add_bytes > PUT_BYTES)
                        {
                            let (s, r, why) = self.flush(&hub_id, src, &mut batch)?;
                            sent += s;
                            refused += r;
                            first_refusal = first_refusal.or(why);
                        }
                        batch.push(d);
                    }
                }
            }
        }
        if !batch.is_empty() {
            let (s, r, why) = self.flush(&hub_id, src, &mut batch)?;
            sent += s;
            refused += r;
            first_refusal = first_refusal.or(why);
        }

        // What is gone: rows whose thing is no longer listed, and what the hub holds that we
        // never had a row for.
        let part_of = |k: &str| -> String {
            match k.rsplit_once('#') {
                Some((base, n)) if n.chars().all(|c| c.is_ascii_digit()) && !n.is_empty() => {
                    base.to_owned()
                }
                _ => k.to_owned(),
            }
        };
        let hub_only = lock(&self.mem).hub_only.remove(&src.id).unwrap_or_default();
        let mut gone: Vec<String> = known
            .iter()
            .filter(|(_, d)| !live.contains(d.parent.as_str()))
            .map(|(k, _)| k.clone())
            .collect();
        gone.extend(
            hub_only
                .into_iter()
                .filter(|k| !live.contains(k.as_str()) && !live.contains(part_of(k).as_str())),
        );
        gone.extend(stale_keys);
        gone.sort();
        gone.dedup();
        let parents: HashSet<&str> = known.values().map(|d| d.parent.as_str()).collect();
        let gone_parents: HashSet<&str> = known
            .iter()
            .filter(|(_, d)| !live.contains(d.parent.as_str()))
            .map(|(_, d)| d.parent.as_str())
            .collect();
        let held_back = !gone.is_empty()
            && !force
            && (!listing.complete
                || (listing.entries.is_empty() && !parents.is_empty())
                || (parents.len() >= 10 && gone_parents.len() * 10 > parents.len() * 3));
        let mut deleted = 0;
        if !gone.is_empty() && !held_back {
            for chunk in gone.chunks(DELETE_BATCH) {
                accepted(self.hub.ask(json!({
                    "type": "ix_delete", "source": hub_id, "keys": chunk,
                }))?)?;
                self.db
                    .write(|tx| store::delete_docs(tx, &src.id, chunk))
                    .map_err(|e| e.to_string())?;
                deleted += chunk.len();
            }
        }

        let total = self
            .db
            .snapshot(|tx| store::docs(tx, &src.id))
            .map_err(|e| e.to_string())?;
        let mut line = format!("{} docs", total.len());
        if sent > 0 {
            line.push_str(&format!(", {sent} sent"));
        }
        if deleted > 0 {
            line.push_str(&format!(", {deleted} removed"));
        }
        if skipped > 0 {
            line.push_str(&format!(", {skipped} left out"));
        }
        if refused > 0 {
            line.push_str(&format!(
                ", {refused} refused by the hub ({})",
                first_refusal.unwrap_or_default()
            ));
        }
        if held_back {
            line.push_str(&format!(
                ". Held back {} removals because the source looks incomplete; Index now confirms them",
                gone.len()
            ));
        } else if let Some(n) = listing_note(&listing) {
            line.push_str(&format!(". {n}"));
        }
        self.say(&src.id, line);
        Ok(())
    }

    /// Send the batch as one put and record what the hub took. Returns how many docs were sent
    /// and refused, and the first refusal's reason.
    fn flush(
        &self,
        hub_id: &str,
        src: &Source,
        batch: &mut Vec<PutDoc>,
    ) -> Result<(usize, usize, Option<String>), String> {
        let docs: Vec<PutDoc> = std::mem::take(batch);
        let frame = json!({
            "type": "ix_put", "source": hub_id, "kind": src.kind.as_str(),
            "docs": docs.iter().map(doc_json).collect::<Vec<_>>(),
        });
        let answer = accepted(self.hub.ask(frame)?)?;
        let results = answer
            .get("results")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let hashes: HashMap<&str, &str> = docs
            .iter()
            .map(|d| (d.key.as_str(), d.hash.as_str()))
            .collect();
        let (mut ok, mut refused, mut why) = (0, 0, None);
        let mut taken = Vec::new();
        for r in &results {
            let key = r.get("key").and_then(Value::as_str).unwrap_or("");
            match (r.get("status").and_then(Value::as_str), hashes.get(key)) {
                (Some("inserted" | "replaced" | "unchanged"), Some(h)) => {
                    taken.push((key.to_owned(), (*h).to_owned()));
                    ok += 1;
                }
                (Some("refused"), Some(h)) => {
                    refused += 1;
                    why =
                        why.or_else(|| r.get("reason").and_then(Value::as_str).map(str::to_owned));
                    lock(&self.mem).refused.insert((
                        src.id.clone(),
                        key.to_owned(),
                        (*h).to_owned(),
                    ));
                }
                _ => {}
            }
        }
        self.db
            .write(|tx| {
                for (k, h) in &taken {
                    store::mark_pushed(tx, &src.id, k, h)?;
                }
                Ok::<_, rusqlite::Error>(())
            })
            .map_err(|e| e.to_string())?;
        std::thread::sleep(self.pacing.between_frames);
        Ok((ok, refused, why))
    }

    // ---- vectors -----------------------------------------------------------------------------

    fn embedder_wanted(&self, s: &Settings) -> bool {
        !s.server_id.is_empty()
    }

    fn embed_pass(&self, settings: &Settings) -> Result<(), String> {
        let ours = normalise_model(&settings.model);
        loop {
            if self.embedder.busy() {
                self.say_all("Vectors wait while a local chat is working.");
                return Ok(());
            }
            let list = accepted(
                self.hub
                    .ask(json!({"type": "ix_pending", "limit": EMBED_BATCH}))?,
            )?;
            let chunks = list
                .get("chunks")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            if chunks.is_empty() {
                lock(&self.mem).pending = Some(0);
                return Ok(());
            }
            if let Some(theirs) = list.get("model").and_then(Value::as_str) {
                if normalise_model(theirs) != ours {
                    self.say_all(format!(
                        "The hub searches with {theirs}, but this PC is set to {ours}."
                    ));
                    return Ok(());
                }
            }
            lock(&self.mem).pending = None;
            let ids: Vec<i64> = chunks
                .iter()
                .filter_map(|c| c.get("chunk").and_then(Value::as_i64))
                .collect();
            let texts: Vec<String> = chunks
                .iter()
                .map(|c| {
                    c.get("text")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_owned()
                })
                .collect();
            if ids.len() != texts.len() {
                return Ok(());
            }
            let vectors = match self.embedder.embed(&texts) {
                Ok(v) => v,
                Err(EmbedErr::Locked) => {
                    self.say_all("Vectors wait while the graphics card is reserved.");
                    return Ok(());
                }
                Err(EmbedErr::Failed(why)) => {
                    self.say_all(format!("Vectors paused: {why}"));
                    return Ok(());
                }
            };
            let items: Vec<Value> = ids
                .iter()
                .zip(&vectors)
                .map(|(id, v)| {
                    let (q, scale) = quantise(v);
                    json!({
                        "chunk": id,
                        "vec": base64::engine::general_purpose::STANDARD.encode(q),
                        "scale": scale,
                        "model": ours,
                    })
                })
                .collect();
            let done = accepted(
                self.hub
                    .ask(json!({"type": "ix_vectors", "items": items}))?,
            )?;
            if done.get("stored").and_then(Value::as_i64).unwrap_or(0) == 0 {
                self.say_all("The hub did not take the vectors.");
                return Ok(());
            }
            std::thread::sleep(self.pacing.between_frames);
        }
    }

    /// The hub's source list now, for the sources this PC registered: how many docs it holds of
    /// each, so Settings can compare. A failure is an empty answer.
    pub fn local_counts(&self, id: &str) -> (u32, u32) {
        let docs = self
            .db
            .snapshot(|tx| store::docs(tx, id))
            .unwrap_or_default();
        (
            docs.len() as u32,
            docs.values().filter(|d| d.is_pushed()).count() as u32,
        )
    }

    /// Delete a source's docs from the hub, then forget the source. A hub that cannot be reached
    /// leaves everything as it was.
    pub fn remove_source(&self, id: &str) -> Result<(), String> {
        let docs = self
            .db
            .snapshot(|tx| store::docs(tx, id))
            .map_err(|e| e.to_string())?;
        let pushed: Vec<String> = docs
            .iter()
            .filter(|(_, d)| d.pushed_hash.is_some())
            .map(|(k, _)| k.clone())
            .collect();
        if !pushed.is_empty() {
            if self.hub.status() != HubStatus::Ready {
                return Err("The hub is not reachable, so its copy cannot be removed yet.".into());
            }
            let machine = self.machine_id()?;
            for chunk in pushed.chunks(DELETE_BATCH) {
                accepted(self.hub.ask(json!({
                    "type": "ix_delete", "source": format!("{machine}:{id}"), "keys": chunk,
                }))?)?;
            }
        }
        self.db
            .write(|tx| store::remove_source(tx, id))
            .map_err(|e| e.to_string())?;
        let mut m = lock(&self.mem);
        m.sources.remove(id);
        m.hub_only.remove(id);
        m.skipped.retain(|(s, _), _| s != id);
        Ok(())
    }
}

fn listing_note(l: &Listing) -> Option<String> {
    l.note.clone()
}

/// What the hub calls a source: the folder's name, or what the source is.
fn display_name(s: &Source) -> String {
    std::path::Path::new(&s.path)
        .file_name()
        .and_then(|n| n.to_str())
        .map_or_else(|| s.kind.as_str().to_owned(), str::to_owned)
}
