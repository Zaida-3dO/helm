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
    /// Runs after each put is stored.
    on_put: Mutex<Option<Box<dyn Fn() + Send>>>,
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
                if let Some(f) = self.on_put.lock().unwrap().as_ref() {
                    f();
                }
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
        helm_consent::register_migrations(&mut reg);
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
    r.add_source("chats", Kind::HelmChats, Path::new(""), "", Scope::Me);
    r.add_source("cc", Kind::ClaudeCode, Path::new(""), "", Scope::Me);
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

    // The hub's date window arrives as ISO strings (or milliseconds) and is honoured.
    let window = |after: Value, before: Value| {
        answerer.trace(
            &json!({"viewer": "7", "kind": "helm_chats", "chat": "mine", "query": "zebra",
                               "after": after, "before": before}),
        )["results"]
            .as_array()
            .unwrap()
            .len()
    };
    assert_eq!(
        window(json!("1970-01-01T00:00:00Z"), json!("1970-01-01T00:00:01Z")),
        1
    );
    assert_eq!(window(json!("2026-01-01T00:00:00Z"), json!(null)), 0);
    assert_eq!(window(json!(0), json!(100)), 1);
    assert_eq!(window(json!(100), json!(200)), 0);

    // With the home profile not linked to a person, the hub names the session's owner.
    r.db.write(|tx| {
        helm_profiles::store::update(
            tx,
            &home.id,
            helm_profiles::store::Patch {
                hub_person_id: Some(None),
                ..Default::default()
            },
        )
    })
    .unwrap();
    let owned = |viewer: &str, owner: Value| {
        answerer.trace(
            &json!({"viewer": viewer, "owner": owner, "kind": "claude_code",
                               "chat": "sess1", "query": "zebra"}),
        )["ok"]
            .clone()
    };
    assert_eq!(owned("1", json!(1)), true);
    assert_eq!(owned("7", json!(1)), false);
    assert_eq!(owned("1", json!(null)), false);

    // Nothing is read for a search once indexing, or that kind of source, is switched off.
    r.db.write(|tx| {
        let mut s = store::load_settings(tx)?;
        s.enabled = false;
        store::save_settings(tx, &s)
    })
    .unwrap();
    assert_eq!(ask("7", "helm_chats", "mine", "zebra")["ok"], false);
    r.db.write(|tx| {
        let mut s = store::load_settings(tx)?;
        s.enabled = true;
        store::save_settings(tx, &s)?;
        tx.execute(
            "UPDATE memindex_sources SET enabled = 0 WHERE kind = 'helm_chats'",
            [],
        )
    })
    .unwrap();
    assert_eq!(ask("7", "helm_chats", "mine", "zebra")["ok"], false);

    // The query embed: a vector with the model's name, or "locked" while the card is reserved.
    assert_eq!(
        answerer.embed("search_query: zebra"),
        Embedded::Vector(vec![0.5, -1.0, 0.25], "nomic-embed-text".into())
    );
    r.emb.locked.store(true, Ordering::SeqCst);
    assert_eq!(answerer.embed("search_query: zebra"), Embedded::Locked);
}

#[test]
fn credential_looking_passages_and_hidden_files_stay_home_and_switching_off_stops_the_next_frame() {
    let r = Rig::new();
    let notes = r.root.join("notes");
    // Built here so no real-looking secret sits in the source.
    let token = format!("ghp_{}", "a1B2".repeat(6));
    write(
        &notes,
        "a.md",
        &format!(
            "# Plain\n\nsome plain words {}\n\n# Keys\n\ntoken: {token}\n",
            "filler ".repeat(270)
        ),
    );
    write(&notes, "only.md", &format!("# Keys\n\n{token}\n"));
    write(&notes, ".quiet.md", "# Quiet\n\nhidden words\n");
    r.add_source("notes", Kind::Folder, &notes, "", Scope::Me);
    r.ix.pass(None, false);
    assert_eq!(r.hub.keys(":notes"), ["a.md"], "{}", r.status("notes"));
    let sent = r.hub.doc("a.md").to_string();
    assert!(sent.contains("some plain words"), "{sent}");
    assert!(!sent.contains(&token), "the credential was sent: {sent}");
    let status = r.status("notes");
    assert!(
        status.contains("1 passages held back") && status.contains("1 left out"),
        "{status}"
    );

    // Indexing switched off while a pass is sending: the next frame is not sent.
    let many = r.root.join("many");
    for i in 0..250 {
        write(
            &many,
            &format!("n{i}.md"),
            &format!("# N{i}\n\nnote number {i}\n"),
        );
    }
    r.add_source("many", Kind::Folder, &many, "", Scope::Me);
    let db = r.db.clone();
    *r.hub.on_put.lock().unwrap() = Some(Box::new(move || {
        db.write(|tx| {
            let mut s = store::load_settings(tx)?;
            s.enabled = false;
            store::save_settings(tx, &s)
        })
        .unwrap();
    }));
    let puts = r.hub.count("ix_put");
    r.ix.pass(Some(&HashSet::from(["many".to_owned()])), false);
    assert_eq!(r.hub.count("ix_put") - puts, 1, "{}", r.status("many"));
    assert_eq!(r.ix.note(), "Memory indexing is off.");
}

#[test]
fn an_agent_may_only_narrow_memory_indexing_and_files_sources_under_its_own_person() {
    use helm_consent::kinds::AllowKinds;
    use helm_hosttools::{CallOrigin, HostToolRegistry, ToolCtx, ToolHandler};
    let r = Rig::new();
    let (pat, home) =
        r.db.write(|tx| {
            let pat = helm_profiles::store::create(tx, "Pat", None, Some("7"), 5).unwrap();
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
            for (chat, owner) in [("mine", &home.id), ("pats", &pat.id)] {
                tx.execute(
                    "INSERT INTO chats(id, project_id, title, profile_id, created_at, updated_at)
                     VALUES (?1, 'p1', 't', ?2, 1, 1)",
                    params![chat, owner],
                )?;
            }
            // Indexing starts off.
            let mut s = store::load_settings(tx)?;
            s.enabled = false;
            store::save_settings(tx, &s)?;
            Ok::<_, rusqlite::Error>((pat, home))
        })
        .unwrap();
    let mut kinds = AllowKinds::new();
    crate::tool::register_allow_kinds(&mut kinds);
    let kinds = Arc::new(kinds);
    let mut services = helm_ipc::Services::new();
    services.insert(r.db.clone());
    services.insert(kinds.clone());
    services.insert(Arc::new(helm_core::profile::Profile {
        dir: r.root.clone(),
        hash: String::new(),
    }));
    let sink: Arc<dyn helm_ipc::EventSink> = Arc::new(helm_ipc::RecordingSink::default());
    crate::service::register_services(&mut services, &sink, r.hub.clone());
    let services = Arc::new(services);
    let svc = services.get::<crate::service::Service>().unwrap();
    let mut reg = HostToolRegistry::new();
    crate::tool::register_host_tools(&mut reg);
    let ToolHandler::Blocking(tool) = reg.get("app_memory_index").unwrap().handler.clone() else {
        panic!("blocking tool")
    };
    let call = |chat: &str, args: Value| {
        tool(
            &helm_hosttools::ToolCtx {
                caller_chat_id: chat.into(),
                is_child: false,
                tool_call_id: None,
                origin: CallOrigin::Agent,
                services: services.clone(),
                events: sink.clone(),
                ops: None,
            },
            args,
        )
        .map_err(|e| e.public)
    };
    let _: Option<ToolCtx> = None;

    // What widens what leaves the PC needs the user's card; nothing changes meanwhile.
    for args in [
        json!({"action": "switch", "enabled": true}),
        json!({"action": "switch", "enabled": true, "serverId": "other"}),
    ] {
        let why = call("mine", args).unwrap_err();
        assert!(why.contains("permission"), "{why}");
        assert!(!svc.view().enabled);
    }
    // A household member's chat changes none of the switches, even off.
    let why = call("pats", json!({"action": "switch", "enabled": false})).unwrap_err();
    assert!(why.contains("own person"), "{why}");
    // Another model on the same server needs no card.
    call(
        "mine",
        json!({"action": "switch", "model": "nomic-embed-text"}),
    )
    .unwrap();
    // Moving the embedding to another server needs one; naming the same one, and turning off,
    // do not.
    r.db.write(|tx| {
        let mut s = store::load_settings(tx)?;
        s.enabled = true;
        store::save_settings(tx, &s)
    })
    .unwrap();
    let why = call("mine", json!({"action": "switch", "serverId": "other"})).unwrap_err();
    assert!(why.contains("permission"), "{why}");
    assert_eq!(svc.view().server_id, "ollama");
    call(
        "mine",
        json!({"action": "switch", "serverId": "ollama", "enabled": true}),
    )
    .unwrap();
    call("mine", json!({"action": "switch", "enabled": false})).unwrap();
    assert!(!svc.view().enabled);

    // A household member's chat can only file a source under its own profile.
    let folder = r.root.join("pats-notes");
    std::fs::create_dir_all(&folder).unwrap();
    let path = folder.to_string_lossy().into_owned();
    let why = call(
        "pats",
        json!({"action": "add", "kind": "folder", "path": path, "personId": home.id}),
    )
    .unwrap_err();
    assert!(why.contains("own profile"), "{why}");
    // The card says whose person the source is filed under.
    let card = |chat: &str, person: &str| {
        let kind = kinds.get(crate::tool::KIND).unwrap();
        r.db.snapshot(|tx| {
            Ok::<_, rusqlite::Error>((kind.canonical)(
                tx,
                chat,
                &json!({"action": "add", "kind": "folder", "path": path, "personId": person}),
            ))
        })
        .unwrap()
        .map(|c| (c.details, c.target["personId"].as_str().unwrap().to_owned()))
    };
    let (details, filed) = card("pats", "").unwrap();
    assert_eq!(details[0], "Filed under: Pat");
    assert_eq!(filed, pat.id);
    assert!(card("pats", &home.id).is_err());
    assert_eq!(card("mine", &pat.id).unwrap().0[0], "Filed under: Pat");
    assert_eq!(
        card("mine", "").unwrap().0[0],
        format!("Filed under: {}", home.name)
    );
}
