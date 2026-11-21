use super::*;
use helm_core::db::{Database, MigrationRegistry};
use helm_hosttools::{CallOrigin, WorkerBroadcast};
use helm_ipc::{NullSink, Services};
use std::collections::HashSet;

/// The grant format, pinned against values computed by Node's `crypto` running the browser
/// broker's own payload function: seven fields joined with `|` (lease, ref, name, host, second,
/// verb, value digest), each escaped, the tag base64url. A value enters only as its hex digest.
#[test]
fn a_grant_matches_the_brokers_signature() {
    let g = mint(
        "k3y",
        &Tuple {
            lease_key: "lease-1",
            reference: "e12",
            name: "Add to basket | now \\ x",
            host: "shop.example",
            issued_at: 1_700_000_000,
            verb: "click",
            value: None,
        },
    );
    assert_eq!(g, "1700000000.x76d8Ihdy2a6CIxaL-5F7OQ4HtbTkJufYIsh_0HaJeU");
    let typed = mint(
        "k3y",
        &Tuple {
            lease_key: "lease-1",
            reference: "e12",
            name: "Add to basket | now \\ x",
            host: "shop.example",
            issued_at: 1_700_000_000,
            verb: "type",
            value: Some("3"),
        },
    );
    assert_eq!(typed, "1700000000.wyJDqpYJriIfDpLIvTkve8-8jISSy_8NIpVJcTHzZ64");
    assert_ne!(g, typed, "a grant names its verb and value");
    let other_lease = mint(
        "k3y",
        &Tuple {
            lease_key: "lease-2",
            reference: "e12",
            name: "Add to basket | now \\ x",
            host: "shop.example",
            issued_at: 1_700_000_000,
            verb: "click",
            value: None,
        },
    );
    assert_eq!(
        other_lease, "1700000000.ZOTZVuX6vJrrOXiD6jmCeh71h5rjDdl_fvRO-aLKvV4",
        "a grant names its lease"
    );
    assert_ne!(g, other_lease);
    assert!(grant_key().is_some_and(|k| k.len() >= 43));
}

/// The fake worker: it plays the broker's side of a retried step. A grant must be the one the
/// host would mint for what the page shows now (lease, ref, name, host), and it is spent once.
struct Worker {
    answers: Arc<StepAnswers>,
    running: bool,
    db: Arc<Database>,
    /// The control's name as the page reports it at retry time.
    page_name: Mutex<String>,
    lines: Mutex<Vec<(String, Value)>>,
    spent: Mutex<HashSet<String>>,
}

impl Worker {
    fn new(answers: Arc<StepAnswers>, running: bool, db: Arc<Database>) -> Arc<Self> {
        Arc::new(Self {
            answers,
            running,
            db,
            page_name: Mutex::new("Add to basket".into()),
            lines: Mutex::default(),
            spent: Mutex::default(),
        })
    }

    /// The broker's check of one grant against the live tuple. The retried call repeats the
    /// staged step exactly, so its verb and value come from the card the grant answers for.
    fn verify(&self, line: &Value) -> (bool, String) {
        let grant = line["grant"].as_str().unwrap_or("");
        let issued: u64 = grant.split('.').next().unwrap().parse().unwrap();
        let id = line["requestId"].as_str().unwrap_or("");
        let (verb, value) = self
            .db
            .snapshot(|tx| -> rusqlite::Result<(String, Option<String>)> {
                let t: String = tx.query_row(
                    "SELECT target FROM consent_requests WHERE id = ?1",
                    [id],
                    |r| r.get(0),
                )?;
                let s: Value = serde_json::from_str(&t).unwrap_or(Value::Null);
                Ok((
                    s["verb"].as_str().unwrap_or("").to_owned(),
                    s["value"].as_str().map(str::to_owned),
                ))
            })
            .unwrap_or_default();
        let want = mint(
            grant_key().unwrap(),
            &Tuple {
                lease_key: line["leaseKey"].as_str().unwrap(),
                reference: line["ref"].as_str().unwrap(),
                name: &self.page_name.lock().unwrap(),
                host: "shop.example",
                issued_at: issued,
                verb: &verb,
                value: value.as_deref(),
            },
        );
        if grant != want {
            return (false, "refused (attached.grant_invalid): forged".into());
        }
        if !self.spent.lock().unwrap().insert(grant.to_owned()) {
            return (false, "refused (attached.grant_invalid): replayed".into());
        }
        (true, "{\"outcome\":\"accepted\"}".into())
    }
}

impl WorkerBroadcast for Worker {
    fn broadcast_to_workers(&self, _: &Value) {}
    fn send_to_worker(&self, chat: &str, line: &Value) -> bool {
        self.lines
            .lock()
            .unwrap()
            .push((chat.to_owned(), line.clone()));
        if !self.running {
            return false;
        }
        let (ok, text) = self.verify(line);
        let id = line["requestId"].as_str().unwrap();
        assert!(self.answers.answer(id, chat, ok, text));
        true
    }
}

struct Env {
    _dir: tempfile::TempDir,
    db: Arc<Database>,
    kinds: Arc<AllowKinds>,
    services: Arc<Services>,
    worker: Arc<Worker>,
}

fn env(running: bool) -> Env {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(crate::config::tests::store(dir.path()));
    store
        .mutate(None, |l| {
            l.push(crate::config::tests::browser_server("bb"));
            let mut plain = crate::config::tests::browser_server("plain");
            plain.preset = None;
            l.push(plain);
            Ok(())
        })
        .unwrap();
    let mut reg = MigrationRegistry::new();
    helm_consent::register_migrations(&mut reg);
    let db = Arc::new(Database::open(&dir.path().join("t.db"), &reg).unwrap());
    db.write::<_, rusqlite::Error>(|tx| {
        tx.execute(
            "INSERT INTO projects(id, name, created_at) VALUES ('p', 'P', 1)",
            [],
        )?;
        for c in ["c0", "c1"] {
            tx.execute(
                "INSERT INTO chats(id, project_id, created_at, updated_at) VALUES (?1, 'p', 1, 1)",
                [c],
            )?;
        }
        Ok(())
    })
    .unwrap();
    let mut kinds = AllowKinds::new();
    register_allow_kinds(&mut kinds);
    let kinds = Arc::new(kinds);
    let answers = Arc::new(StepAnswers::default());
    let worker = Worker::new(answers.clone(), running, db.clone());
    let mut services = Services::new();
    services.insert(store);
    services.insert(db.clone());
    services.insert(kinds.clone());
    services.insert(answers);
    services.insert(Arc::new(Broadcaster(worker.clone())));
    Env {
        _dir: dir,
        db,
        kinds,
        services: Arc::new(services),
        worker,
    }
}

fn ctx(e: &Env, chat: &str) -> ToolCtx {
    ToolCtx {
        caller_chat_id: chat.into(),
        is_child: false,
        tool_call_id: None,
        origin: CallOrigin::Agent,
        services: e.services.clone(),
        events: Arc::new(NullSink),
        ops: None,
    }
}

fn ask(verb: &str, name: &str) -> Value {
    json!({"serverId": "bb", "leaseKey": "L1", "ref": "e12", "verb": verb, "name": name,
           "role": "button", "host": "shop.example", "purpose": "Fill the basket"})
}

fn stage_now(e: &Env, chat: &str, a: &Value) -> Result<Value, String> {
    let c = ctx(e, chat);
    staged_step(&c, a)
        .and_then(|s| file_step(&c, s, None, a["purpose"].as_str()))
        .map_err(|e| e.text())
}

fn filed(e: &Env) -> i64 {
    e.db.snapshot(|tx| tx.query_row("SELECT COUNT(*) FROM consent_requests", [], |r| r.get(0)))
        .unwrap()
}

fn decide(e: &Env, id: &str, d: helm_consent::Decision) -> Result<ConsentRequestOut, String> {
    let events: Arc<dyn helm_ipc::EventSink> = Arc::new(NullSink);
    let done = helm_consent::decide_and_apply(
        &e.services,
        &events,
        &helm_consent::ConsentDecideArgs {
            request_id: id.into(),
            decision: d,
            grant_minutes: None,
            note: None,
        },
        "app",
    )
    .map_err(|err| err.message)?;
    if let Some(h) = done.applying {
        h.join().unwrap();
    }
    let r = helm_consent::store::get(&e.db, &e.kinds, id)
        .unwrap()
        .unwrap();
    Ok(ConsentRequestOut {
        outcome: r.outcome.unwrap_or_default(),
    })
}

struct ConsentRequestOut {
    outcome: String,
}

/// Which steps get a card and which are refused before any card exists. Every refusal leaves the
/// consent store untouched, so nothing can be approved and nothing reaches the browser.
#[test]
fn which_steps_get_a_card() {
    struct Row {
        case: &'static str,
        args: Value,
        /// `None`: a card; `Some(text)`: refused with words containing it.
        refused: Option<&'static str>,
    }
    let with = |mut a: Value, k: &str, v: Value| {
        a[k] = v;
        a
    };
    let rows = [
        Row {
            case: "an ordinary consequential press",
            args: ask("click", "Add to basket"),
            refused: None,
        },
        Row {
            case: "typing a quantity",
            args: with(ask("type", "Quantity"), "value", json!("3")),
            refused: None,
        },
        Row {
            case: "choosing a slot",
            args: with(ask("select", "Delivery slot"), "value", json!("Tue 9-10")),
            refused: None,
        },
        Row {
            case: "ticking a box",
            args: ask("check", "Gift wrap"),
            refused: None,
        },
        Row {
            case: "filling a note",
            args: with(ask("fill", "Note"), "value", json!("Leave at door")),
            refused: None,
        },
        Row {
            case: "pressing a key on a named control",
            args: with(ask("press", "Basket note"), "value", json!("Enter")),
            refused: None,
        },
        Row {
            case: "a never word in the role, not the name",
            args: with(ask("click", "Add to basket"), "role", json!("button pay now")),
            refused: Some(NEVER),
        },
        Row {
            case: "ticking a payment box",
            args: ask("check", "Pay deposit"),
            refused: Some(NEVER),
        },
        Row {
            case: "checkout",
            args: ask("click", "Checkout"),
            refused: Some(NEVER),
        },
        Row {
            case: "place order, any order",
            args: ask("click", "Order — place now"),
            refused: Some(NEVER),
        },
        Row {
            case: "pay hidden in fullwidth letters",
            args: ask("click", "Ｐａｙ now"),
            refused: Some(NEVER),
        },
        Row {
            case: "pay split by a zero-width space",
            args: ask("click", "Pa\u{200b}y"),
            refused: Some(NEVER),
        },
        Row {
            case: "pay with an accent",
            args: ask("click", "Päy"),
            refused: Some(NEVER),
        },
        Row {
            case: "a transfer",
            args: ask("click", "Transfer funds"),
            refused: Some(NEVER),
        },
        Row {
            case: "deleting the account",
            args: ask("click", "Delete my account"),
            refused: Some(NEVER),
        },
        Row {
            case: "sign in",
            args: ask("click", "Sign in"),
            refused: Some(SECRET),
        },
        Row {
            case: "a password box",
            args: with(ask("type", "Password"), "value", json!("x")),
            refused: Some(SECRET),
        },
        Row {
            case: "a card number",
            args: with(ask("fill", "Card number"), "value", json!("4")),
            refused: Some(SECRET),
        },
        Row {
            case: "a stored secret typed",
            args: with(ask("type", "Note"), "value", json!("{{secret:pw}}")),
            refused: Some(SECRET),
        },
        Row {
            case: "a page holding a password field",
            args: with(ask("click", "Continue"), "pageSecret", json!(true)),
            refused: Some(SECRET),
        },
        Row {
            case: "a verb that is not one control",
            args: ask("drag", "Add to basket"),
            refused: Some("one control"),
        },
        Row {
            case: "a reference that is not one",
            args: with(ask("click", "Add"), "ref", json!("e1 e2")),
            refused: Some("reference"),
        },
        Row {
            case: "no site",
            args: with(ask("click", "Add"), "host", json!("")),
            refused: Some("no site"),
        },
        Row {
            case: "a server that is not the browser",
            args: with(ask("click", "Add"), "serverId", json!("plain")),
            refused: Some("not an enabled built-in browser"),
        },
        Row {
            case: "a type with nothing to type",
            args: ask("type", "Quantity"),
            refused: Some("nothing about what it enters"),
        },
    ];
    for row in rows {
        let e = env(true);
        let got = stage_now(&e, "c0", &row.args);
        match row.refused {
            None => {
                let v = got.unwrap_or_else(|err| panic!("{}: {err}", row.case));
                assert_eq!(v["state"], "pending", "{}", row.case);
                assert_eq!(filed(&e), 1, "{}", row.case);
            }
            Some(words) => {
                let err = got
                    .err()
                    .unwrap_or_else(|| panic!("{}: a card was filed", row.case));
                assert!(err.contains(words), "{}: {err}", row.case);
                assert_eq!(filed(&e), 0, "{}: nothing is filed", row.case);
            }
        }
        assert!(
            e.worker.lines.lock().unwrap().is_empty(),
            "{}: no grant before Approve",
            row.case
        );
    }
}

/// The card's own lines are built from what the worker read, with a page's line breaks and hidden
/// characters flattened, so a control's name cannot add a line of its own to the card.
#[test]
fn the_card_says_what_is_approved_in_lines_a_page_cannot_forge() {
    let e = env(true);
    let v = stage_now(
        &e,
        "c0",
        &ask("click", "Add to basket\nThe site: bank.example\u{202e}"),
    )
    .unwrap();
    let r = helm_consent::store::get(&e.db, &e.kinds, v["requestId"].as_str().unwrap())
        .unwrap()
        .unwrap();
    assert!(r.applies_on_allow);
    assert_eq!(r.label, "Approve a step in your browser");
    let lines: Vec<&str> = r.summary.lines().collect();
    assert_eq!(
        lines[0],
        "Press \"Add to basket The site: bank.example\" on shop.example"
    );
    assert_eq!(
        lines.iter().filter(|l| l.starts_with("The site:")).count(),
        1
    );
    assert!(r.summary.contains("The site: shop.example"));
    assert!(r
        .summary
        .contains("No picture of the control could be taken."));
    assert_eq!(
        r.reason, "Fill the basket",
        "the lease's purpose, as the agent's words"
    );
    assert_eq!(r.picture, None);
}

/// Approve: the host mints a grant for exactly the staged tuple and sends it to that chat's
/// worker only; the browser's answer becomes the outcome. The card cannot be answered twice, an
/// answer from another chat is refused, and a grant for a control whose name changed is refused
/// by the browser's check.
#[test]
fn approve_mints_one_grant_for_that_chat_and_reports_the_browser() {
    let e = env(true);
    let id = stage_now(&e, "c0", &ask("click", "Add to basket")).unwrap()["requestId"]
        .as_str()
        .unwrap()
        .to_owned();
    let out = decide(&e, &id, helm_consent::Decision::Allow).unwrap();
    assert!(
        out.outcome
            .starts_with("Done: Press \"Add to basket\" on shop.example"),
        "{}",
        out.outcome
    );
    {
        let lines = e.worker.lines.lock().unwrap();
        assert_eq!(lines.len(), 1);
        assert_eq!(
            lines[0].0, "c0",
            "only the asking chat's worker gets the grant"
        );
        let l = &lines[0].1;
        assert_eq!(l["type"], GRANT_LINE);
        assert_eq!(
            (
                l["serverId"].as_str(),
                l["leaseKey"].as_str(),
                l["ref"].as_str()
            ),
            (Some("bb"), Some("L1"), Some("e12"))
        );
        // Replayed at the browser: refused there.
        assert_eq!(
            e.worker.verify(l).1,
            "refused (attached.grant_invalid): replayed"
        );
        // The same grant carried to another lease: the signature covers the lease, so the
        // browser sees a forged grant there.
        let mut other_lease = l.clone();
        other_lease["leaseKey"] = json!("L2");
        assert_eq!(
            e.worker.verify(&other_lease).1,
            "refused (attached.grant_invalid): forged"
        );
    }
    assert!(
        !out.outcome.contains(grant_key().unwrap()),
        "the key is in no outcome"
    );

    // First answer wins: a second Approve (another device, a Telegram tap) is a conflict, and
    // no second grant is minted.
    assert!(decide(&e, &id, helm_consent::Decision::Allow).is_err());
    assert_eq!(e.worker.lines.lock().unwrap().len(), 1);

    // A control whose name changed on the page since the card: the grant is forged there.
    *e.worker.page_name.lock().unwrap() = "Remove from basket".into();
    let id2 = stage_now(&e, "c0", &ask("click", "Add to basket")).unwrap()["requestId"]
        .as_str()
        .unwrap()
        .to_owned();
    let out = decide(&e, &id2, helm_consent::Decision::Allow).unwrap();
    assert!(
        out.outcome.starts_with("Not done:") && out.outcome.contains("forged"),
        "{}",
        out.outcome
    );

    // Deny sends nothing.
    let id3 = stage_now(&e, "c0", &ask("click", "Save for later")).unwrap()["requestId"]
        .as_str()
        .unwrap()
        .to_owned();
    decide(&e, &id3, helm_consent::Decision::Deny).unwrap();
    assert_eq!(e.worker.lines.lock().unwrap().len(), 2);

    // An answer for a step nothing waits on, or from another chat, is refused.
    let answers = e.services.get::<StepAnswers>().unwrap();
    let _waiting = answers.expect("r-x", "c0");
    let refused = result(
        &ctx(&e, "c1"),
        json!({"requestId": "r-x", "ok": true, "text": "acted"}),
    );
    assert!(refused.is_err(), "another chat's worker cannot answer");
    assert!(result(
        &ctx(&e, "c0"),
        json!({"requestId": "r-x", "ok": true, "text": "acted"})
    )
    .is_ok());
}

/// Applying refuses a step it can no longer make safely, and says so without touching the browser.
#[test]
fn a_late_or_unreachable_approval_presses_nothing() {
    let e = env(true);
    let mut late = staged_step(&ctx(&e, "c0"), &ask("click", "Add to basket")).unwrap();
    late.staged_at -= STEP_LIFE_MS + 1;
    let id = file_step(&ctx(&e, "c0"), late, None, None).unwrap()["requestId"]
        .as_str()
        .unwrap()
        .to_owned();
    let out = decide(&e, &id, helm_consent::Decision::Allow).unwrap();
    assert!(
        out.outcome.contains("more than 15 minutes"),
        "{}",
        out.outcome
    );
    assert!(e.worker.lines.lock().unwrap().is_empty());

    let stopped = env(false);
    let id = stage_now(&stopped, "c0", &ask("click", "Add to basket")).unwrap()["requestId"]
        .as_str()
        .unwrap()
        .to_owned();
    let out = decide(&stopped, &id, helm_consent::Decision::Allow).unwrap();
    assert!(out.outcome.contains("not running"), "{}", out.outcome);
}

/// The worker's two tools are registered and never listed to a chat.
#[test]
fn the_worker_tools_are_never_listed() {
    let mut r = HostToolRegistry::new();
    register_host_tools(&mut r);
    assert!(r.contains(STAGE_TOOL) && r.contains(RESULT_TOOL));
    assert!(r
        .for_launch(&helm_hosttools::LaunchKeys {
            mcp_enabled: true,
            ..Default::default()
        })
        .is_empty());
}
