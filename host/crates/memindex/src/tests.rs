//! The indexer against a stand-in hub (it keeps what it is sent, like the real one), a stand-in
//! embedder and temporary folders. Nothing here reaches a hub or a model.

use crate::engine::{EmbedErr, Embedder, Indexer, Pacing};
use crate::feeds::Env;
use crate::hub::{HubStatus, IndexHub, Responder};
use crate::model::{Kind, Scope, Settings, Source};
use crate::store;
use helm_core::db::{Database, MigrationRegistry};
use rusqlite::params;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashSet};
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// A hub that keeps docs by source and key, answers every frame the indexer sends, and refuses a
/// doc owned by person `999` the way the real one refuses a person it may not act for.
#[derive(Default)]
struct FakeHub {
    down: AtomicBool,
    epoch: AtomicU64,
    docs: Mutex<BTreeMap<(String, String), Value>>,
    vectors: Mutex<HashSet<(String, i64)>>,
    frames: Mutex<Vec<String>>,
}

impl FakeHub {
    fn count(&self, kind: &str) -> usize {
        self.frames
            .lock()
            .unwrap()
            .iter()
            .filter(|f| *f == kind)
            .count()
    }

    fn keys(&self, source_end: &str) -> Vec<String> {
        self.docs
            .lock()
            .unwrap()
            .keys()
            .filter(|(s, _)| s.ends_with(source_end))
            .map(|(_, k)| k.clone())
            .collect()
    }

    fn doc(&self, key: &str) -> Value {
        self.docs
            .lock()
            .unwrap()
            .iter()
            .find(|((_, k), _)| k == key)
            .map(|(_, d)| d.clone())
            .unwrap_or(Value::Null)
    }

    /// Chunk ids are `ord` plus a thousand per doc position: stable, and unique enough here.
    fn chunk_ids(&self) -> Vec<(String, i64)> {
        let docs = self.docs.lock().unwrap();
        docs.iter()
            .enumerate()
            .flat_map(|(i, ((_, key), d))| {
                (0..d["chunks"].as_array().map_or(0, Vec::len))
                    .map(move |n| (key.clone(), (i * 1000 + n) as i64))
            })
            .collect()
    }
}

impl IndexHub for FakeHub {
    fn ask(&self, frame: Value) -> Result<Value, String> {
        if self.down.load(Ordering::SeqCst) {
            return Err("The hub line dropped.".into());
        }
        let kind = frame["type"].as_str().unwrap_or("").to_owned();
        self.frames.lock().unwrap().push(kind.clone());
        let mut docs = self.docs.lock().unwrap();
        let source = frame["source"].as_str().unwrap_or("").to_owned();
        Ok(match kind.as_str() {
            "ix_hello" => {
                let out: Vec<Value> = frame["sources"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|s| {
                        let id = s["id"].as_str().unwrap();
                        let lines: Vec<String> = docs
                            .iter()
                            .filter(|((sid, _), _)| sid == id)
                            .map(|((_, k), d)| format!("{k}:{}", d["hash"].as_str().unwrap()))
                            .collect();
                        json!({"id": id, "docs": lines.len(),
                               "digest": format!("{:x}", Sha256::digest(lines.join("\n").as_bytes()))})
                    })
                    .collect();
                json!({"ok": true, "sources": out, "features": ["index"]})
            }
            "ix_manifest" => {
                let items: Vec<Value> = docs
                    .iter()
                    .filter(|((s, _), _)| *s == source)
                    .map(|((_, k), d)| json!({"key": k, "hash": d["hash"]}))
                    .collect();
                json!({"ok": true, "items": items, "more": false})
            }
            "ix_put" => {
                let results: Vec<Value> = frame["docs"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|d| {
                        let key = d["key"].as_str().unwrap().to_owned();
                        if d["owner"].as_str() == Some("999") {
                            return json!({"key": key, "status": "refused", "reason": "not this machine's person"});
                        }
                        let slot = (source.clone(), key.clone());
                        let status = match docs.get(&slot) {
                            Some(old) if old["hash"] == d["hash"] => "unchanged",
                            Some(_) => "replaced",
                            None => "inserted",
                        };
                        docs.insert(slot, d.clone());
                        json!({"key": key, "status": status})
                    })
                    .collect();
                json!({"ok": true, "results": results})
            }
            "ix_delete" => {
                let mut n = 0;
                for k in frame["keys"].as_array().unwrap() {
                    n += docs
                        .remove(&(source.clone(), k.as_str().unwrap().to_owned()))
                        .is_some() as i64;
                }
                json!({"ok": true, "deleted": n})
            }
            "ix_pending" => {
                drop(docs);
                let have = self.vectors.lock().unwrap().clone();
                let limit = frame["limit"].as_u64().unwrap_or(10) as usize;
                let chunks: Vec<Value> = self
                    .chunk_ids()
                    .into_iter()
                    .filter(|(k, id)| !have.contains(&(k.clone(), *id)))
                    .take(limit)
                    .map(|(k, id)| json!({"chunk": id, "text": format!("text of {k}")}))
                    .collect();
                json!({"ok": true, "chunks": chunks, "model": "nomic-embed-text"})
            }
            "ix_vectors" => {
                drop(docs);
                let by_id: BTreeMap<i64, String> = self
                    .chunk_ids()
                    .into_iter()
                    .map(|(k, id)| (id, k))
                    .collect();
                let mut stored = 0;
                for it in frame["items"].as_array().unwrap() {
                    let id = it["chunk"].as_i64().unwrap();
                    assert_eq!(it["model"], "nomic-embed-text", "the tag is dropped");
                    if let Some(k) = by_id.get(&id) {
                        self.vectors.lock().unwrap().insert((k.clone(), id));
                        stored += 1;
                    }
                }
                json!({"ok": true, "stored": stored})
            }
            _ => json!({"ok": false, "error": "unknown frame"}),
        })
    }

    fn status(&self) -> HubStatus {
        if self.down.load(Ordering::SeqCst) {
            HubStatus::Down
        } else {
            HubStatus::Ready
        }
    }

    fn epoch(&self) -> u64 {
        self.epoch.load(Ordering::SeqCst)
    }

    fn on_change(&self, _f: Arc<dyn Fn() + Send + Sync>) {}

    fn set_responder(&self, _r: Arc<dyn Responder>) {}
}

#[derive(Default)]
struct FakeEmbedder {
    locked: AtomicBool,
    calls: AtomicU64,
}

impl Embedder for FakeEmbedder {
    fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, EmbedErr> {
        if self.locked.load(Ordering::SeqCst) {
            return Err(EmbedErr::Locked);
        }
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(texts.iter().map(|_| vec![0.5, -1.0, 0.25]).collect())
    }

    fn busy(&self) -> bool {
        false
    }
}

struct Rig {
    _dir: tempfile::TempDir,
    root: std::path::PathBuf,
    db: Arc<Database>,
    hub: Arc<FakeHub>,
    emb: Arc<FakeEmbedder>,
    env: Arc<Env>,
    ix: Indexer,
}

impl Rig {
    fn new() -> Rig {
        let mut reg = MigrationRegistry::new();
        helm_chats::register_migrations(&mut reg);
        helm_profiles::register_migrations(&mut reg);
        crate::register_migrations(&mut reg);
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        let db = Arc::new(Database::open(&root.join("t.sqlite3"), &reg).unwrap());
        db.write(|tx| {
            store::save_settings(
                tx,
                &Settings {
                    enabled: true,
                    server_id: "ollama".into(),
                    ..Default::default()
                },
            )
        })
        .unwrap();
        let hub = Arc::new(FakeHub::default());
        let emb = Arc::new(FakeEmbedder::default());
        let env = Arc::new(Env::new(
            db.clone(),
            Some(root.join("claude")),
            root.join("sessions"),
        ));
        let ix = Self::indexer(&db, &hub, &env, &emb);
        Rig {
            _dir: dir,
            root,
            db,
            hub,
            emb,
            env,
            ix,
        }
    }

    fn indexer(
        db: &Arc<Database>,
        hub: &Arc<FakeHub>,
        env: &Arc<Env>,
        emb: &Arc<FakeEmbedder>,
    ) -> Indexer {
        Indexer::new(
            db.clone(),
            hub.clone(),
            env.clone(),
            emb.clone(),
            Pacing {
                between_frames: Duration::ZERO,
            },
            Arc::new(|| {}),
        )
    }

    fn add_source(&self, id: &str, kind: Kind, path: &Path, person: &str, scope: Scope) {
        self.db
            .write(|tx| {
                store::save_source(
                    tx,
                    &Source {
                        id: id.into(),
                        kind,
                        path: path.to_string_lossy().into_owned(),
                        person_id: person.into(),
                        scope,
                        include: vec![],
                        exclude: vec![],
                        enabled: true,
                        created_at: 1,
                    },
                )
            })
            .unwrap();
    }

    fn status(&self, id: &str) -> String {
        self.ix.source_state(id).status
    }
}

fn write(dir: &Path, name: &str, text: &str) {
    let p = dir.join(name);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, text).unwrap();
}

#[test]
fn a_folder_is_pushed_kept_in_step_held_back_from_a_mass_delete_and_resumed() {
    let r = Rig::new();
    let notes = r.root.join("notes");
    write(&notes, "a.md", "# A\n\nalpha words here\n");
    write(&notes, "b.md", "# B\n\nbeta words here\n");
    write(&notes, "secret/x.md", "# X\n\nexcluded\n");
    r.db.write(|tx| {
        store::save_source(
            tx,
            &Source {
                id: "notes".into(),
                kind: Kind::Folder,
                path: notes.to_string_lossy().into_owned(),
                person_id: String::new(),
                scope: Scope::Me,
                include: vec![],
                exclude: vec!["secret".into()],
                enabled: true,
                created_at: 1,
            },
        )
    })
    .unwrap();

    // First pass: both files go up as private docs of the line's own person, then get vectors.
    r.ix.pass(None, false);
    assert_eq!(r.hub.keys(":notes"), ["a.md", "b.md"]);
    let a = r.hub.doc("a.md");
    assert_eq!(
        (a["scope"].as_str(), a["owner"].is_null()),
        (Some("me"), true)
    );
    assert_eq!(a["tier"], "note");
    assert!(
        r.ix.source_state("notes")
            .status
            .starts_with("2 docs, 2 sent"),
        "{}",
        r.status("notes")
    );
    assert!(
        r.hub.vectors.lock().unwrap().len() >= 2,
        "pass two embedded what was pending"
    );
    assert_eq!(r.ix.pending(), Some(0));

    // Edit, add and delete: put replaces, puts a new one, deletes the gone one.
    write(&notes, "a.md", "# A\n\nalpha words changed\n");
    write(&notes, "c.md", "# C\n\ngamma\n");
    std::fs::remove_file(notes.join("b.md")).unwrap();
    r.ix.pass(None, false);
    assert_eq!(r.hub.keys(":notes"), ["a.md", "c.md"]);
    assert!(
        r.status("notes").contains("1 removed"),
        "{}",
        r.status("notes")
    );

    // A restart (new indexer, new line) resends nothing: the digests agree.
    let puts = r.hub.count("ix_put");
    r.hub.epoch.fetch_add(1, Ordering::SeqCst);
    let again = Rig::indexer(&r.db, &r.hub, &r.env, &r.emb);
    again.pass(None, false);
    assert_eq!(r.hub.count("ix_put"), puts);
    assert_eq!(
        r.hub.count("ix_manifest"),
        0,
        "equal digests need no manifest"
    );

    // A hub restored from an older copy is brought back by the manifest walk.
    r.hub.docs.lock().unwrap().retain(|(_, k), _| k != "c.md");
    r.hub.epoch.fetch_add(1, Ordering::SeqCst);
    Rig::indexer(&r.db, &r.hub, &r.env, &r.emb).pass(None, false);
    assert_eq!(r.hub.keys(":notes"), ["a.md", "c.md"]);

    // Ten more files, then eight vanish: the guard holds the deletes until Index now.
    for i in 0..10 {
        write(
            &notes,
            &format!("n{i}.md"),
            &format!("# N{i}\n\nnote {i}\n"),
        );
    }
    r.ix.pass(None, false);
    assert_eq!(r.hub.keys(":notes").len(), 12);
    for i in 0..8 {
        std::fs::remove_file(notes.join(format!("n{i}.md"))).unwrap();
    }
    r.ix.pass(None, false);
    assert_eq!(r.hub.keys(":notes").len(), 12, "held back");
    assert!(
        r.status("notes").contains("Held back 8"),
        "{}",
        r.status("notes")
    );
    r.ix.pass(None, true);
    assert_eq!(r.hub.keys(":notes").len(), 4);

    // A folder that has gone missing deletes nothing, forced or not.
    std::fs::remove_dir_all(&notes).unwrap();
    r.ix.pass(None, false);
    assert_eq!(r.hub.keys(":notes").len(), 4);

    // The card reserved: text still goes up, vectors wait; unreserved, they catch up.
    std::fs::create_dir_all(&notes).unwrap();
    write(&notes, "a.md", "# A\n\nalpha words changed\n");
    write(&notes, "c.md", "# C\n\ngamma\n");
    write(&notes, "d.md", "# D\n\ndelta\n");
    write(&notes, "n8.md", "# N8\n\nnote 8\n");
    write(&notes, "n9.md", "# N9\n\nnote 9\n");
    r.emb.locked.store(true, Ordering::SeqCst);
    let before = r.hub.vectors.lock().unwrap().len();
    r.ix.pass(None, false);
    assert!(r.hub.keys(":notes").contains(&"d.md".to_owned()));
    assert_eq!(
        r.hub.vectors.lock().unwrap().len(),
        before,
        "no vectors while locked"
    );
    assert!(r.ix.note().contains("graphics card"), "{}", r.ix.note());
    r.emb.locked.store(false, Ordering::SeqCst);
    r.ix.pass(None, false);
    assert_eq!(r.ix.pending(), Some(0));

    // A hub that drops mid-pass leaves rows to resume from.
    write(&notes, "e.md", "# E\n\nepsilon\n");
    r.hub.down.store(true, Ordering::SeqCst);
    r.ix.pass(None, false);
    assert!(r.ix.note().contains("Waiting for the hub"));
    r.hub.down.store(false, Ordering::SeqCst);
    r.ix.pass(None, false);
    assert!(r.hub.keys(":notes").contains(&"e.md".to_owned()));
}

#[test]
fn a_session_a_helm_chat_ran_is_left_to_the_chat_and_a_long_one_goes_in_parts() {
    let r = Rig::new();
    let projects = r.root.join("claude");
    let line = |text: &str, n: usize| {
        format!(
            "{}\n{}\n",
            json!({"type":"user","uuid":format!("u{n}"),"timestamp":"2026-11-01T10:00:00Z",
                   "message":{"role":"user","content":text}}),
            json!({"type":"assistant","uuid":format!("a{n}"),"timestamp":"2026-11-01T10:00:05Z",
                   "message":{"role":"assistant","content":[{"type":"text","text":format!("answer {n}")}]}}),
        )
    };
    write(
        &projects.join("proj"),
        "own-session.jsonl",
        &line("ask one", 1),
    );
    write(
        &projects.join("proj"),
        "other-session.jsonl",
        &line("ask two", 2),
    );
    let long: String = (0..230)
        .map(|n| line(&format!("question {n}"), n))
        .collect();
    write(&projects.join("proj"), "long-session.jsonl", &long);
    // A subagent file in a session folder is not a main session.
    write(
        &projects
            .join("proj")
            .join("other-session")
            .join("subagents"),
        "agent-1.jsonl",
        &line("deep", 9),
    );
    // Helm's binding for a chat names own-session.
    std::fs::create_dir_all(r.root.join("sessions")).unwrap();
    std::fs::write(
        r.root.join("sessions").join("chat1.claude.json"),
        json!({"nativeSessionId": "own-session", "cwd": "", "updatedAt": 1}).to_string(),
    )
    .unwrap();
    // And a usage line in another chat's log names a second one.
    std::fs::write(
        r.root.join("sessions").join("chat2.jsonl"),
        format!(
            "{}\n",
            json!({"type":"usage","id":"s:1","nativeSession":"other-session"})
        ),
    )
    .unwrap();
    r.add_source("code", Kind::ClaudeCode, Path::new(""), "", Scope::Me);
    r.ix.pass(None, false);
    let keys = r.hub.keys(":code");
    assert_eq!(
        keys,
        ["long-session", "long-session#2"],
        "{} / {}",
        r.status("code"),
        r.ix.note()
    );
    assert!(
        r.hub.doc("long-session")["chunks"]
            .as_array()
            .unwrap()
            .len()
            <= 200
    );
    assert_eq!(
        r.hub.doc("long-session")["locator"],
        "claude-code:long-session"
    );
}

#[test]
fn helm_chats_are_filed_under_their_profiles_person_and_shares_to_unknown_people_stay_private() {
    let r = Rig::new();
    let (pat, sam) =
        r.db.write(|tx| {
            let pat = helm_profiles::store::create(tx, "Pat", None, Some("7"), 5).unwrap();
            let sam = helm_profiles::store::create(tx, "Sam", None, None, 6).unwrap();
            tx.execute(
                "INSERT INTO projects(id, kind, name, created_at) VALUES ('p1','folder','Proj',1)",
                [],
            )?;
            Ok::<_, rusqlite::Error>((pat, sam))
        })
        .unwrap();
    let home = r.env.home().unwrap();
    let chat = |id: &str, profile: &str, shared: &[&str]| {
        r.db.write(|tx| {
            tx.execute(
                "INSERT INTO chats(id, project_id, title, profile_id, created_at, updated_at)
                 VALUES (?1, 'p1', ?2, ?3, 1, 1)",
                params![id, format!("chat {id}"), profile],
            )?;
            for (i, (role, text)) in [
                ("user", "what is the plan"),
                ("assistant", "the plan is this"),
            ]
            .iter()
            .enumerate()
            {
                tx.execute(
                    "INSERT INTO messages(chat_id, source_key, role, text, timestamp, sequence)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                    params![id, format!("k{i}"), role, text, 1_000 + i as i64, i as i64],
                )?;
            }
            for s in shared {
                tx.execute(
                    "INSERT INTO chat_shares(chat_id, profile_id) VALUES (?1, ?2)",
                    params![id, s],
                )?;
            }
            Ok::<_, rusqlite::Error>(())
        })
        .unwrap();
    };
    chat("own", &home.id, &[]);
    chat("toPat", &home.id, &[&pat.id]);
    chat("patToSam", &pat.id, &[&sam.id]);
    chat("samOwn", &sam.id, &[]);
    r.add_source("chats", Kind::HelmChats, Path::new(""), "", Scope::Me);
    r.ix.pass(None, false);
    assert_eq!(
        r.hub.keys(":chats"),
        ["own", "patToSam", "toPat"],
        "Sam has no hub person: nothing of theirs"
    );
    let field = |k: &str, f: &str| r.hub.doc(k)[f].clone();
    assert_eq!(
        (field("own", "scope"), field("own", "owner")),
        (json!("me"), Value::Null)
    );
    assert_eq!(
        (field("toPat", "scope"), field("toPat", "readers")),
        (json!("some"), json!(["7"]))
    );
    assert_eq!(field("toPat", "locator"), json!("helm://chats/toPat"));
    // Owned by Pat, shared only to a profile the hub does not know: private, owner 7.
    assert_eq!(
        (
            field("patToSam", "scope"),
            field("patToSam", "owner"),
            field("patToSam", "readers")
        ),
        (json!("me"), json!("7"), json!([]))
    );
    // Sharing more is a change the hub hears of; the words did not move.
    let before = r.hub.doc("own")["hash"].clone();
    r.db.write(|tx| {
        tx.execute(
            "INSERT INTO chat_shares(chat_id, profile_id) VALUES ('own', ?1)",
            [&pat.id],
        )
    })
    .unwrap();
    r.ix.pass(None, false);
    assert_ne!(r.hub.doc("own")["hash"], before);
    assert_eq!(field("own", "scope"), json!("some"));
}

#[test]
fn the_hubs_questions_are_answered_only_for_a_viewer_who_may_see_the_chat() {
    use crate::answer::Answerer;
    use crate::hub::{Embedded, Responder};
    let r = Rig::new();
    let (pat, sam, home) =
        r.db.write(|tx| {
            let pat = helm_profiles::store::create(tx, "Pat", None, Some("7"), 5).unwrap();
            let sam = helm_profiles::store::create(tx, "Sam", None, Some("8"), 6).unwrap();
            let home = helm_profiles::store::home(tx).unwrap().unwrap();
            helm_profiles::store::update(
                tx,
                &home.id,
                helm_profiles::store::Patch {
                    hub_person_id: Some(Some("1".into())),
                    ..Default::default()
                },
            )
            .unwrap();
            tx.execute(
                "INSERT INTO projects(id, kind, name, created_at) VALUES ('p1','folder','Proj',1)",
                [],
            )?;
            Ok::<_, rusqlite::Error>((pat, sam, home))
        })
        .unwrap();
    for (id, owner, shared) in [("mine", &pat.id, None), ("homes", &home.id, Some(&pat.id))] {
        r.db.write(|tx| {
            tx.execute(
                "INSERT INTO chats(id, project_id, title, profile_id, created_at, updated_at)
                 VALUES (?1, 'p1', 't', ?2, 1, 1)",
                params![id, owner],
            )?;
            tx.execute(
                "INSERT INTO messages(chat_id, source_key, role, text, thinking, timestamp, sequence)
                 VALUES (?1, 'k0', 'assistant', 'plain words', 'secretly the zebra plan', 50, 0)",
                [id],
            )?;
            if let Some(p) = shared {
                tx.execute(
                    "INSERT INTO chat_shares(chat_id, profile_id) VALUES (?1, ?2)",
                    params![id, p],
                )?;
            }
            Ok::<_, rusqlite::Error>(())
        })
        .unwrap();
    }
    let answerer = Answerer {
        env: r.env.clone(),
        embedder: r.emb.clone(),
    };
    let ask = |viewer: &str, kind: &str, chat: &str, query: &str| {
        answerer.trace(&json!({"viewer": viewer, "kind": kind, "chat": chat, "query": query}))
    };
    let rows = [
        ("7", "mine", "zebra", true, 1),
        ("8", "mine", "zebra", false, 0),
        ("7", "homes", "zebra", true, 1),
        ("8", "homes", "zebra", false, 0),
        ("1", "homes", "ZEBRA", true, 1),
        ("1", "mine", "zebra", false, 0),
        ("7", "mine", "nothing like it", true, 0),
        ("99", "mine", "zebra", false, 0),
    ];
    for (viewer, chat, query, ok, hits) in rows {
        let got = ask(viewer, "helm_chats", chat, query);
        assert_eq!(got["ok"], json!(ok), "{viewer} on {chat}: {got}");
        if ok {
            assert_eq!(got["results"].as_array().unwrap().len(), hits, "{got}");
        }
    }
    let hit = ask("7", "helm_chats", "mine", "zebra");
    assert!(hit["results"][0]["snippet"]
        .as_str()
        .unwrap()
        .contains("zebra"));
    assert_eq!(hit["results"][0]["link"], "helm://chats/mine/messages/1");
    let _ = sam;

    // A Claude Code session is its home person's alone, and its id cannot name a path.
    let projects = r.root.join("claude").join("proj");
    write(
        &projects,
        "sess1.jsonl",
        &format!(
            "{}\n{}\n",
            json!({"type":"user","uuid":"u1","timestamp":"2026-11-01T10:00:00Z","message":{"content":"hello"}}),
            json!({"type":"assistant","uuid":"a1","timestamp":"2026-11-01T10:00:05Z",
                   "message":{"content":[{"type":"thinking","thinking":"the zebra again"}]}}),
        ),
    );
    let got = ask("1", "claude_code", "sess1", "zebra");
    assert_eq!(got["results"].as_array().unwrap().len(), 1, "{got}");
    assert_eq!(got["results"][0]["link"], "claude-code:sess1@a1");
    assert_eq!(ask("7", "claude_code", "sess1", "zebra")["ok"], false);
    assert_eq!(ask("1", "claude_code", "../sess1", "zebra")["ok"], false);

    // The query embed: a vector with the model's name, or "locked" while the card is reserved.
    assert_eq!(
        answerer.embed("search_query: zebra"),
        Embedded::Vector(vec![0.5, -1.0, 0.25], "nomic-embed-text".into())
    );
    r.emb.locked.store(true, Ordering::SeqCst);
    assert_eq!(answerer.embed("search_query: zebra"), Embedded::Locked);
}
