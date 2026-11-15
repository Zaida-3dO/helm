//! Staging and applying setup changes, end to end over a temp profile, an in-memory keychain and
//! a loopback hub: what is applied, what is refused, and that the pairing code shows up nowhere.

use super::*;
use crate::pair::tests::{fake_hub, TOKEN};
use helm_core::db::MigrationRegistry;
use helm_core::prefs::{self, DEVICE_RUN_FROM_OTHERS};
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
        events,
        helm_updates::scan::Version::parse("2.0.1").unwrap(),
    )));
    if own_hub_ok {
        s.insert(Arc::new(AllowOwnHub));
    }
    let rules = crate::pref_rules::pref_rules(&s);
    s.insert(Arc::new(helm_hosttools::PrefRules(rules)));
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
                    "deviceRunFromOthers": false,
                    "deviceHouseholdRun": false,
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
        assert!(lines.contains(&format!("{RUN_FROM_OTHERS}: on → off")));
        assert!(lines.contains(&format!(
            "Update folder: none → {}",
            folder.to_str().unwrap()
        )));
        if remove_folder {
            std::fs::remove_dir(&folder).unwrap();
        }

        // The switch is turned off by someone else between the card and the approval: the card
        // said "on", so the outcome names the difference.
        if changed {
            e.db.write(|tx| prefs::set(tx, DEVICE_RUN_FROM_OTHERS, &Value::Bool(false)))
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
            e.pref("deviceRunFromOthers"),
            changed.then_some(Value::Bool(false)),
            "{case}: the switches change only with everything else"
        );
        assert_eq!(
            e.pref("updateSource").is_some(),
            changed,
            "{case}: the folder changes only with everything else"
        );
        if changed {
            assert_eq!(e.paired().person_id, "3");
            assert_eq!(e.pref("deviceHouseholdRun"), Some(Value::Bool(false)));
            assert!(outcome.contains("The code file can be deleted."));
            assert!(
                outcome.contains("was off, not on as the card said."),
                "{outcome}"
            );
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
            "guest",
            json!({"deviceRunFromOthers": false}),
            "household member",
        ),
        (
            "home",
            json!({"deviceRunFromOthers": true}),
            "deviceRunFromOthers is already on",
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
            "theme is a setting you change yourself",
        ),
        (
            "home",
            json!({"agentSettingsAll": true}),
            "agentSettingsAll is yours to change only",
        ),
        (
            "home",
            json!({"nonsense": 1}),
            "not a setting that can be staged",
        ),
        ("home", json!({"terminalShell": "zsh"}), "pwsh"),
        (
            "home",
            json!({"terminalShell": "pwsh"}),
            "terminalShell is already pwsh",
        ),
        (
            "home",
            json!({"householdSharing": {"chats": 1}}),
            "householdSharing",
        ),
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

    // A good one files one request; a household member's chat cannot, and nothing an agent
    // calls applies it.
    let ok = e.stage("home", pairing(file, "Pat")).unwrap();
    assert_eq!(ok["state"], "pending");
    assert!(!e.paired().usable());
}

#[test]
fn any_high_setting_is_staged_applied_and_left_for_the_users_undo() {
    let e = env();
    let staged = e
        .stage(
            "home",
            json!({
                "terminalShell": "gitbash",
                "nativeMcp": false,
                "backgroundModel": {"local": "qwen"},
                "householdSharing": {"chats": true, "transcripts": true},
            }),
        )
        .unwrap();
    // The chat is told at once and carries on; nothing has changed yet.
    assert_eq!(staged["state"], "pending");
    let lines: Vec<String> = serde_json::from_value(staged["changes"].clone()).unwrap();
    assert_eq!(lines.len(), 4, "{lines:?}");
    assert!(lines.contains(&"terminalShell: pwsh → gitbash".to_owned()));
    assert!(lines.contains(&"nativeMcp: on → off".to_owned()));
    assert_eq!(e.pref("terminalShell"), None);
    let outcome = e.approve(staged["requestId"].as_str().unwrap());
    assert!(outcome.starts_with("Applied."), "{outcome}");
    assert!(
        outcome.contains("terminalShell is now gitbash."),
        "{outcome}"
    );
    assert_eq!(e.pref("terminalShell"), Some(json!("gitbash")));
    assert_eq!(e.pref("nativeMcp"), Some(json!(false)));
    assert_eq!(e.pref("backgroundModel"), Some(json!({"local": "qwen"})));
    // The owner's rule filled in the switch the others depend on being off.
    assert_eq!(
        e.pref("householdSharing"),
        Some(json!({"chats": true, "transcripts": true, "messages": false}))
    );
    // The user's change list has them for Undo, under the chat that staged them.
    let listed: Vec<(String, String)> =
        e.db.snapshot(|tx| {
            tx.prepare("SELECT key, chat_id FROM settings_changes ORDER BY key")?
                .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
                .collect()
        })
        .unwrap();
    assert_eq!(listed.len(), 4, "{listed:?}");
    assert!(listed.iter().all(|(_, c)| c == "home"));
    assert!(e.sink.kinds().contains(&"external_settings".to_string()));
}
