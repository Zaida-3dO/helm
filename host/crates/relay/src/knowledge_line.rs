//! Knowledge sync over the hub line.
//!
//! [`KnowledgeLine`] is the production [`HubKnowledge`]: `kn_pull` and `kn_put` go up the line and
//! the matching `kn_files` and `kn_put_result` come back by request id. A request is never queued
//! for a later line: with the line down it fails at once, and the pass tries again on the next
//! line-up. `kn_changed` (some device changed a file) and a new line wake the sync loop, as does
//! any change to a knowledge layer on this PC ([`helm_skills::sync::Trigger`]).
//!
//! [`start`] runs that loop. A pass first lines up the hub's people with local profiles (a person
//! the hub knows becomes a profile here, linked by the hub's id), then runs
//! [`helm_skills::sync::sync_once`] over Everyone and every linked profile, and announces the
//! layers whose files changed here with `knowledge-changed`.

use crate::client::{Client, KnowledgeSink, LineHandle, LineState};
use base64::Engine as _;
use helm_core::db::Database;
use helm_core::profile::Profile;
use helm_ipc::{AppEvent, EventSink, OpCtx};
use helm_profiles::store::{self, Patch, ProfileError};
use helm_skills::sync::{self, BoxFut, HubFile, HubKnowledge, Pulled, PutReply, PutRequest};
use rusqlite::Transaction;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::{oneshot, Notify};

/// How long a request may wait for the hub's answer.
const TIMEOUT: Duration = Duration::from_secs(30);
/// Changes within this time make one pass.
const DEBOUNCE: Duration = Duration::from_millis(800);

pub struct KnowledgeLine {
    line: Arc<LineHandle>,
    pending: Mutex<HashMap<String, oneshot::Sender<Value>>>,
    wake: Notify,
    /// The hub's people have not been lined up with profiles since the line came up.
    people_due: AtomicBool,
}

impl KnowledgeLine {
    pub fn new(line: Arc<LineHandle>) -> Arc<Self> {
        Arc::new(KnowledgeLine {
            line,
            pending: Mutex::new(HashMap::new()),
            wake: Notify::new(),
            people_due: AtomicBool::new(true),
        })
    }

    /// Ask for a pass.
    pub fn poke(&self) {
        self.wake.notify_one();
    }

    fn pending(&self) -> std::sync::MutexGuard<'_, HashMap<String, oneshot::Sender<Value>>> {
        self.pending.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Send `frame` with a fresh id and wait for the frame the hub answers it with.
    async fn ask(&self, mut frame: Value) -> Result<Value, String> {
        let id = uuid::Uuid::new_v4().simple().to_string();
        frame["id"] = json!(id);
        let (tx, rx) = oneshot::channel();
        self.pending().insert(id.clone(), tx);
        if !self.line.device_frame(frame) {
            self.pending().remove(&id);
            return Err("Helm is not connected to the hub.".into());
        }
        match tokio::time::timeout(TIMEOUT, rx).await {
            Ok(Ok(answer)) => Ok(answer),
            Ok(Err(_)) => Err("The hub line dropped before the answer came.".into()),
            Err(_) => {
                self.pending().remove(&id);
                Err("The hub did not answer in time.".into())
            }
        }
    }
}

impl KnowledgeSink for KnowledgeLine {
    fn frame(&self, frame: Value) {
        match frame.get("type").and_then(Value::as_str) {
            Some("kn_changed") => self.poke(),
            Some("kn_files" | "kn_put_result") => {
                let id = frame.get("id").and_then(Value::as_str).unwrap_or("");
                if let Some(tx) = self.pending().remove(id) {
                    let _ = tx.send(frame);
                }
            }
            _ => {}
        }
    }

    fn line_up(&self) {
        self.people_due.store(true, Ordering::SeqCst);
        self.poke();
    }

    fn line_down(&self) {
        // Dropping the senders fails every request still waiting.
        self.pending().clear();
    }
}

/// The files of a `kn_files` answer, or the hub's sentence.
fn parse_pull(answer: &Value) -> Result<Pulled, String> {
    if let Some(why) = answer.get("error").and_then(Value::as_str) {
        return Err(why.to_owned());
    }
    let text = |f: &Value, k: &str| f.get(k).and_then(Value::as_str).unwrap_or("").to_owned();
    let files = answer
        .get("files")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .map(|f| HubFile {
            layer: text(f, "layer"),
            path: text(f, "path"),
            seq: f.get("seq").and_then(Value::as_i64).unwrap_or(0),
            sha256: text(f, "sha256"),
            deleted: f.get("deleted").and_then(Value::as_bool).unwrap_or(false),
            body: f
                .get("body")
                .and_then(Value::as_str)
                .and_then(|b| base64::engine::general_purpose::STANDARD.decode(b).ok()),
        })
        .collect();
    Ok(Pulled {
        files,
        cursor: answer.get("cursor").and_then(Value::as_i64).unwrap_or(0),
        more: answer.get("more").and_then(Value::as_bool).unwrap_or(false),
    })
}

fn put_frame(req: &PutRequest) -> Value {
    let mut frame = json!({
        "type": "kn_put",
        "layer": req.layer,
        "path": req.path,
        "baseSeq": req.base_seq,
        "sha256": req.sha256,
        "deleted": req.deleted,
    });
    if let Some(body) = &req.body {
        frame["body"] = json!(base64::engine::general_purpose::STANDARD.encode(body));
    }
    frame
}

fn parse_put(answer: &Value) -> PutReply {
    if answer.get("ok").and_then(Value::as_bool) == Some(true) {
        return PutReply::Saved {
            seq: answer.get("seq").and_then(Value::as_i64).unwrap_or(0),
        };
    }
    if let Some(c) = answer.get("conflict") {
        return PutReply::Conflict {
            seq: c.get("seq").and_then(Value::as_i64).unwrap_or(0),
            sha256: c
                .get("sha256")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned(),
            deleted: c.get("deleted").and_then(Value::as_bool).unwrap_or(false),
        };
    }
    PutReply::Refused(
        answer
            .get("error")
            .and_then(Value::as_str)
            .unwrap_or("The hub would not keep that file.")
            .to_owned(),
    )
}

impl HubKnowledge for KnowledgeLine {
    fn pull(&self, since: i64, limit: usize) -> BoxFut<'_, Result<Pulled, String>> {
        Box::pin(async move {
            let answer = self
                .ask(json!({ "type": "kn_pull", "since": since, "limit": limit }))
                .await?;
            parse_pull(&answer)
        })
    }

    fn put(&self, req: PutRequest) -> BoxFut<'_, Result<PutReply, String>> {
        Box::pin(async move { Ok(parse_put(&self.ask(put_frame(&req)).await?)) })
    }
}

// ---- the hub's people -------------------------------------------------------------------------

/// `(hub person id, name)` from the hub's `people` answer. The id is the hub's plain number, sent
/// as `id` (a number) or `personId` (text).
pub fn people_of(answer: &Value) -> Vec<(String, String)> {
    answer
        .get("people")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|p| {
            let id = match p.get("personId").or_else(|| p.get("id"))? {
                Value::Number(n) => n.to_string(),
                Value::String(s) => s.trim().to_owned(),
                _ => return None,
            };
            let name = p.get("name").and_then(Value::as_str)?.trim().to_owned();
            (!id.is_empty() && !name.is_empty()).then_some((id, name))
        })
        .collect()
}

/// Line up the hub's people with the local profiles: a person already linked is left alone, an
/// unlinked profile of the same name is linked to them, and anyone else gets a new profile. A
/// person whose name cannot be a profile here (taken by a linked profile, or the limit) is
/// skipped. Returns whether any profile changed.
pub fn link_people(
    tx: &Transaction<'_>,
    people: &[(String, String)],
    now: i64,
) -> Result<bool, ProfileError> {
    let mut changed = false;
    for (person, name) in people {
        let existing = store::list(tx)?;
        // Nothing is made before this install has its home profile.
        if existing.is_empty()
            || existing
                .iter()
                .any(|p| p.hub_person_id.as_deref() == Some(person.as_str()))
        {
            continue;
        }
        let same_name = existing
            .iter()
            .find(|p| p.hub_person_id.is_none() && p.name.to_lowercase() == name.to_lowercase());
        let done = match same_name {
            Some(p) => store::update(
                tx,
                &p.id,
                Patch {
                    hub_person_id: Some(Some(person.clone())),
                    ..Default::default()
                },
            ),
            None => store::create(tx, name, None, Some(person.as_str()), now),
        };
        match done {
            Ok(_) => changed = true,
            Err(ProfileError::Bad(why)) => {
                log::info!("knowledge sync: {name} was not added: {why}")
            }
            Err(e) => return Err(e),
        }
    }
    Ok(changed)
}

// ---- the loop ---------------------------------------------------------------------------------

/// Start syncing knowledge: attach the sink to the line and run the loop on `handle`.
pub fn start(handle: &tokio::runtime::Handle, ctx: &OpCtx, client: &Arc<Client>) {
    let (Some(profile), Some(db)) = (
        ctx.services.get::<Profile>(),
        ctx.services.get::<Database>(),
    ) else {
        return;
    };
    // Copies set aside by earlier passes are kept for a while, not for ever.
    let parked_in = profile.dir.clone();
    handle.spawn_blocking(move || {
        let removed = helm_skills::sync::prune_conflicts(&parked_in, helm_core::now_ms());
        if removed > 0 {
            log::info!("knowledge sync: removed {removed} old parked copies");
        }
    });
    let k = KnowledgeLine::new(client.handle());
    client.attach_knowledge(k.clone());
    if let Some(trigger) = ctx.services.get::<helm_skills::sync::Trigger>() {
        let k = k.clone();
        trigger.on_change(move || k.poke());
    }
    handle.spawn(run(
        k,
        client.handle(),
        db,
        profile.dir.clone(),
        ctx.events.clone(),
    ));
}

async fn run(
    k: Arc<KnowledgeLine>,
    line: Arc<LineHandle>,
    db: Arc<Database>,
    dir: PathBuf,
    events: Arc<dyn EventSink>,
) {
    loop {
        k.wake.notified().await;
        tokio::time::sleep(DEBOUNCE).await;
        if line.state() != LineState::Online {
            continue;
        }
        if k.people_due.swap(false, Ordering::SeqCst) {
            match line.peer("people", json!({})).await {
                Ok(answer) => {
                    let people = people_of(&answer);
                    let now = helm_core::now_ms();
                    match db.write(|tx| link_people(tx, &people, now)) {
                        Ok(true) => events.emit(AppEvent::new("profiles_changed")),
                        Ok(false) => {}
                        Err(e) => {
                            log::warn!("knowledge sync: the hub's people were not added: {e:?}")
                        }
                    }
                }
                // Without the household the profiles stay as they are; the files still sync.
                Err(e) => log::info!("knowledge sync: the hub's people were not read: {e}"),
            }
        }
        let profiles: Vec<(String, Option<String>)> = db
            .snapshot(|tx| {
                let mut st = tx.prepare("SELECT id, hub_person_id FROM profiles")?;
                let rows = st
                    .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                Ok::<_, rusqlite::Error>(rows)
            })
            .unwrap_or_default();
        let links = sync::links(&profiles);
        match sync::sync_once(&dir, &links, &*k).await {
            Ok(report) => {
                for e in &report.errors {
                    log::info!("knowledge sync: {e}");
                }
                if !report.parked.is_empty() {
                    log::info!(
                        "knowledge sync: {} changed file(s) were set aside in the conflicts folder",
                        report.parked.len()
                    );
                }
                for layer in &report.changed {
                    events.emit(AppEvent::new("knowledge-changed").with("layer", layer.key()));
                }
            }
            Err(e) => {
                log::info!("knowledge sync: {e}");
                // The pass did not finish: the hub's people are re-read with the next one.
                k.people_due.store(true, Ordering::SeqCst);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use helm_core::db::MigrationRegistry;

    #[test]
    fn frames_round_trip_and_the_hubs_people_become_profiles() {
        // A put carries its body as base64 and a deletion carries none.
        let put = put_frame(&PutRequest {
            layer: "person:7".into(),
            path: "rules/a.md".into(),
            base_seq: 3,
            sha256: "abc".into(),
            deleted: false,
            body: Some(b"hello".to_vec()),
        });
        assert_eq!(put["baseSeq"], 3);
        assert_eq!(put["body"], "aGVsbG8=");
        let gone = put_frame(&PutRequest {
            layer: "everyone".into(),
            path: "rules/a.md".into(),
            base_seq: 3,
            sha256: String::new(),
            deleted: true,
            body: None,
        });
        assert!(gone.get("body").is_none() && gone["deleted"] == true);

        // The three answers to a put, and the hub's own sentence for a refusal.
        for (answer, want) in [
            (json!({"ok": true, "seq": 9}), PutReply::Saved { seq: 9 }),
            (
                json!({"ok": false, "conflict": {"seq": 4, "sha256": "x", "deleted": true}}),
                PutReply::Conflict {
                    seq: 4,
                    sha256: "x".into(),
                    deleted: true,
                },
            ),
            (
                json!({"ok": false, "error": "This layer is full."}),
                PutReply::Refused("This layer is full.".into()),
            ),
        ] {
            assert_eq!(parse_put(&answer), want);
        }

        // A pull: files decoded, a deletion without a body, and an error as the hub's sentence.
        let pulled = parse_pull(&json!({
            "files": [
                {"layer": "everyone", "path": "rules/a.md", "seq": 5, "sha256": "s",
                 "deleted": false, "body": "aGVsbG8="},
                {"layer": "everyone", "path": "rules/b.md", "seq": 6, "deleted": true},
            ],
            "cursor": 6, "more": true,
        }))
        .unwrap();
        assert_eq!(
            (pulled.cursor, pulled.more, pulled.files.len()),
            (6, true, 2)
        );
        assert_eq!(pulled.files[0].body.as_deref(), Some(&b"hello"[..]));
        assert!(pulled.files[1].deleted && pulled.files[1].body.is_none());
        assert_eq!(parse_pull(&json!({"error": "Nope."})).unwrap_err(), "Nope.");

        // People come as a number or as text.
        let people = people_of(&json!({"people": [
            {"id": 7, "name": "Sam", "machines": []},
            {"personId": "9", "name": " Alex "},
            {"id": 11},
            {"id": 12, "name": ""},
        ]}));
        assert_eq!(
            people,
            [
                ("7".to_owned(), "Sam".to_owned()),
                ("9".to_owned(), "Alex".to_owned())
            ]
        );

        // Lined up with profiles: made when new, linked when an unlinked profile has the name,
        // and left alone once linked.
        let dir = tempfile::tempdir().unwrap();
        let mut reg = MigrationRegistry::new();
        helm_chats::register_migrations(&mut reg);
        helm_profiles::register_migrations(&mut reg);
        let db = Database::open(&dir.path().join("t.db"), &reg).unwrap();
        db.write(|tx| {
            let home = store::ensure_home(tx, 1)?;
            store::update(
                tx,
                &home.id,
                Patch {
                    name: Some("Me".into()),
                    ..Default::default()
                },
            )?;
            store::create(tx, "alex", None, None, 2)?;
            Ok::<_, ProfileError>(())
        })
        .unwrap();
        assert!(db.write(|tx| link_people(tx, &people, 3)).unwrap());
        assert!(!db.write(|tx| link_people(tx, &people, 4)).unwrap());
        let all = db.snapshot(|tx| store::list(tx)).unwrap();
        let person_of = |name: &str| {
            all.iter()
                .find(|p| p.name.to_lowercase() == name)
                .and_then(|p| p.hub_person_id.clone())
        };
        assert_eq!(all.len(), 3, "Alex linked, Sam made");
        assert_eq!(person_of("alex").as_deref(), Some("9"));
        assert_eq!(person_of("sam").as_deref(), Some("7"));
        assert_eq!(person_of("me"), None);
    }
}
