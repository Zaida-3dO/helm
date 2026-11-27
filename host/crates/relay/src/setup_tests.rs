//! Staging and applying setup changes, end to end over a temp profile, an in-memory keychain and
//! a loopback hub: what is applied, what is refused, and that the pairing code shows up nowhere.

use super::*;
use crate::pair::tests::{fake_hub, TOKEN};
use helm_core::db::MigrationRegistry;
use helm_core::prefs::{self, PrefKey};
use helm_ipc::RecordingSink;
use helm_platform::keychain::MemoryKeychain;
use std::net::TcpListener;

/// The code as the file holds it, and as Helm sends it.
const TYPED: &str = "abcd-1234";
const CODE: &str = "ABCD1234";

struct Env {
    dir: tempfile::TempDir,
    services: Arc<Services>,
    sink: Arc<RecordingSink>,
    db: Arc<Database>,
}

fn env() -> Env {
    env_with(true)
}

/// `own_hub_ok`: the staging check against this PC's own addresses is relaxed, because the test
/// hub listens on a loopback port.
fn env_with(own_hub_ok: bool) -> Env {
    let dir = tempfile::tempdir().unwrap();
    let profile = helm_core::profile::Profile::open(&dir.path().join("profile")).unwrap();
    let platform = helm_platform::Platform {
        keychain: Arc::new(MemoryKeychain::default()),
        ..helm_platform::Platform::native()
    };
    let mut reg = MigrationRegistry::new();
    helm_chats::register_migrations(&mut reg);
    helm_profiles::register_migrations(&mut reg);
    helm_consent::register_migrations(&mut reg);
    helm_hosttools::register_migrations(&mut reg);
    let db = Arc::new(Database::open(&dir.path().join("t.db"), &reg).unwrap());
    db.write(|tx| {
        tx.execute(
            "INSERT INTO projects(id, name, created_at) VALUES ('p', 'P', 1)",
            [],
        )?;
        for c in ["home", "guest"] {
            tx.execute(
                "INSERT INTO chats(id, project_id, created_at, updated_at) VALUES (?1, 'p', 1, 1)",
                [c],
            )?;
        }
        // A household member's profile, linked to another hub person, owns the guest chat.
        helm_profiles::store::ensure_home(tx, 1)?;
        let alex = helm_profiles::store::create(tx, "Alex", None, Some("9"), 2)?;
        helm_profiles::store::assign(tx, &["guest".into()], &alex.id)?;
        Ok::<_, helm_profiles::store::ProfileError>(())
    })
    .unwrap();
    let sink = Arc::new(RecordingSink::default());
    let events: Arc<dyn EventSink> = sink.clone();
    let creds = crate::credentials(&profile, &platform);
    let mut kinds = AllowKinds::new();
    register_allow_kinds(&mut kinds);
    let mut s = Services::new();
    s.insert(Arc::new(helm_mcp::McpStore::new(&profile.dir, creds)));
    s.insert(Arc::new(helm_updates::UpdateService::new(
        db.clone(),
        events.clone(),
        helm_updates::scan::Version::parse("2.0.1").unwrap(),
    )));
    if own_hub_ok {
        s.insert(Arc::new(AllowOwnHub));
    }
    let rules = crate::pref_rules::pref_rules(&s, &events);
    s.insert(Arc::new(helm_hosttools::PrefRules(rules)));
    let mut extensions = vec![crate::settings_extension(&profile.dir)];
    extensions.extend(helm_profiles::settings_keys::settings_extensions(
        &Services::new(),
    ));
    s.insert(Arc::new(helm_hosttools::SettingsExtensions(extensions)));
    s.insert(Arc::new(profile));
    s.insert(Arc::new(platform));
    s.insert(db.clone());
    s.insert(Arc::new(kinds));
    s.insert(Arc::new(helm_consent::MaintenanceLock::default()));
    Env {
        dir,
        services: Arc::new(s),
        sink,
        db,
    }
}

impl Env {
    /// Stage changes for the copy that belongs to `person`, a profile named by the chat.
    fn stage_for(&self, chat: &str, person: &str, changes: Value) -> Result<Value, String> {
        let mut reg = HostToolRegistry::new();
        register_host_tools(&mut reg);
        let helm_hosttools::ToolHandler::Blocking(f) = reg.get(TOOL).unwrap().handler.clone()
        else {
            panic!("blocking tool")
        };
        let ctx = ToolCtx {
            caller_chat_id: chat.into(),
            is_child: false,
            tool_call_id: None,
            origin: helm_hosttools::CallOrigin::Agent,
            services: self.services.clone(),
            events: self.sink.clone(),
            ops: None,
        };
        f(
            &ctx,
            json!({"changes": changes, "person": person, "reason": "The user asked for this."}),
        )
        .map_err(|e| e.public)
    }

    fn stage(&self, chat: &str, changes: Value) -> Result<Value, String> {
        let mut reg = HostToolRegistry::new();
        register_host_tools(&mut reg);
        let helm_hosttools::ToolHandler::Blocking(f) = reg.get(TOOL).unwrap().handler.clone()
        else {
            panic!("blocking tool")
        };
        let ctx = ToolCtx {
            caller_chat_id: chat.into(),
            is_child: false,
            tool_call_id: None,
            origin: helm_hosttools::CallOrigin::Agent,
            services: self.services.clone(),
            events: self.sink.clone(),
            ops: None,
        };
        f(
            &ctx,
            json!({"changes": changes, "reason": "The user asked to set up this PC."}),
        )
        .map_err(|e| e.public)
    }

    /// The user's click: decide, wait for the apply, and read the outcome.
    fn approve(&self, request_id: &str) -> String {
        let events: Arc<dyn EventSink> = self.sink.clone();
        let d = helm_consent::decide_and_apply(
            &self.services,
            &events,
            &helm_consent::ConsentDecideArgs {
                request_id: request_id.into(),
                decision: helm_consent::Decision::Allow,
                grant_minutes: None,
                note: None,
            },
            "app",
        )
        .unwrap();
        d.applying.unwrap().join().unwrap();
        self.db
            .snapshot(|tx| {
                tx.query_row(
                    "SELECT outcome FROM consent_requests WHERE id = ?1",
                    [request_id],
                    |r| r.get::<_, Option<String>>(0),
                )
            })
            .unwrap()
            .unwrap_or_default()
    }

    /// A chat's own `app_update_settings`.
    fn update(&self, chat: &str, changes: Value) -> Result<Value, String> {
        let mut reg = HostToolRegistry::new();
        helm_hosttools::settings::register_host_tools(&mut reg);
        let helm_hosttools::ToolHandler::Blocking(f) =
            reg.get("app_update_settings").unwrap().handler.clone()
        else {
            panic!("blocking tool")
        };
        let ctx = ToolCtx {
            caller_chat_id: chat.into(),
            is_child: false,
            tool_call_id: None,
            origin: helm_hosttools::CallOrigin::Agent,
            services: self.services.clone(),
            events: self.sink.clone(),
            ops: None,
        };
        f(&ctx, json!({"changes": changes})).map_err(|e| e.public)
    }

    /// The Telegram policy file of the profile that owns `chat`.
    fn telegram_policy(&self, chat: &str) -> crate::telegram::PolicyFile {
        let who: String = self
            .db
            .snapshot(|tx| {
                tx.query_row(
                    "SELECT COALESCE(NULLIF(profile_id, ''), (SELECT id FROM profiles WHERE is_home = 1))
                       FROM chats WHERE id = ?1",
                    [chat],
                    |r| r.get(0),
                )
            })
            .unwrap();
        let dir = self
            .services
            .get::<helm_core::profile::Profile>()
            .unwrap()
            .dir
            .clone();
        crate::telegram::load_file(&crate::people::telegram_paths(&dir, &who).policy)
    }

    fn pref(&self, key: &str) -> Option<Value> {
        self.db
            .snapshot(|tx| Ok::<_, rusqlite::Error>(prefs::read(tx, key)))
            .unwrap()
    }

    fn paired(&self) -> config::RelayConfig {
        let p = self.services.get::<helm_core::profile::Profile>().unwrap();
        let pl = self.services.get::<helm_platform::Platform>().unwrap();
        config::load(&crate::credentials(&p, &pl))
    }

    /// Everything stored or sent about requests: targets, summaries, outcomes, and events.
    fn everything_said(&self) -> String {
        let rows: Vec<String> = self
            .db
            .snapshot(|tx| {
                tx.prepare(
                    "SELECT target || summary || COALESCE(outcome, '') FROM consent_requests",
                )?
                .query_map([], |r| r.get(0))?
                .collect()
            })
            .unwrap();
        let events: Vec<String> = self
            .sink
            .events
            .lock()
            .unwrap()
            .iter()
            .map(|e| format!("{} {:?}", e.kind, e.fields))
            .collect();
        format!("{rows:?} {events:?}")
    }
}

fn hub_answer(person: &str, machine: &str) -> String {
    // The hub's real shape: ids as numbers.
    json!({"token": TOKEN, "personId": 3, "personName": person, "machineId": 7,
           "machineName": machine, "scopes": ["relay"]})
    .to_string()
}

fn dead_address() -> String {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    format!("ws://{}/relay", l.local_addr().unwrap())
}

#[test]
fn staged_settings_apply_all_or_nothing_and_never_show_the_code() {
    enum Hub {
        Answers(&'static str, String),
        Down,
    }
    // (case, the hub, the person the chat named, folder removed before Approve, outcome starts,
    //  paired after, switches and folder changed after)
    let rows = [
        (
            "all four",
            Hub::Answers("200 OK", hub_answer("Pat", "Laptop")),
            "Pat",
            false,
            "Applied. Paired as Pat on Laptop",
            true,
            true,
        ),
        (
            "names differ",
            Hub::Answers("200 OK", hub_answer("Sam", "Desk")),
            "Pat",
            false,
            "Nothing changed: The hub paired this code as Sam on Desk, not Pat on Laptop",
            false,
            false,
        ),
        (
            "code refused",
            Hub::Answers("403 Forbidden", "{}".into()),
            "Pat",
            false,
            "Nothing changed: That code is not valid",
            false,
            false,
        ),
        (
            "hub down",
            Hub::Down,
            "Pat",
            false,
            "Nothing changed: The hub could not be reached",
            false,
            false,
        ),
        (
            "folder gone",
            Hub::Down,
            "Pat",
            true,
            "Nothing changed: The update folder must be an existing folder",
            false,
            false,
        ),
        // The switch is turned off by someone else between the card and the approval: the card
        // said "on", so nothing applies, not even the pairing.
        (
            "moved since the card",
            Hub::Down,
            "Pat",
            false,
            "Nothing changed: agentSettingsAll is not what the card showed",
            false,
            false,
        ),
    ];
    for (case, hub, person, remove_folder, outcome_starts, paired, changed) in rows {
        let e = env();
        let code_file = e.dir.path().join("pair.txt");
        std::fs::write(&code_file, format!("{TYPED}\n")).unwrap();
        let folder = e.dir.path().join("dist");
        std::fs::create_dir(&folder).unwrap();
        let (address, seen) = match hub {
            Hub::Answers(status, body) => {
                let (url, t) = fake_hub(status, body);
                (url, Some(t))
            }
            Hub::Down => (dead_address(), None),
        };
        let staged = e
            .stage(
                "home",
                json!({
                    "hubPairing": {"address": address, "codeFile": code_file.to_str().unwrap(),
                                   "person": person, "machine": "Laptop"},
                    "updateSource": folder.to_str().unwrap(),
                    "agentSettingsAll": true,
                }),
            )
            .unwrap();
        let lines: Vec<String> = serde_json::from_value(staged["changes"].clone()).unwrap();
        assert!(
            lines[0].contains(&format!("as {person} on Laptop")),
            "{case}: {lines:?}"
        );
        assert!(lines
            .iter()
            .any(|l| l.contains(code_file.to_str().unwrap())));
        assert!(lines.contains(&"agentSettingsAll: off → on".to_owned()));
        assert!(lines.contains(&format!(
            "Update folder: none → {}",
            folder.to_str().unwrap()
        )));
        if remove_folder {
            std::fs::remove_dir(&folder).unwrap();
        }

        let moved = case == "moved since the card";
        if moved {
            e.db.write(|tx| {
                prefs::set(
                    tx,
                    PrefKey::classified("agentSettingsAll"),
                    &Value::Bool(true),
                )
            })
            .unwrap();
        }
        let outcome = e.approve(staged["requestId"].as_str().unwrap());
        assert!(outcome.starts_with(outcome_starts), "{case}: {outcome}");
        if let Some(t) = seen {
            assert!(
                t.join().unwrap().contains(CODE),
                "{case}: the hub got the code"
            );
        }
        assert_eq!(e.paired().usable(), paired, "{case}");
        assert_eq!(
            e.pref("agentSettingsAll"),
            (changed || moved).then_some(Value::Bool(true)),
            "{case}: the switches change only with everything else"
        );
        assert_eq!(
            e.pref("updateSource").is_some(),
            changed,
            "{case}: the folder changes only with everything else"
        );
        if changed {
            assert_eq!(e.paired().person_id, "3");
            assert!(outcome.contains("The code file can be deleted."));
            assert!(e.sink.kinds().contains(&"preferences".to_string()));
        } else {
            assert!(
                e.services
                    .get::<helm_mcp::McpStore>()
                    .unwrap()
                    .load()
                    .map(|l| l.is_empty())
                    .unwrap_or(true),
                "{case}: no memory server"
            );
        }
        if case == "names differ" || case == "code refused" {
            assert!(outcome.contains("ask for a new one"), "{case}: {outcome}");
        }
        // The code appears nowhere: not the tool result, the card, the target, the outcome or
        // any event.
        let said = format!("{staged} {outcome} {}", e.everything_said());
        for secret in [CODE, TYPED, TOKEN] {
            assert!(!said.contains(secret), "{case}: {secret} leaked");
        }
    }
}

#[test]
fn staging_is_refused_when_it_would_be_wrong_or_change_nothing() {
    let e = env_with(false);
    let code_file = e.dir.path().join("pair.txt");
    std::fs::write(&code_file, TYPED).unwrap();
    let big = e.dir.path().join("big.txt");
    std::fs::write(&big, "x".repeat(300)).unwrap();
    let file = code_file.to_str().unwrap();
    let pairing = |file: &str, person: &str| {
        json!({"hubPairing": {"address": "ws://192.168.1.10:8000", "codeFile": file,
                              "person": person, "machine": "Laptop"}})
    };
    // (chat, changes, part of the refusal)
    let rows = [
        (
            "home",
            json!({"agentSettingsAll": false}),
            "agentSettingsAll is already off",
        ),
        (
            "home",
            json!({"updateSource": null}),
            "updateSource is already none",
        ),
        ("home", json!({}), "at least one change"),
        (
            "home",
            json!({"theme": "dark"}),
            "theme needs no approval: change it yourself with app_update_settings",
        ),
        (
            "home",
            json!({"nonsense": 1}),
            "nonsense is not a Helm setting",
        ),
        (
            "home",
            json!({"newChatPermissionMode": "plan"}),
            "acceptEdits",
        ),
        (
            "home",
            json!({"newChatPermissionMode": null}),
            "newChatPermissionMode is already default",
        ),
        (
            "home",
            json!({"credentials": {"for": "bank"}}),
            "credentials for must be one of",
        ),
        (
            "home",
            json!({"credentials": {"for": "mcp", "value": "hunter2"}}),
            "never staged",
        ),
        ("home", json!({"credentials": "hunter2"}), "never staged"),
        (
            "home",
            json!({"updateSource": "relative\\dist"}),
            "existing folder",
        ),
        ("home", pairing("relative.txt", "Pat"), "absolute path"),
        ("home", pairing(r"\\?\C:\pair.txt", "Pat"), "absolute path"),
        ("home", pairing(r"\\.\pipe\pair", "Pat"), "absolute path"),
        (
            "home",
            pairing(big.to_str().unwrap(), "Pat"),
            "more than a pairing code",
        ),
        (
            "home",
            pairing(e.dir.path().to_str().unwrap(), "Pat"),
            "plain file",
        ),
        ("home", pairing(file, ""), "person must be"),
        ("home", pairing(file, "Pat\u{7}"), "person must be"),
        (
            "home",
            json!({"hubPairing": {"address": "ws://8.8.8.8:8000", "codeFile": file,
                                  "person": "Pat", "machine": "Laptop"}}),
            "never public",
        ),
        (
            "home",
            json!({"hubPairing": {"address": "ws://127.0.0.1:8000", "codeFile": file,
                                  "person": "Pat", "machine": "Laptop"}}),
            "this PC itself",
        ),
        ("home", pairing(&format!("{file}"), "Pat"), "absolute path"),
    ];
    for (chat, changes, part) in rows {
        let err = e.stage(chat, changes.clone()).unwrap_err();
        assert!(
            err.starts_with("[Helm] Nothing was staged:"),
            "{changes}: {err}"
        );
        assert!(err.contains(part), "{changes}: {err}");
        assert!(!err.contains(TYPED) && !err.contains(CODE));
    }
    let pending: i64 =
        e.db.snapshot(|tx| tx.query_row("SELECT COUNT(*) FROM consent_requests", [], |r| r.get(0)))
            .unwrap();
    assert_eq!(pending, 0, "nothing was filed");

    // A good one files one request, and nothing an agent calls applies it.
    let ok = e.stage("home", pairing(file, "Pat")).unwrap();
    assert_eq!(ok["state"], "pending");
    assert!(!e.paired().usable());
}

#[test]
fn any_approval_setting_is_staged_applied_and_left_for_the_users_undo() {
    let e = env();
    let folder = e.dir.path().join("dist");
    std::fs::create_dir(&folder).unwrap();
    let staged = e
        .stage(
            "home",
            json!({
                "newChatPermissionMode": "acceptEdits",
                "agentSettingsAll": true,
                "updateSource": folder.to_str().unwrap(),
            }),
        )
        .unwrap();
    // The chat is told at once and carries on; nothing has changed yet.
    assert_eq!(staged["state"], "pending");
    let lines: Vec<String> = serde_json::from_value(staged["changes"].clone()).unwrap();
    assert_eq!(lines.len(), 3, "{lines:?}");
    assert!(lines.contains(&"newChatPermissionMode: default → acceptEdits".to_owned()));
    assert!(lines.contains(&"agentSettingsAll: off → on".to_owned()));
    assert_eq!(e.pref("newChatPermissionMode"), None);
    let outcome = e.approve(staged["requestId"].as_str().unwrap());
    assert!(outcome.starts_with("Applied."), "{outcome}");
    assert!(
        outcome.contains("newChatPermissionMode is now acceptEdits."),
        "{outcome}"
    );
    assert_eq!(e.pref("newChatPermissionMode"), Some(json!("acceptEdits")));
    assert_eq!(e.pref("agentSettingsAll"), Some(json!(true)));
    // The user's change list has them for Undo, under the chat that staged them.
    let listed: Vec<(String, String)> =
        e.db.snapshot(|tx| {
            tx.prepare("SELECT key, chat_id FROM settings_changes ORDER BY key")?
                .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
                .collect()
        })
        .unwrap();
    assert_eq!(listed.len(), 3, "{listed:?}");
    assert!(listed.iter().all(|(_, c)| c == "home"));
    assert!(e.sink.kinds().contains(&"external_settings".to_string()));

    // A setting that moved after the card was made is not applied: the user approved the lines
    // they saw, not whatever the setting has become.
    let staged = e.stage("home", json!({"agentSettingsAll": false})).unwrap();
    e.db.write(|tx| {
        helm_core::prefs::set(
            tx,
            helm_core::prefs::PrefKey::classified("agentSettingsAll"),
            &json!(false),
        )
    })
    .unwrap();
    let outcome = e.approve(staged["requestId"].as_str().unwrap());
    assert!(
        outcome.starts_with("Nothing changed: agentSettingsAll is not what the card showed"),
        "{outcome}"
    );

    // A household member's chat stages a change exactly as the PC's own person's chat does.
    let staged = e.stage("guest", json!({"agentSettingsAll": true})).unwrap();
    assert_eq!(staged["state"], "pending");

    // A server list reads as names and addresses, and what starts a program, on the card.
    let servers = json!([
        {"name": "Mine", "baseUrl": "http://127.0.0.1:9", "folder": "D:/S", "enabled": false},
        {"name": "Other", "baseUrl": "http://127.0.0.1:8", "preset": "Fast"},
    ]);
    assert_eq!(
        show("localServers", &servers),
        "Mine at http://127.0.0.1:9, folder D:/S (off); Other at http://127.0.0.1:8, preset Fast"
    );
}

/// A value is one row of the card however odd or long it is: a line break or a text-reordering
/// mark cannot add or hide a row, and a long value is cut in its row with a mark and given whole
/// under the rows, never refused for its length.
#[test]
fn a_value_is_one_row_of_the_card_however_odd_or_long_it_is() {
    let e = env();
    let policy = |to: &str| Staged {
        settings: BTreeMap::from([(
            "gatekeeper".to_owned(),
            Change {
                from: json!({"policy": "Old."}),
                to: json!({"policy": to}),
                value: json!({"policy": to}),
                lines: Vec::new(),
            },
        )]),
        ..Staged::default()
    };
    let long = "Ask me first. ".repeat(70);
    let huge = "x".repeat(9000);
    // (the new policy, whether the row points to the full text, text the full text must hold)
    for (to, in_full, expect) in [
        ("Ask me first.", false, ""),
        (
            "Ask me first.\nNothing else on this card changes.\r\nOr this.",
            false,
            "",
        ),
        ("a\u{202e}b\u{1b}[31mc\u{200b}", false, ""),
        (long.as_str(), true, "Ask me first. Ask me first."),
        (huge.as_str(), true, "more characters not shown"),
    ] {
        let target = serde_json::to_value(policy(to)).unwrap();
        let card =
            e.db.snapshot(|tx| {
                canonical(tx, "home", &target).map_err(rusqlite::Error::InvalidParameterName)
            })
            .unwrap();
        let rows: Vec<&str> = card.summary.lines().collect();
        assert_eq!(rows.len(), 2, "one title and one row: {rows:?}");
        assert!(
            rows[1].starts_with("gatekeeper.policy: Old. \u{2192} "),
            "{rows:?}"
        );
        assert!(
            card.summary
                .chars()
                .all(|c| c == '\n' || !c.is_control() && !('\u{202a}'..='\u{202e}').contains(&c)),
            "{:?}",
            card.summary
        );
        assert!(rows
            .iter()
            .all(|l| l.chars().count() <= helm_consent::kinds::MAX_SUMMARY_LINE));
        assert_eq!(rows[1].ends_with("(in full below)"), in_full, "{rows:?}");
        assert_eq!(card.details.is_empty(), !in_full);
        assert!(card.details.join("\n").contains(expect), "{expect}");
        assert!(card.details.iter().all(|l| l.chars().count() <= 240));
    }
    let odd = policy("Ask.\nNothing else changes.");
    let (lines, _) = card_lines(&odd);
    assert!(
        lines[1].contains('\u{23ce}'),
        "the break is shown: {lines:?}"
    );
}

/// Removing a profile is one card that names the profile, says where its chats go and that there
/// is no Undo, however many profiles there are; Approve removes exactly that one.
#[test]
fn removing_a_profile_is_one_clear_card_however_many_profiles_there_are() {
    let e = env();
    let ids: Vec<(String, String)> = e
        .db
        .write(|tx| {
            let mut out = Vec::new();
            for (n, name) in ["Bea", "Cato", "Dara", "Eliot", "Fenella-with-a-long-name"]
                .iter()
                .enumerate()
            {
                let p = helm_profiles::store::create(tx, name, None, None, 3 + n as i64)?;
                out.push((p.id, p.name));
            }
            // Two chats of Cato's move to Home with him.
            for c in ["c1", "c2"] {
                tx.execute(
                    "INSERT INTO chats(id, project_id, created_at, updated_at) VALUES (?1, 'p', 1, 1)",
                    [c],
                )?;
            }
            helm_profiles::store::assign(tx, &["c1".into(), "c2".into()], &out[1].0)?;
            Ok::<_, helm_profiles::store::ProfileError>(out)
        })
        .unwrap();
    let (cato_id, _) = ids[1].clone();
    // Home, Alex and five more: a list of every name and id is far past one row.
    let staged = e
        .stage("home", json!({"deleteProfile": {"id": cato_id}}))
        .unwrap();
    assert_eq!(staged["state"], "pending");
    let lines: Vec<String> = serde_json::from_value(staged["changes"].clone()).unwrap();
    let said = lines.join("\n");
    assert!(said.contains("Remove the profile Cato"), "{said}");
    assert!(
        said.contains("Its 2 chats (0 working now) move to"),
        "{said}"
    );
    assert!(said.contains("there is no Undo"), "{said}");
    assert!(
        !said.contains("Bea"),
        "only the removed profile is named: {said}"
    );
    let outcome = e.approve(staged["requestId"].as_str().unwrap());
    assert!(outcome.starts_with("Applied."), "{outcome}");
    let left: Vec<String> =
        e.db.snapshot(|tx| {
            tx.prepare("SELECT name FROM profiles ORDER BY name")?
                .query_map([], |r| r.get(0))?
                .collect()
        })
        .unwrap();
    assert_eq!(left.len(), 6);
    assert!(!left.contains(&"Cato".to_owned()) && left.contains(&"Bea".to_owned()));
    // The chats are the home profile's now, and no Undo is offered for the removal.
    let owner: String =
        e.db.snapshot(|tx| {
            tx.query_row("SELECT profile_id FROM chats WHERE id = 'c1'", [], |r| {
                r.get(0)
            })
        })
        .unwrap();
    let home: String =
        e.db.snapshot(|tx| {
            tx.query_row("SELECT id FROM profiles WHERE is_home = 1", [], |r| {
                r.get(0)
            })
        })
        .unwrap();
    assert_eq!(owner, home);
    let ring: i64 =
        e.db.snapshot(|tx| {
            tx.query_row(
                "SELECT COUNT(*) FROM settings_changes WHERE key = 'deleteProfile'",
                [],
                |r| r.get(0),
            )
        })
        .unwrap();
    assert_eq!(ring, 0, "a removal has no Undo, so the ring holds none");
}

/// The home person's older single-person policy file is not lost when a chat is the first to
/// change it: the change lands on that file, and the move into the person's folder keeps it.
#[test]
fn a_chats_first_change_keeps_the_older_policy_file_of_the_home_person() {
    let e = env();
    let dir = e
        .services
        .get::<helm_core::profile::Profile>()
        .unwrap()
        .dir
        .clone();
    let older = crate::telegram::policy_path(&dir);
    std::fs::create_dir_all(older.parent().unwrap()).unwrap();
    std::fs::write(&older, r#"{"maxAgeHours": 12, "maxTokens": 400000}"#).unwrap();
    e.update("home", json!({"telegram": {"model": "opus"}}))
        .unwrap();
    let kept = crate::telegram::load_file(&older);
    assert_eq!((kept.max_age_hours, kept.new_chat().0), (12, Some("opus")));
    crate::people::migrate(&dir, &e.db);
    let moved = e.telegram_policy("home");
    assert_eq!(
        (moved.max_age_hours, moved.new_chat().0),
        (12, Some("opus"))
    );
}

/// A secret never travels through a chat: staging `credentials` names a page, the card says so,
/// and Approve opens that page without storing or sending anything back.
#[test]
fn a_chat_asks_for_a_secret_by_opening_the_page_never_by_carrying_it() {
    let e = env();
    let staged = e
        .stage("guest", json!({"credentials": {"for": "localmodels"}}))
        .unwrap();
    let lines: Vec<String> = serde_json::from_value(staged["changes"].clone()).unwrap();
    assert_eq!(
        lines,
        ["Open Settings > Local models so you can type a local model server's key yourself (no secret is staged)"]
    );
    let before = e.sink.kinds().len();
    let outcome = e.approve(staged["requestId"].as_str().unwrap());
    assert!(
        outcome.starts_with("Applied. Settings > Local models is open"),
        "{outcome}"
    );
    let events = e.sink.events.lock().unwrap().clone();
    let open = events
        .iter()
        .skip(before)
        .find(|ev| ev.kind == "shell_open_settings")
        .expect("the page is opened");
    assert_eq!(open.fields["pane"], "localmodels");
    // Nothing was stored.
    let changed: i64 =
        e.db.snapshot(|tx| tx.query_row("SELECT COUNT(*) FROM settings_changes", [], |r| r.get(0)))
            .unwrap();
    assert_eq!(changed, 0);
}

/// Two people's chats reach their own copy of a per-person setting, the same switches are
/// staged for both, and neither is turned away.
#[test]
fn each_person_changes_their_own_channel_policy_and_stages_the_same_switches() {
    let e = env();
    e.update("home", json!({"telegram": {"model": "opus"}}))
        .unwrap();
    e.update(
        "guest",
        json!({"telegram": {"model": "haiku", "maxAgeHours": 12}}),
    )
    .unwrap();
    let (home, guest) = (e.telegram_policy("home"), e.telegram_policy("guest"));
    assert_eq!(home.new_chat().0, Some("opus"));
    assert_eq!(guest.new_chat().0, Some("haiku"));
    assert_eq!((home.max_age_hours, guest.max_age_hours), (24, 12));
    // The switches that decide what an outside answer may approve are staged, for both alike.
    let mut said = Vec::new();
    for chat in ["home", "guest"] {
        let why = e
            .update(chat, json!({"telegram": {"approvals": true}}))
            .unwrap_err();
        assert!(
            why.contains("telegram.approvals") && why.contains("app_stage_settings"),
            "{why}"
        );
        said.push(why);
    }
    assert_eq!(said[0], said[1]);
    assert!(!e.telegram_policy("guest").approvals);
    let staged = e
        .stage("guest", json!({"telegram": {"approvals": true}}))
        .unwrap();
    assert_eq!(staged["changes"][0], "telegram.approvals: off → on");
    let outcome = e.approve(staged["requestId"].as_str().unwrap());
    assert!(outcome.starts_with("Applied."), "{outcome}");
    assert!(e.telegram_policy("guest").approvals);
    assert!(
        !e.telegram_policy("home").approvals,
        "the other person's copy is untouched"
    );
    // Naming the other person reaches their copy: from the guest's chat, to change it directly
    // and to stage the switch, and the card says whose copy it is.
    let home_id: String =
        e.db.snapshot(|tx| {
            tx.query_row("SELECT id FROM profiles WHERE is_home = 1", [], |r| {
                r.get(0)
            })
        })
        .unwrap();
    let mut args = json!({"changes": {"telegram": {"model": "sonnet"}}, "person": home_id});
    let mut reg = HostToolRegistry::new();
    helm_hosttools::settings::register_host_tools(&mut reg);
    let helm_hosttools::ToolHandler::Blocking(f) =
        reg.get("app_update_settings").unwrap().handler.clone()
    else {
        panic!("blocking tool")
    };
    let ctx = ToolCtx {
        caller_chat_id: "guest".into(),
        is_child: false,
        tool_call_id: None,
        origin: helm_hosttools::CallOrigin::Agent,
        services: e.services.clone(),
        events: e.sink.clone(),
        ops: None,
    };
    f(&ctx, args.clone()).unwrap();
    assert_eq!(e.telegram_policy("home").new_chat().0, Some("sonnet"));
    assert_eq!(
        e.telegram_policy("guest").new_chat().0,
        Some("haiku"),
        "the guest's own is untouched"
    );
    args["changes"] = json!({"telegram": {"remoteAnswers": true}});
    assert!(f(&ctx, args)
        .unwrap_err()
        .public
        .contains("app_stage_settings"));
    let staged = e
        .stage_for(
            "guest",
            &home_id,
            json!({"telegram": {"remoteAnswers": true}}),
        )
        .unwrap();
    assert!(
        staged["changes"][0].as_str().unwrap().starts_with("For "),
        "{staged}"
    );
    let outcome = e.approve(staged["requestId"].as_str().unwrap());
    assert!(outcome.starts_with("Applied."), "{outcome}");
    assert!(e.telegram_policy("home").remote_answers);
    assert!(!e.telegram_policy("guest").remote_answers);
}
