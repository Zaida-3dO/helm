//! Staging and applying MCP server changes end to end over a temp profile: the exact card, what
//! is saved on Approve, what is refused, and that no secret value shows up anywhere.

use super::*;
use crate::config::tests::store;
use helm_consent::{ConsentDecideArgs, Decision};
use helm_core::db::MigrationRegistry;
use helm_hosttools::{CallOrigin, ToolHandler};
use helm_ipc::{EventSink, RecordingSink, Services};

struct Env {
    dir: tempfile::TempDir,
    store: Arc<McpStore>,
    db: Arc<Database>,
    services: Arc<Services>,
    sink: Arc<RecordingSink>,
}

fn env() -> Env {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(store(dir.path()));
    let mut reg = MigrationRegistry::new();
    helm_consent::register_migrations(&mut reg);
    let db = Arc::new(Database::open(&dir.path().join("t.db"), &reg).unwrap());
    db.write::<_, rusqlite::Error>(|tx| {
        tx.execute(
            "INSERT INTO projects(id, name, created_at) VALUES ('p', 'P', 1)",
            [],
        )?;
        tx.execute(
            "INSERT INTO chats(id, project_id, created_at, updated_at) VALUES ('c0', 'p', 1, 1)",
            [],
        )?;
        tx.execute_batch(
            "CREATE TABLE profiles(id TEXT PRIMARY KEY, name TEXT, is_home INTEGER);
             INSERT INTO profiles VALUES ('home', 'Home', 1), ('sam', 'Sam', 0);",
        )
    })
    .unwrap();
    let mut kinds = AllowKinds::new();
    register_allow_kinds(&mut kinds);
    let sink = Arc::new(RecordingSink::default());
    let mut s = Services::new();
    s.insert(store.clone());
    s.insert(db.clone());
    s.insert(Arc::new(kinds));
    s.insert(Arc::new(helm_consent::MaintenanceLock::default()));
    Env {
        dir,
        store,
        db,
        services: Arc::new(s),
        sink,
    }
}

impl Env {
    /// Call one MCP tool as an agent from chat `c0`; the error is the text the agent reads.
    fn call(&self, tool: &str, args: Value) -> Result<Value, String> {
        let mut reg = HostToolRegistry::new();
        crate::tools::register_host_tools(&mut reg);
        let ToolHandler::Blocking(f) = reg.get(tool).expect("tool").handler.clone() else {
            panic!("blocking tool")
        };
        let ctx = ToolCtx {
            caller_chat_id: "c0".into(),
            is_child: false,
            tool_call_id: None,
            origin: CallOrigin::Agent,
            services: self.services.clone(),
            events: self.sink.clone(),
            ops: None,
        };
        f(&ctx, args).map_err(|e| e.text())
    }

    fn revision(&self) -> String {
        self.store.snapshot().unwrap().revision
    }

    /// Stage `args` on `tool` with askUser and return the pending request's id.
    fn ask(&self, tool: &str, mut args: Value) -> String {
        args["askUser"] = json!(true);
        args["reason"] = json!("The user asked for this.");
        self.card(self.call(tool, args))
    }

    fn card(&self, answer: Result<Value, String>) -> String {
        let v = answer.expect("a card is filed");
        assert_eq!(v["state"], "pending");
        let mut keys: Vec<_> = v.as_object().unwrap().keys().cloned().collect();
        keys.sort();
        assert_eq!(
            keys,
            ["note", "requestId", "state"],
            "the agent hears no card line"
        );
        v["requestId"].as_str().unwrap().to_owned()
    }

    fn column(&self, id: &str, col: &str) -> String {
        self.db
            .snapshot(|tx| {
                tx.query_row(
                    &format!("SELECT COALESCE({col}, '') FROM consent_requests WHERE id = ?1"),
                    [id],
                    |r| r.get::<_, String>(0),
                )
            })
            .unwrap()
    }

    /// The user's click. Approving waits for the apply and returns its outcome.
    fn decide(&self, id: &str, decision: Decision) -> String {
        let events: Arc<dyn EventSink> = self.sink.clone();
        let d = helm_consent::decide_and_apply(
            &self.services,
            &events,
            &ConsentDecideArgs {
                request_id: id.into(),
                decision,
                grant_minutes: None,
                note: None,
            },
            "app",
        )
        .unwrap();
        if let Some(h) = d.applying {
            h.join().unwrap();
        }
        self.column(id, "outcome")
    }

    fn put(&self, server: McpServer) {
        self.store
            .mutate(None, |l| {
                l.push(server);
                Ok(())
            })
            .unwrap();
    }

    fn server(&self, id: &str) -> McpServer {
        self.store
            .load()
            .unwrap()
            .into_iter()
            .find(|s| s.id == id)
            .unwrap_or_else(|| panic!("no server {id}"))
    }

    fn env_of(&self, server: &str, key: &str, profile: &str) -> String {
        let (_, all) = self
            .store
            .resolve_enabled_in(
                &|k| (k == "SOME_VAR").then(|| "from-host".to_owned()),
                None,
                false,
                false,
                Some(profile),
            )
            .unwrap();
        all.into_iter().find(|s| s.id == server).unwrap().env[key].clone()
    }
}

fn literal(value: &str) -> ValueRef {
    ValueRef {
        kind: RefKind::Literal,
        value: Some(value.into()),
        secret_id: None,
        prefix: None,
        profiles: Default::default(),
    }
}

fn stdio(id: &str, name: &str, env: Vec<(&str, ValueRef)>) -> McpServer {
    McpServer {
        id: id.into(),
        name: name.into(),
        enabled: true,
        transport: Transport {
            kind: TransportKind::Stdio,
            command: Some("node.exe".into()),
            args: vec!["a.js".into()],
            env: env.into_iter().map(|(k, v)| (k.to_owned(), v)).collect(),
            url: None,
            headers: Default::default(),
            oauth: None,
        },
        allow_private_http: false,
        preset: None,
    }
}

#[test]
fn a_change_the_user_approves_is_saved_as_the_card_showed_it() {
    let e = env();
    e.put(stdio("fake", "Fake", vec![]));

    // An agent's own call to run the browser broker is refused with how to ask, and changes
    // nothing; the same call with askUser files a card showing every line and no variable's
    // source name.
    let add = |rev: &str, env: Value, extra: Value| {
        let mut a = json!({"revision": rev, "server": {"id": "bk", "name": "Browser copy", "enabled": true,
            "transport": {"type": "stdio", "command": "node.exe",
                "args": ["broker-tool.js", "--port", "9"], "env": env}}});
        a["server"]["transport"]["args"][2] = extra;
        a
    };
    let broker_env = json!({"BROKER_TOKEN": {"kind": "environment", "value": "X_HOST_VAR"}});
    let rev = e.revision();
    let refused = e
        .call(admin::ADD, add(&rev, broker_env.clone(), json!("9")))
        .unwrap_err();
    assert!(refused.contains("askUser: true"), "{refused}");
    assert_eq!(e.store.load().unwrap().len(), 1);
    let id = e.ask(admin::ADD, add(&rev, broker_env, json!("9")));
    assert_eq!(
        e.column(&id, "summary"),
        [
            "Add the MCP server Browser copy",
            "Runs: node.exe",
            "Argument 1: broker-tool.js",
            "Argument 2: --port",
            "Argument 3: 9",
            "Variables (names only): BROKER_TOKEN (a variable on this PC)",
            "Approving saves this once. The program runs when a chat next uses this server.",
        ]
        .join("\n")
    );
    assert!(!e.column(&id, "summary").contains("X_HOST_VAR"));
    assert_eq!(
        e.store.load().unwrap().len(),
        1,
        "nothing is saved before the click"
    );
    let outcome = e.decide(&id, Decision::Allow);
    assert!(outcome.starts_with("Applied."), "{outcome}");
    assert_eq!(e.server("bk").transport.args[0], "broker-tool.js");

    // A change made after the card was filed: approving saves nothing and says so.
    let id = e.ask(
        admin::EDIT,
        json!({"revision": e.revision(), "serverId": "bk", "name": "Renamed"}),
    );
    e.put(stdio("later", "Later", vec![]));
    let outcome = e.decide(&id, Decision::Allow);
    assert!(
        outcome.starts_with("Not applied:") && outcome.contains("changed since"),
        "{outcome}"
    );
    assert_eq!(e.server("bk").name, "Browser copy");

    // Deny leaves the server as it is.
    let id = e.ask(
        admin::REMOVE,
        json!({"revision": e.revision(), "serverId": "later"}),
    );
    e.decide(&id, Decision::Deny);
    assert_eq!(e.server("later").name, "Later");

    // A built-in preset is removed only on a card; Approve takes it away.
    e.store
        .mutate(None, |l| {
            l.iter_mut().find(|s| s.id == "later").unwrap().preset =
                Some(crate::google::PRESET_ID.into());
            Ok(())
        })
        .unwrap();
    let refused = e
        .call(
            admin::REMOVE,
            json!({"revision": e.revision(), "serverId": "later"}),
        )
        .unwrap_err();
    assert!(refused.contains("askUser: true"), "{refused}");
    let id = e.ask(
        admin::REMOVE,
        json!({"revision": e.revision(), "serverId": "later"}),
    );
    assert!(e.decide(&id, Decision::Allow).starts_with("Applied."));
    assert!(e.store.load().unwrap().iter().all(|s| s.id != "later"));

    // A card that could not show the whole change is not filed: a typed secret, a long argument,
    // a hidden character, an edit that changes nothing.
    let rev = e.revision();
    for (what, args, says) in [
        (
            "a typed secret",
            json!({"revision": rev, "server": {"id": "s1", "name": "S", "enabled": true,
                "transport": {"type": "stdio", "command": "node.exe",
                    "env": {"TOKEN": {"kind": "literal", "value": "hunter2"}}}}}),
            "never carries a secret value",
        ),
        (
            "a 300-character argument",
            json!({"revision": rev, "server": {"id": "s2", "name": "S", "enabled": true,
                "transport": {"type": "stdio", "command": "node.exe", "args": ["a".repeat(300)]}}}),
            "too long to show whole",
        ),
        (
            "a hidden character",
            json!({"revision": rev, "server": {"id": "s3", "name": "S", "enabled": true,
                "transport": {"type": "stdio", "command": "node.exe", "args": ["a\u{202e}b"]}}}),
            "invisible",
        ),
    ] {
        let mut a = args;
        a["askUser"] = json!(true);
        a["reason"] = json!("x");
        let err = e.call(admin::ADD, a).unwrap_err();
        assert!(err.contains(says), "{what}: {err}");
        assert!(!err.contains("hunter2"), "{what}: {err}");
    }
    let err = e
        .call(
            admin::UPDATE,
            json!({"revision": e.revision(), "serverId": "bk", "enabled": true, "askUser": true}),
        )
        .unwrap_err();
    assert!(err.contains("as it is"), "{err}");
    assert_eq!(e.store.load().unwrap().len(), 2);
}

#[test]
fn a_person_gets_their_own_secret_from_a_file_and_the_level_of_a_google_server_changes() {
    let e = env();
    e.put(stdio(
        "api",
        "Api",
        vec![
            ("TOKEN", literal("shared-1")),
            (
                "FROM_HOST",
                ValueRef {
                    kind: RefKind::Environment,
                    value: Some("SOME_VAR".into()),
                    ..literal("")
                },
            ),
        ],
    ));
    let file = |name: &str, text: &str| {
        let p = e.dir.path().join(name);
        std::fs::write(&p, text).unwrap();
        p.to_string_lossy().into_owned()
    };
    let secret = |profile: &str, name: &str, path: &str, keep: bool| {
        json!({"server": "Api", "profile": profile, "secret": name, "valueFile": path,
            "keepFile": keep, "reason": "The user asked."})
    };

    // The card names the path and the person, never the value; Approve saves it for that person
    // only and deletes the file.
    let path = file("pw.txt", "pw-123\n");
    let id = e.card(e.call(SECRET_TOOL, secret("sam", "TOKEN", &path, false)));
    let card = e.column(&id, "summary");
    assert!(
        card.contains(&path) && card.contains("Set TOKEN for Sam on Api"),
        "{card}"
    );
    assert!(
        card.contains("The file is then deleted") && !card.contains("pw-123"),
        "{card}"
    );
    let outcome = e.decide(&id, Decision::Allow);
    assert!(
        outcome.starts_with("Applied.") && !outcome.contains("pw-123"),
        "{outcome}"
    );
    assert!(!Path::new(&path).exists());
    assert_eq!(e.env_of("api", "TOKEN", "sam"), "pw-123");
    assert_eq!(e.env_of("api", "TOKEN", "home"), "shared-1");
    assert!(
        !std::fs::read_to_string(e.dir.path().join(crate::config::FILE_NAME))
            .unwrap()
            .contains("pw-123")
    );

    // keepFile leaves the file; a secret read from a variable has no typed value to give.
    let kept = file("keep.txt", "pw-456");
    let id = e.card(e.call(SECRET_TOOL, secret("home", "TOKEN", &kept, true)));
    assert!(e.column(&id, "summary").contains("The file is kept"));
    assert!(e.decide(&id, Decision::Allow).starts_with("Applied."));
    assert!(Path::new(&kept).exists());
    assert_eq!(e.env_of("api", "TOKEN", "home"), "pw-456");
    for (args, says) in [
        (secret("sam", "FROM_HOST", &kept, false), "typed secret"),
        (secret("sam", "MISSING", &kept, false), "add MISSING first"),
        (secret("nobody", "TOKEN", &kept, false), "No profile"),
        (
            secret("sam", "TOKEN", "relative.txt", false),
            "absolute path",
        ),
        (
            secret("sam", "TOKEN", &e.dir.path().to_string_lossy(), false),
            "plain file",
        ),
    ] {
        let err = e.call(SECRET_TOOL, args).unwrap_err();
        assert!(err.contains(says), "{says}: {err}");
    }

    // The Google server's level belongs to every account on it; any other server has none.
    let mut g = stdio("google", "Google accounts", vec![]);
    g.transport.command = Some("uvx.exe".into());
    g.transport.args = crate::google::DEFAULT_ARGS.map(str::to_owned).to_vec();
    g.preset = Some(crate::google::PRESET_ID.into());
    e.put(g);
    let level = |server: &str, service: &str, level: &str| json!({"server": server, "service": service, "level": level});
    assert!(e
        .call(LEVEL_TOOL, level("Api", "calendar", "edit_events"))
        .unwrap_err()
        .contains("only a Google accounts server"));
    assert!(e
        .call(LEVEL_TOOL, level("Google accounts", "gmail", "drafts"))
        .unwrap_err()
        .contains("already"));
    let id = e.card(e.call(
        LEVEL_TOOL,
        level("Google accounts", "calendar", "edit_events"),
    ));
    assert!(e
        .column(&id, "summary")
        .contains("Google accounts: Calendar off → edit events (every account on this server)"));
    let outcome = e.decide(&id, Decision::Allow);
    assert!(
        outcome.starts_with("Applied.") && outcome.contains("sign in again"),
        "{outcome}"
    );
    let args = e.server("google").transport.args;
    assert_eq!(
        crate::google::calendar_shown(&args),
        Some(crate::google::CalendarPermission::EditEvents)
    );
    let id = e.card(e.call(LEVEL_TOOL, level("google", "gmail", "send")));
    assert!(e
        .column(&id, "summary")
        .contains("Each message still needs its own Allow."));
}
