//! Two devices talking through an in-memory hub: the host side of the device wire, end to end.
//!
//! One test walks the whole flow so the setup is paid for once: what a watcher may ask, what it
//! hears back, and every refusal. Time is paused, so the 250 ms batching and the hub's 30 second
//! timeout cost nothing. No network, no database.

use helm_chats::remote::{RemoteSlot, Routes};
use helm_chats::types::{
    ChatEvent, Interaction, InteractionAttention as Att, InteractionMethod as Meth,
};
use helm_core::db::{Database, MigrationRegistry};
use helm_devices::host::HostPort;
use helm_devices::link::{DeviceLink, MemLink, MemRouter, Rpc};
use helm_devices::mirror::{self, ChatView, Mirror, MirrorSlot, PeopleSync, WhoFilter};
use helm_devices::testing::{fake_interaction, fake_record, fake_row, FakePort};
use helm_devices::wire::{code, DeviceFrame, DevicePerson, Frame, Push};
use helm_devices::{ChatPort, Service};
use helm_ipc::{AppEvent, Dispatcher, OpCtx, OpIntercept, OpRegistry, RecordingSink, Services};
use rusqlite::OptionalExtension;
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};
use std::time::Duration;

struct Watcher {
    link: Arc<MemLink>,
    rpc: Arc<Rpc>,
    pushes: Arc<Mutex<Vec<Push>>>,
    name: String,
}

impl Watcher {
    /// A device of the host's own person.
    fn new(router: &MemRouter, uid: &str, name: &str, peer: bool) -> Watcher {
        Watcher::of(router, uid, name, peer, "person-a", "Alex")
    }

    /// A device of `person`.
    fn of(
        router: &MemRouter,
        uid: &str,
        name: &str,
        peer: bool,
        person: &str,
        person_name: &str,
    ) -> Watcher {
        let link = router.connect(uid, Some(person), Some(person_name), peer);
        let pushes = Arc::new(Mutex::new(Vec::new()));
        let seen = pushes.clone();
        link.subscribe(Box::new(move |f| {
            if let Frame::DevPush(p) = f {
                seen.lock().unwrap().push(p);
            }
        }));
        let rpc = Rpc::new(link.clone());
        let w = Watcher {
            link,
            rpc,
            pushes,
            name: name.to_owned(),
        };
        w.announce(uid);
        w
    }

    fn announce(&self, uid: &str) {
        self.link.send(Frame::Device(DeviceFrame {
            uid: uid.to_owned(),
            name: self.name.clone(),
            run: true,
            people: Vec::new(),
        }));
    }

    /// The pushes heard so far for one subscription, in arrival order.
    fn heard(&self, sub: &str) -> Vec<Push> {
        self.pushes
            .lock()
            .unwrap()
            .iter()
            .filter(|p| p.sub == sub)
            .cloned()
            .collect()
    }
}

async fn until(mut done: impl FnMut() -> bool) {
    for _ in 0..400 {
        if done() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    panic!("timed out waiting");
}

async fn settle() {
    tokio::time::sleep(Duration::from_millis(20)).await;
}

fn message(revision: i64, id: i64, text: &str) -> helm_ipc::AppEvent {
    ChatEvent::Message {
        chat_id: "c1".into(),
        revision,
        rows: vec![fake_row("c1", id, revision, text)],
    }
    .to_app_event()
}

fn refresh() -> helm_ipc::AppEvent {
    ChatEvent::Refresh {
        chat_id: "c1".into(),
    }
    .to_app_event()
}

#[tokio::test(start_paused = true)]
async fn a_watcher_reads_and_drives_a_chat_on_another_device() {
    let router = MemRouter::new();
    let port = Arc::new(FakePort::new("dev-a", "Desk"));
    let link_a = router.connect("dev-a", Some("person-a"), Some("Alex"), true);
    let host = Service::new(port.clone(), link_a, tokio::runtime::Handle::current());
    host.announce();
    let b = Watcher::new(&router, "dev-b", "Laptop", true);
    let c = Watcher::new(&router, "dev-c", "Tablet", false);
    settle().await;

    port.add_chat("c1", "First", 10);
    port.put_row(fake_row("c1", 1, 5, "hello"));
    port.put_row(fake_row("c1", 2, 6, "world"));

    // The hello: who this device is, who uses it, where chats can start.
    let hello = b.rpc.call("dev-a", "hello", json!({})).await.unwrap();
    assert_eq!(hello["device"]["name"], "Desk");
    assert_eq!(hello["people"][0]["personId"], "person-a");
    assert_eq!(hello["projects"][0]["path"], "/work");

    // What the hub and the host refuse, in one table.
    let big = "x".repeat(2 * 1024 * 1024);
    let refused: Vec<(&Watcher, &str, &str, Value, &str)> = vec![
        (&c, "dev-a", "hello", json!({}), code::SCOPE),
        (&b, "dev-zzz", "hello", json!({}), code::OFFLINE),
        (
            &b,
            "dev-a",
            "consent_decide",
            json!({"id": "x"}),
            code::BAD_ARGS,
        ),
        (
            &b,
            "dev-a",
            "chat_sub",
            json!({"chatId": "c1", "surprise": 1}),
            code::BAD_ARGS,
        ),
        (
            &b,
            "dev-a",
            "chat_sub",
            json!({"chatId": "nope"}),
            code::NOT_FOUND,
        ),
        (
            &b,
            "dev-a",
            "send",
            json!({"chatId": "c1", "clientId": "not-a-uuid", "text": "hi", "mode": "steer"}),
            code::BAD_ARGS,
        ),
        (
            &b,
            "dev-a",
            "send",
            json!({"chatId": "c1", "clientId": "7f0f2f0e-6b0e-4d52-8d3a-1f0e2d3c4b5a",
                   "text": "y".repeat(16 * 1024 + 1), "mode": "queue"}),
            code::BAD_ARGS,
        ),
        (
            &b,
            "dev-a",
            "rename",
            json!({"chatId": "c1", "title": big}),
            code::TOO_LARGE,
        ),
    ];
    for (w, to, op, args, want) in refused {
        let e = w.rpc.call(to, op, args).await.unwrap_err();
        assert_eq!(e.code, want, "{op} to {to}");
    }
    // The hub's own timeout: a device that is up but never answers.
    let _silent = router.connect("dev-d", None, None, true);
    let e = b.rpc.call("dev-d", "hello", json!({})).await.unwrap_err();
    assert_eq!(e.code, code::TIMEOUT);

    // The list: the draft stays home, the profile becomes the person, pushes follow in order.
    let list = b
        .rpc
        .call("dev-a", "chats_sub", json!({"since": 0}))
        .await
        .unwrap();
    let c1 = &list["chats"][0];
    assert_eq!(list["chats"].as_array().unwrap().len(), 1);
    assert_eq!(
        (c1["id"].as_str(), c1["personId"].as_str()),
        (Some("c1"), Some("person-a"))
    );
    assert_eq!(c1["draft"], "");
    assert!(c1.get("profileId").is_none());
    assert_eq!(list["cursor"], 100);
    for (title, rev) in [("Renamed", 11), ("Renamed again", 12)] {
        host.observe(
            &ChatEvent::Chat {
                chat: fake_record("c1", title, rev),
            }
            .to_app_event(),
        );
    }
    until(|| b.heard("chats").len() == 2).await;
    let seen = b.heard("chats");
    assert_eq!(seen.iter().map(|p| p.seq).collect::<Vec<_>>(), [1, 2]);
    assert_eq!(seen[1].from.as_deref(), Some("dev-a"));
    assert_eq!(seen[1].event["chat"]["title"], "Renamed again");

    // A chat: rows newer than the cursor only, then batched pushes (latest row per id wins).
    let sub = b
        .rpc
        .call("dev-a", "chat_sub", json!({"chatId": "c1", "since": 0}))
        .await
        .unwrap();
    assert_eq!(sub["rows"].as_array().unwrap().len(), 2);
    let sub = b
        .rpc
        .call("dev-a", "chat_sub", json!({"chatId": "c1", "since": 5}))
        .await
        .unwrap();
    assert_eq!(
        (sub["rows"].as_array().unwrap().len(), &sub["more"]),
        (1, &json!(false))
    );
    host.observe(&message(12, 2, "world!"));
    host.observe(&message(13, 2, "world!!"));
    host.observe(&message(13, 3, "new"));
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(b.heard("chat:c1").is_empty(), "rows wait for their batch");
    until(|| !b.heard("chat:c1").is_empty()).await;
    let batch = b.heard("chat:c1")[0].clone();
    assert_eq!(batch.seq, 1);
    assert_eq!(batch.event["kind"], "message");
    let rows = batch.event["rows"].as_array().unwrap();
    assert_eq!(
        rows.iter()
            .map(|r| (r["id"].as_i64().unwrap(), r["text"].as_str().unwrap()))
            .collect::<Vec<_>>(),
        [(2, "world!!"), (3, "new")]
    );
    host.observe(
        &ChatEvent::Pending {
            chat_id: "c1".into(),
            revision: 14,
            pending: vec![],
        }
        .to_app_event(),
    );
    host.observe(&refresh());
    until(|| b.heard("chat:c1").len() == 3).await;
    let kinds: Vec<_> = b
        .heard("chat:c1")
        .iter()
        .map(|p| (p.seq, p.event["kind"].as_str().unwrap().to_owned()))
        .collect();
    assert_eq!(
        kinds,
        [
            (1, "message".to_owned()),
            (2, "pending".to_owned()),
            (3, "refresh".to_owned())
        ]
    );
    // A gap is mended by subscribing again from the revision held: the count starts over.
    b.rpc
        .call("dev-a", "chat_sub", json!({"chatId": "c1", "since": 14}))
        .await
        .unwrap();
    host.observe(&refresh());
    until(|| b.heard("chat:c1").len() == 4).await;
    assert_eq!(b.heard("chat:c1")[3].seq, 1);

    // A row of two megabytes of tool output arrives cut, in the answer and in a push.
    port.put_row(fake_row("c1", 9, 20, &big));
    let sub = b
        .rpc
        .call("dev-a", "chat_sub", json!({"chatId": "c1", "since": 19}))
        .await
        .unwrap();
    let row = &sub["rows"][0];
    assert!(row["text"].as_str().unwrap().len() <= 64 * 1024 + 4);
    assert_eq!(row["details"]["remoteTrimmed"], true);
    host.observe(&message(21, 10, &big));
    until(|| b.heard("chat:c1").len() == 5).await;
    let pushed = &b.heard("chat:c1")[4].event["rows"][0];
    assert!(pushed["text"].as_str().unwrap().len() <= 64 * 1024 + 4);
    assert_eq!(pushed["details"]["remoteTrimmed"], true);

    // A send runs once however often it arrives; a refused one can be tried again.
    let id = "7f0f2f0e-6b0e-4d52-8d3a-1f0e2d3c4b5a";
    let send = json!({"chatId": "c1", "clientId": id, "text": "hi", "mode": "steer"});
    assert_eq!(
        b.rpc.call("dev-a", "send", send.clone()).await.unwrap(),
        json!({"accepted": true})
    );
    assert_eq!(
        b.rpc.call("dev-a", "send", send).await.unwrap(),
        json!({"accepted": true, "duplicate": true})
    );
    assert_eq!(
        port.st().sent,
        [("c1".to_owned(), id.to_owned(), "hi".to_owned())]
    );
    let lost = "0b1c2d3e-4f50-4a6b-8c7d-9e0f1a2b3c4d";
    let e = b
        .rpc
        .call(
            "dev-a",
            "send",
            json!({"chatId": "nope", "clientId": lost, "text": "hi", "mode": "steer"}),
        )
        .await
        .unwrap_err();
    assert_eq!(e.code, code::NOT_FOUND);
    assert!(!port.st().claimed.contains(lost));

    // With the switch off, only reading works.
    port.set_run(false);
    for (op, args) in [
        (
            "send",
            json!({"chatId": "c1", "clientId": lost, "text": "hi", "mode": "steer"}),
        ),
        ("stop", json!({"chatId": "c1"})),
        ("rename", json!({"chatId": "c1", "title": "x"})),
        (
            "create",
            json!({"personId": "person-a", "projectId": "proj-1"}),
        ),
        ("archive", json!({"chatId": "c1", "archived": true})),
    ] {
        let e = b.rpc.call("dev-a", op, args).await.unwrap_err();
        assert_eq!(e.code, code::NOT_ALLOWED, "{op}");
    }
    assert!(b.rpc.call("dev-a", "hello", json!({})).await.is_ok());
    port.set_run(true);
    assert_eq!(
        b.rpc
            .call(
                "dev-a",
                "create",
                json!({"personId": "person-a", "projectId": "proj-1"})
            )
            .await
            .unwrap(),
        json!({"chatId": "new-1"})
    );
    // A chat made from afar belongs to the acting person: the hub's verified person replaces
    // whatever name the request carried.
    assert_eq!(
        b.rpc
            .call(
                "dev-a",
                "create",
                json!({"personId": "nobody", "projectId": "proj-1"})
            )
            .await
            .unwrap(),
        json!({"chatId": "new-2"})
    );
    assert_eq!(
        port.st()
            .created
            .iter()
            .map(|c| c.person_id.as_str())
            .collect::<Vec<_>>(),
        ["person-a", "person-a"]
    );
    b.rpc
        .call("dev-a", "stop", json!({"chatId": "c1"}))
        .await
        .unwrap();
    assert_eq!(port.st().stopped, ["c1".to_owned()]);

    // Answers: every waiting request may be answered from afar — a question, a plan, a tool
    // permission, a trust prompt, a secret input — with the words the request shows; a doctored
    // label or a second answer may not.
    let secret: Interaction = Interaction {
        secret: Some(true),
        ..fake_interaction("c1", "secret", Meth::Input, Some(Att::Question), None)
    };
    let plan = ["Approve plan", "Keep planning"];
    let pick = ["A", "B"];
    let answers: Vec<(Option<Interaction>, &str, Value, Option<&str>)> = vec![
        (
            Some(fake_interaction(
                "c1",
                "perm",
                Meth::Confirm,
                Some(Att::Approval),
                None,
            )),
            "perm",
            json!({"confirmed": true, "label": "Allow"}),
            None,
        ),
        (
            Some(fake_interaction(
                "c1",
                "perm2",
                Meth::Confirm,
                Some(Att::Approval),
                None,
            )),
            "perm2",
            json!({"confirmed": true, "label": "Deny"}),
            Some(code::REFUSED),
        ),
        (
            Some(fake_interaction(
                "c1",
                "plan",
                Meth::Select,
                Some(Att::Plan),
                Some(&plan),
            )),
            "plan",
            json!({"value": "Approve plan", "confirmed": true, "label": "Approve plan"}),
            None,
        ),
        (
            Some(fake_interaction(
                "c1",
                "plan2",
                Meth::Select,
                Some(Att::Plan),
                Some(&plan),
            )),
            "plan2",
            json!({"value": "Keep planning", "confirmed": true, "label": "Keep planning"}),
            Some(code::REFUSED),
        ),
        (
            Some(fake_interaction(
                "c1",
                "trust",
                Meth::Confirm,
                Some(Att::Trust),
                None,
            )),
            "trust",
            json!({"confirmed": true, "label": "Allow"}),
            None,
        ),
        (
            Some(secret),
            "secret",
            json!({"value": "hunter2", "label": "Token"}),
            None,
        ),
        (
            Some(fake_interaction(
                "c1",
                "pick",
                Meth::Select,
                Some(Att::Question),
                Some(&pick),
            )),
            "pick",
            json!({"value": "B", "label": "B"}),
            None,
        ),
        (
            Some(fake_interaction(
                "c1",
                "pick2",
                Meth::Select,
                Some(Att::Question),
                Some(&pick),
            )),
            "pick2",
            json!({"value": "C", "label": "C"}),
            Some(code::REFUSED),
        ),
        (
            None,
            "gone",
            json!({"confirmed": true, "label": "Allow"}),
            Some(code::REFUSED),
        ),
        (
            None,
            "perm",
            json!({"confirmed": true, "label": "Allow"}),
            Some(code::REFUSED),
        ),
    ];
    for (waiting, request, extra, want) in answers {
        if let Some(i) = waiting {
            port.put_interaction(i);
        }
        let mut args = json!({"chatId": "c1", "requestId": request});
        args.as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        let got = b.rpc.call("dev-a", "answer", args).await;
        match want {
            None => assert!(got.is_ok(), "{request}: {got:?}"),
            Some(c) => assert_eq!(got.unwrap_err().code, c, "{request}"),
        }
    }
    let answered: Vec<_> = port
        .st()
        .answered
        .iter()
        .map(|a| a.request_id.clone())
        .collect();
    assert_eq!(answered, ["perm", "plan", "trust", "secret", "pick"]);

    // A device that drops off the hub loses its subscriptions, and asks again when it is back.
    assert!(host.watchers().contains(&"dev-b".to_owned()));
    b.link.set_up(false);
    settle().await;
    assert!(!host.watchers().contains(&"dev-b".to_owned()));
    assert_eq!(
        b.rpc
            .call("dev-a", "hello", json!({}))
            .await
            .unwrap_err()
            .code,
        code::OFFLINE
    );
    b.link.set_up(true);
    b.announce("dev-b");
    settle().await;
    b.rpc
        .call("dev-a", "chats_sub", json!({"since": 100}))
        .await
        .unwrap();
    assert!(host.watchers().contains(&"dev-b".to_owned()));
}

#[tokio::test(start_paused = true)]
async fn allow_cards_show_on_every_pc_and_the_first_answer_wins() {
    let router = MemRouter::new();
    let port = Arc::new(FakePort::new("dev-a", "Desk"));
    port.add_chat("c1", "First", 10);
    let link_a = router.connect("dev-a", Some("person-a"), Some("Alex"), true);
    let host = Service::new(port.clone(), link_a, tokio::runtime::Handle::current());
    host.announce();
    let laptop = Watcher::new(&router, "dev-b", "Laptop", true);
    let tablet = Watcher::new(&router, "dev-c", "Tablet", true);
    settle().await;

    let card = |id: &str| {
        json!({
            "id": id, "kind": "test-step", "label": "Step",
            "summary": format!("Do {id}"), "state": "pending", "grantMinutes": 30,
        })
    };
    let decide =
        |id: &str| json!({"chatId": "c1", "requestId": id, "decision": "deny", "note": "no"});
    let cards_changed = || AppEvent::new("consent").with("chatId", "c1");

    // A card already waiting on the chat is seen by every PC when it opens the chat.
    port.put_consent("c1", card("r1"));
    let sub = laptop
        .rpc
        .call("dev-a", "chat_sub", json!({"chatId": "c1", "since": 0}))
        .await
        .unwrap();
    assert_eq!(sub["consent"].as_array().unwrap().len(), 1);
    assert_eq!(sub["consent"][0]["id"], "r1");
    tablet
        .rpc
        .call("dev-a", "chat_sub", json!({"chatId": "c1", "since": 0}))
        .await
        .unwrap();

    // A new card reaches every PC holding the chat at once.
    port.put_consent("c1", card("r2"));
    host.observe(&cards_changed());
    until(|| laptop.heard("chat:c1").len() == 1 && tablet.heard("chat:c1").len() == 1).await;
    for w in [&laptop, &tablet] {
        let push = &w.heard("chat:c1")[0];
        assert_eq!(push.event["kind"], "consent");
        assert_eq!(push.event["pending"].as_array().unwrap().len(), 2);
    }

    // Answered from one PC: the store on the PC that runs the chat takes the decision and names
    // the device, and the card leaves the list everywhere.
    laptop
        .rpc
        .call("dev-a", "consent_decide", decide("r2"))
        .await
        .unwrap();
    assert_eq!(
        port.st().consents,
        [(
            "r2".to_owned(),
            "deny".to_owned(),
            "device:Laptop".to_owned()
        )]
    );
    // A second answer to the same card is refused: the first one won.
    let e = tablet
        .rpc
        .call("dev-a", "consent_decide", decide("r2"))
        .await
        .unwrap_err();
    assert_eq!(e.code, code::REFUSED);

    // Answered on the host PC itself: the window's own click moves the same store, and the
    // service's consent event closes the card on the other PCs.
    port.st()
        .consent
        .get_mut("c1")
        .unwrap()
        .retain(|c| c["id"] != "r1");
    host.observe(&cards_changed());
    until(|| laptop.heard("chat:c1").len() == 2 && tablet.heard("chat:c1").len() == 2).await;
    for w in [&laptop, &tablet] {
        assert!(w.heard("chat:c1")[1].event["pending"]
            .as_array()
            .unwrap()
            .is_empty());
    }

    // The only gate is the switch: with it off, a decision from another PC is refused; reads and
    // pushes keep working, and putting it back lets the answer through.
    port.put_consent("c1", card("r3"));
    host.observe(&cards_changed());
    until(|| laptop.heard("chat:c1").len() == 3 && tablet.heard("chat:c1").len() == 3).await;
    port.set_run(false);
    let e = tablet
        .rpc
        .call("dev-a", "consent_decide", decide("r3"))
        .await
        .unwrap_err();
    assert_eq!(e.code, code::NOT_ALLOWED);
    port.set_run(true);
    tablet
        .rpc
        .call("dev-a", "consent_decide", decide("r3"))
        .await
        .unwrap();
    assert_eq!(port.st().consents.len(), 2);
}

/// A device that only watches: its own database, its end of the hub, and the events it raises.
struct Side {
    mirror: Mirror,
    db: Arc<Database>,
    sink: Arc<RecordingSink>,
    link: Arc<MemLink>,
    _dir: tempfile::TempDir,
}

fn side(
    router: &MemRouter,
    uid: &str,
    name: &str,
    people: Option<Arc<PeopleSync>>,
    person: &str,
    person_name: &str,
    act_for: &[(&str, bool, bool)],
) -> Side {
    let dir = tempfile::tempdir().unwrap();
    let mut reg = MigrationRegistry::new();
    helm_devices::register_migrations(&mut reg);
    let db = Arc::new(Database::open(&dir.path().join("watcher.sqlite3"), &reg).unwrap());
    db.write(|tx| tx.execute("UPDATE device_self SET uid = ?1", [uid]))
        .unwrap();
    let link = router.connect(uid, Some(person), Some(person_name), true);
    if !act_for.is_empty() {
        link.set_people(act_for.iter().map(|(p, _, _)| (*p).to_owned()).collect());
    }
    let sink = Arc::new(RecordingSink::default());
    let mirror = Mirror::new(
        db.clone(),
        link.clone(),
        sink.clone(),
        tokio::runtime::Handle::current(),
        people,
    );
    link.send(Frame::Device(DeviceFrame {
        uid: uid.to_owned(),
        name: name.to_owned(),
        run: true,
        people: act_for
            .iter()
            .map(|(p, telegram, voice)| DevicePerson {
                person_id: (*p).to_owned(),
                telegram: *telegram,
                voice: *voice,
            })
            .collect(),
    }));
    Side {
        mirror,
        db,
        sink,
        link,
        _dir: dir,
    }
}

/// The rows the copy of a chat holds, newest page, as (id, text).
fn rows_of(s: &Side, chat: &str) -> Vec<(i64, String)> {
    let page =
        s.db.snapshot(|tx| mirror::local_page(tx, chat, None, None, None, false))
            .unwrap()
            .unwrap()
            .page;
    page["messages"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| {
            (
                r["id"].as_i64().unwrap(),
                r["text"].as_str().unwrap().to_owned(),
            )
        })
        .collect()
}

fn count(s: &Side, sql: &str) -> i64 {
    s.db.snapshot(|tx| tx.query_row(sql, [], |r| r.get::<_, i64>(0)))
        .unwrap()
}

fn device_row(s: &Side, uid: &str) -> Value {
    let v =
        s.db.snapshot(|tx| mirror::devices_view(tx, s.mirror.line_is_up()))
            .unwrap();
    v["devices"]
        .as_array()
        .unwrap()
        .iter()
        .find(|d| d["uid"] == uid)
        .cloned()
        .unwrap_or(Value::Null)
}

#[tokio::test(start_paused = true)]
async fn a_watcher_keeps_a_copy_of_the_chats_of_another_device() {
    let router = MemRouter::new();
    let port = Arc::new(FakePort::new("dev-a", "Desk"));
    port.add_chat("c1", "First", 10);
    port.put_row(fake_row("c1", 1, 5, "hello"));
    port.put_row(fake_row("c1", 2, 6, "world"));
    let link_a = router.connect("dev-a", Some("person-a"), Some("Alex"), true);
    let host = Service::new(
        port.clone(),
        link_a.clone(),
        tokio::runtime::Handle::current(),
    );
    host.announce();
    let people = Arc::new(Mutex::new(Vec::<String>::new()));
    let heard = people.clone();
    let hook = Arc::new(PeopleSync::new(move |_db, list| {
        heard
            .lock()
            .unwrap()
            .extend(list.iter().map(|p| p.name.clone()));
        true
    }));
    let b = side(
        &router,
        "dev-b",
        "Laptop",
        Some(hook),
        "person-a",
        "Alex",
        &[],
    );

    // The list arrives by itself once the hub shows the device up; the hub's people become
    // profiles, and the device reads as live.
    until(|| b.mirror.owner("c1").is_some()).await;
    until(|| {
        let p = people.lock().unwrap();
        p.contains(&"Alex".to_owned()) && p.contains(&"Person person-a".to_owned())
    })
    .await;
    let d = device_row(&b, "dev-a");
    assert_eq!(
        (d["name"].as_str(), d["state"].as_str()),
        (Some("Desk"), Some("live"))
    );
    let projects =
        b.db.snapshot(|tx| mirror::mirror_projects(tx, "dev-a"))
            .unwrap();
    assert_eq!(projects[0]["name"], "Work");
    let listed =
        b.db.snapshot(|tx| mirror::mirror_chat_list(tx, "dev-a", None, false))
            .unwrap();
    assert_eq!(
        (listed.len(), listed[0]["hostDeviceId"].as_str()),
        (1, Some("dev-a"))
    );
    assert_eq!(listed[0]["draft"], "");

    // The ops the window uses, through the dispatcher, with the check that routes them.
    let mut services = Services::new();
    services.insert(b.db.clone());
    let routes = Arc::new(RemoteSlot::default());
    routes.attach(Arc::new(b.mirror.clone()));
    services.insert(routes);
    services.insert(Arc::new(OpIntercept(Arc::new(Routes))));
    let slot = Arc::new(MirrorSlot::default());
    slot.attach(b.mirror.clone());
    services.insert(slot);
    let mut ops = OpRegistry::new();
    helm_chats::register_ops(&mut ops);
    helm_devices::register_ops(&mut ops);
    let api = Dispatcher::new(
        ops,
        OpCtx {
            services: Arc::new(services),
            events: b.sink.clone(),
        },
        false,
    );

    // Opening a chat starts listening to it: its rows arrive, and the window hears the events a
    // chat that runs here would raise.
    let got = api
        .call_host("chat_get", json!({"chatId": "c1"}))
        .await
        .unwrap();
    assert_eq!(
        (
            got["chat"]["id"].as_str(),
            got["chat"]["hostDeviceId"].as_str()
        ),
        (Some("c1"), Some("dev-a"))
    );
    until(|| rows_of(&b, "c1").len() == 2).await;
    let kinds = b.sink.kinds();
    for k in ["chat", "thread", "pending", "refresh"] {
        assert!(kinds.iter().any(|x| x == k), "no {k} event in {kinds:?}");
    }
    port.put_row(fake_row("c1", 2, 12, "world!"));
    port.put_row(fake_row("c1", 3, 13, "new"));
    host.observe(&message(12, 2, "world!"));
    host.observe(&message(13, 3, "new"));
    until(|| rows_of(&b, "c1").len() == 3).await;
    assert_eq!(rows_of(&b, "c1")[1], (2, "world!".to_owned()));
    assert!(b.sink.kinds().iter().any(|k| k == "message"));

    // Allow cards ride the copy: a changed card list is kept here, the window hears the same
    // consent event it would for a chat that runs here, and this PC can answer the card over
    // the wire.
    port.put_consent(
        "c1",
        json!({"id": "r1", "kind": "test-step", "state": "pending", "grantMinutes": 30}),
    );
    host.observe(&AppEvent::new("consent").with("chatId", "c1"));
    until(|| b.sink.kinds().iter().any(|k| k == "consent")).await;
    let cards = api
        .call_host("devices_consent_list", json!({"chatId": "c1"}))
        .await
        .unwrap();
    assert_eq!(cards["pending"].as_array().unwrap().len(), 1);
    api.call_host(
        "devices_consent_decide",
        json!({"chatId": "c1", "requestId": "r1", "decision": "allow", "grantMinutes": 15}),
    )
    .await
    .unwrap();
    assert_eq!(
        port.st().consents,
        [(
            "r1".to_owned(),
            "allow".to_owned(),
            "device:Laptop".to_owned()
        )]
    );
    // The host's answer closes the card on this PC too, on the same event.
    host.observe(&AppEvent::new("consent").with("chatId", "c1"));
    until(|| b.sink.kinds().iter().filter(|k| *k == "consent").count() >= 2).await;
    let cards = api
        .call_host("devices_consent_list", json!({"chatId": "c1"}))
        .await
        .unwrap();
    assert!(cards["pending"].as_array().unwrap().is_empty());

    // Every chat op on a copied chat: answered from the copy, sent to the device, answered
    // quietly, or refused with the sentence that says where to do it.
    let elsewhere = "This chat runs on Desk. Do that there.";
    let table: Vec<(&str, Value, Result<Value, &str>)> = vec![
        ("transcript_page", json!({"chatId": "c1"}), Ok(json!(null))),
        ("chat_warm", json!({"chatId": "c1"}), Ok(json!(null))),
        ("skills_list", json!({"chatId": "c1"}), Ok(json!([]))),
        (
            "title_suggest",
            json!({"chatId": "c1"}),
            Ok(json!({"title": "", "titles": []})),
        ),
        (
            "model_catalogue",
            json!({"harness": "claude", "deviceUid": "dev-a"}),
            Ok(json!({"models": []})),
        ),
        ("chat_fork", json!({"chatId": "c1"}), Err(elsewhere)),
        ("chat_delete", json!({"chatId": "c1"}), Err(elsewhere)),
        ("message_edit_open", json!({"chatId": "c1"}), Err(elsewhere)),
        (
            "interaction_answer",
            json!({"chatId": "c1", "requestId": "x", "confirmed": true}),
            Err("That request is no longer waiting."),
        ),
    ];
    for (op, args, want) in table {
        let got = api.call_host(op, args).await;
        match want {
            Ok(v) if v.is_null() => assert!(got.is_ok(), "{op}: {got:?}"),
            Ok(v) => assert_eq!(got.unwrap(), v, "{op}"),
            Err(m) => assert_eq!(got.unwrap_err().message, m, "{op}"),
        }
    }
    // A chat that is not a copy is left to its own handler.
    let own = api
        .call_host("chat_fork", json!({"chatId": "mine"}))
        .await
        .unwrap_err();
    assert!(!own.message.contains("runs on"), "{own:?}");

    // A send reaches the device once, as the person's own words.
    let id = "7f0f2f0e-6b0e-4d52-8d3a-1f0e2d3c4b5a";
    let send = json!({"chatId": "c1", "submissionId": id, "text": "hi", "action": "steer", "draftRevision": 0});
    let sent = api.call_host("chat_send", send.clone()).await.unwrap();
    assert_eq!(sent["submission"]["state"], "accepted");
    assert_eq!(
        port.st().sent,
        [("c1".to_owned(), id.to_owned(), "hi".to_owned())]
    );

    // A picture and a file ride with a message: staged on this PC, they cross in ordered
    // chunks and reach the chat on the PC that runs it just as a local attachment would.
    let stage = b.db.path().parent().unwrap().join("mirror-attachments");
    std::fs::create_dir_all(&stage).unwrap();
    let img: Vec<u8> = (0..650_000u32).map(|i| (i % 251) as u8).collect();
    std::fs::write(stage.join("att-img"), &img).unwrap();
    let note = b.db.path().with_file_name("big-note.txt");
    let note_bytes: Vec<u8> = (0..700_004u32).map(|i| b'a' + (i % 26) as u8).collect();
    std::fs::write(&note, &note_bytes).unwrap();
    b.db.write(|tx| {
        tx.execute(
            "INSERT INTO mirror_attachments(id, chat_id, kind, name, mime, target, bytes, created_at)
             VALUES ('att-img', 'c1', 'image', 'shot.png', 'image/png', 'att-img', ?1, 1)",
            [img.len() as i64],
        )?;
        tx.execute(
            "INSERT INTO mirror_attachments(id, chat_id, kind, name, mime, target, bytes, created_at)
             VALUES ('att-file', 'c1', 'file', 'big-note.txt', '', ?1, ?2, 2)",
            rusqlite::params![note.to_str().unwrap(), note_bytes.len() as i64],
        )?;
        Ok::<_, rusqlite::Error>(())
    })
    .unwrap();
    let with_files = api
        .call_host(
            "chat_send",
            json!({
                "chatId": "c1",
                "submissionId": "7f0f2f0e-6b0e-4d52-8d3a-1f0e2d3c4b5b",
                "text": "look",
                "action": "default",
                "draftRevision": 0,
                "attachments": ["att-img", "att-file"],
            }),
        )
        .await
        .unwrap();
    assert_eq!(with_files["submission"]["state"], "accepted");
    {
        let st = port.st();
        let files = st.sent_files.last().unwrap();
        assert_eq!(files.len(), 2);
        assert_eq!(
            (
                files[0].kind.as_str(),
                files[0].name.as_str(),
                files[0].mime.as_str()
            ),
            ("image", "shot.png", "image/png")
        );
        assert_eq!(files[0].bytes, img);
        assert_eq!(
            (files[1].kind.as_str(), files[1].name.as_str()),
            ("file", "big-note.txt")
        );
        assert_eq!(files[1].bytes, note_bytes);
    }
    // Spent by the send: the rows and the picture are gone from this PC.
    assert_eq!(count(&b, "SELECT COUNT(*) FROM mirror_attachments"), 0);
    assert!(!stage.join("att-img").exists());
    // An id that is no longer staged is refused in words, before any second copy moves.
    let e = api
        .call_host(
            "chat_send",
            json!({
                "chatId": "c1",
                "submissionId": "7f0f2f0e-6b0e-4d52-8d3a-1f0e2d3c4b5c",
                "text": "again",
                "action": "default",
                "draftRevision": 0,
                "attachments": ["att-img"],
            }),
        )
        .await
        .unwrap_err();
    assert_eq!(e.code, "refused");
    assert_eq!(port.st().sent.len(), 2);

    // Its switch off: read-only, said in words, nothing sent.
    port.set_run(false);
    host.announce();
    until(|| device_row(&b, "dev-a")["state"] == "readonly").await;
    let e = api.call_host("chat_send", send.clone()).await.unwrap_err();
    assert_eq!(e.code, "not_allowed");
    assert!(
        e.message
            .starts_with("Desk doesn't let other devices run its chats"),
        "{e:?}"
    );
    port.set_run(true);
    host.announce();
    until(|| device_row(&b, "dev-a")["state"] == "live").await;

    // The device goes offline: the copy still reads, a send is refused at once, a draft is kept
    // here and never sent.
    link_a.set_up(false);
    until(|| device_row(&b, "dev-a")["state"] == "offline").await;
    port.put_row(fake_row("c1", 4, 30, "while away"));
    let e = api.call_host("chat_send", send.clone()).await.unwrap_err();
    assert_eq!(e.code, "offline");
    assert!(
        e.message.starts_with("Desk is offline (last seen "),
        "{e:?}"
    );
    assert_eq!(port.st().sent.len(), 2);
    let page = api
        .call_host("transcript_page", json!({"chatId": "c1"}))
        .await
        .unwrap();
    assert_eq!(page["messages"].as_array().unwrap().len(), 3);
    api.call_host(
        "draft_save",
        json!({"chatId": "c1", "text": "note", "revision": 5}),
    )
    .await
    .unwrap();
    let again = api
        .call_host(
            "draft_save",
            json!({"chatId": "c1", "text": "old", "revision": 5}),
        )
        .await;
    assert_eq!(again.unwrap_err().code, "stale");
    let got = api
        .call_host("chat_get", json!({"chatId": "c1"}))
        .await
        .unwrap();
    assert_eq!(got["chat"]["draft"], "note");
    assert_eq!(port.st().sent.len(), 2);
    // This PC's own line down reads differently.
    b.mirror.line_down();
    b.link.set_up(false);
    assert_eq!(device_row(&b, "dev-a")["state"], "reconnecting");
    let e = api.call_host("chat_send", send.clone()).await.unwrap_err();
    assert_eq!(e.code, "reconnecting");
    b.link.set_up(true);
    b.mirror.line_up();
    b.link.send(Frame::Device(DeviceFrame {
        uid: "dev-b".into(),
        name: "Laptop".into(),
        run: true,
        people: Vec::new(),
    }));

    // The device returns: the copy catches up from the revision it holds, without repeats.
    link_a.set_up(true);
    until(|| rows_of(&b, "c1").iter().map(|r| r.0).collect::<Vec<_>>() == [1, 2, 3, 4]).await;
    assert_eq!(
        count(&b, "SELECT COUNT(*) FROM mirror_rows WHERE chat_id = 'c1'"),
        4
    );

    // `refresh` starts the chat over from the device; a skipped push is mended by asking again.
    let before = b.sink.kinds().iter().filter(|k| *k == "refresh").count();
    host.observe(&refresh());
    until(|| b.sink.kinds().iter().filter(|k| *k == "refresh").count() > before).await;
    until(|| rows_of(&b, "c1").len() == 4).await;
    port.put_row(fake_row("c1", 5, 40, "unpushed"));
    link_a.send(Frame::DevPush(Push {
        to: Some("dev-b".into()),
        from: None,
        sub: "chat:c1".into(),
        seq: 99,
        event: json!({"kind": "message", "chatId": "c1", "revision": 50, "rows": []}),
    }));
    until(|| rows_of(&b, "c1").len() == 5).await;

    // Only the newest 3000 rows are kept, and rows of a chat nobody opens for a month are let go;
    // the chat stays in the list.
    let many: Vec<_> = (100..3110)
        .map(|i| fake_row("c1", i, 1000 + i, "x"))
        .collect();
    host.observe(
        &ChatEvent::Message {
            chat_id: "c1".into(),
            revision: 5000,
            rows: many,
        }
        .to_app_event(),
    );
    until(|| count(&b, "SELECT COUNT(*) FROM mirror_rows WHERE chat_id = 'c1'") >= 3000).await;
    settle().await;
    assert_eq!(
        count(&b, "SELECT COUNT(*) FROM mirror_rows WHERE chat_id = 'c1'"),
        3000
    );
    assert_eq!(
        count(&b, "SELECT older FROM mirror_chats WHERE chat_id = 'c1'"),
        1
    );
    let month = 31 * 24 * 60 * 60 * 1000;
    b.db.write(|tx| mirror::prune_unopened(tx, helm_core::now_ms() + month))
        .unwrap();
    assert_eq!(
        count(&b, "SELECT COUNT(*) FROM mirror_rows WHERE chat_id = 'c1'"),
        0
    );
    assert!(b.mirror.owner("c1").is_some());
}

/// The hello on the production port, through the real op dispatcher: the catalogue op is
/// called the way the app calls it, and a failed catalogue leaves the hello standing without one.
#[tokio::test(start_paused = true)]
async fn hello_works_on_the_production_port_through_the_real_ops() {
    let dir = tempfile::tempdir().unwrap();
    let mut reg = MigrationRegistry::new();
    helm_devices::register_migrations(&mut reg);
    let db = Arc::new(Database::open(&dir.path().join("host.sqlite3"), &reg).unwrap());
    // The profiles table is the profiles crate's, read here directly: make just enough of it.
    db.write(|tx| {
        tx.execute_batch(
            "CREATE TABLE profiles (id TEXT PRIMARY KEY, hub_person_id TEXT, name TEXT, colour TEXT, about TEXT);
             INSERT INTO profiles VALUES ('p-a', 'person-a', 'Alex', 'red', '');
             INSERT INTO projects (id, kind, name, path, color, created_at)
             VALUES ('proj-1', 'folder', 'Work', '/work', 'blue', 0),
                    ('brd-1', 'board', 'Task board', NULL, '#ff0000', 0);
             -- chat_groups belongs to the projects crate; stand in just enough of it.
             CREATE TABLE chat_groups (chat_id TEXT PRIMARY KEY, project_id TEXT NOT NULL);
             INSERT INTO chat_groups VALUES ('boarded', 'brd-1');",
        )
    })
    .unwrap();

    // The full host: the real dispatcher with the real chat ops, and the profile the
    // catalogue op reads.
    let mut services = Services::new();
    services.insert(db.clone());
    services.insert(Arc::new(
        helm_core::profile::Profile::open(&dir.path().join("prof")).unwrap(),
    ));
    let mut ops = OpRegistry::new();
    helm_chats::register_ops(&mut ops);
    let api = Dispatcher::new(
        ops,
        OpCtx {
            services: Arc::new(services),
            events: Arc::new(RecordingSink::default()),
        },
        false,
    );
    let port = HostPort::new(Arc::new(api));
    let hello = port.hello().await.unwrap();
    assert!(!hello.device.uid.is_empty());
    assert_eq!(hello.people[0].person_id, "person-a");
    let folder = hello
        .projects
        .iter()
        .find(|p| p.kind == "folder")
        .expect("the hello carries folders");
    assert_eq!(
        (folder.name.as_str(), folder.path.as_str()),
        ("Work", "/work")
    );
    // Boards travel in the hello too, with an empty path and their own colour.
    let board = hello
        .projects
        .iter()
        .find(|p| p.kind == "board")
        .expect("the hello carries boards");
    assert_eq!(
        (board.id.as_str(), board.name.as_str()),
        ("brd-1", "Task board")
    );
    assert_eq!(
        (board.colour.as_str(), board.path.as_str()),
        ("#ff0000", "")
    );
    assert_eq!(hello.catalogue.expect("a catalogue")["harness"], "claude");

    // A chat shown under a board travels with that board; a chat with no board keeps its
    // folder alone.
    let shown = port
        .remote_chat(fake_record("boarded", "Boarded", 3))
        .to_value();
    assert_eq!(shown["boardId"], "brd-1");
    assert_eq!(shown["boardName"], "Task board");
    assert_eq!(shown["boardColour"], "#ff0000");
    assert_eq!(shown["projectName"], "Work");
    let plain = port
        .remote_chat(fake_record("c-plain", "Plain", 3))
        .to_value();
    assert_eq!(plain["projectName"], "Work");
    assert!(plain.get("boardId").is_none(), "{plain}");

    // The catalogue op failing (here: no profile folder for it to read) is logged, not fatal:
    // the hello still answers, with no catalogue.
    let mut bare = Services::new();
    bare.insert(db);
    let mut ops = OpRegistry::new();
    helm_chats::register_ops(&mut ops);
    let api = Dispatcher::new(
        ops,
        OpCtx {
            services: Arc::new(bare),
            events: Arc::new(RecordingSink::default()),
        },
        false,
    );
    let hello = HostPort::new(Arc::new(api)).hello().await.unwrap();
    assert_eq!(hello.people[0].person_id, "person-a");
    assert_eq!(hello.catalogue, None);
}

#[tokio::test(start_paused = true)]
async fn a_household_member_gets_their_own_view_of_the_list_but_is_refused_nothing() {
    let router = MemRouter::new();
    let port = Arc::new(FakePort::new("dev-a", "Desk"));
    port.st().people.push("person-b".to_owned());
    port.add_chat("mine", "The owner's", 10);
    port.add_chat_of("hers", "Sam's", 11, "person-b", &[]);
    port.add_chat_of("shared", "Shared with Sam", 12, "person-a", &["person-b"]);
    let link_a = router.connect("dev-a", Some("person-a"), Some("Alex"), true);
    let host = Service::new(port.clone(), link_a, tokio::runtime::Handle::current());
    host.announce();
    let sam = Watcher::of(&router, "dev-s", "Sam's laptop", true, "person-b", "Sam");
    let own = Watcher::new(&router, "dev-o", "Alex's laptop", true);
    settle().await;

    // The list holds only Sam's chat and the one shared with her; the owner's device sees all.
    let ids = |v: Value| -> Vec<String> {
        v["chats"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| c["id"].as_str().unwrap().to_owned())
            .collect()
    };
    let list = sam
        .rpc
        .call("dev-a", "chats_sub", json!({"since": 0}))
        .await;
    assert_eq!(ids(list.unwrap()), ["hers", "shared"]);
    let list = own
        .rpc
        .call("dev-a", "chats_sub", json!({"since": 0}))
        .await;
    assert_eq!(ids(list.unwrap()), ["mine", "hers", "shared"]);
    // A change to the owner's chat never reaches her; a chat no longer shared is taken out.
    host.observe(
        &ChatEvent::Chat {
            chat: fake_record("mine", "Renamed", 13),
        }
        .to_app_event(),
    );
    // (The fake reads every changed record as the owner's own, shared with no one.)
    host.observe(
        &ChatEvent::Chat {
            chat: fake_record("shared", "No longer shared", 14),
        }
        .to_app_event(),
    );
    until(|| own.heard("chats").len() == 2).await;
    settle().await;
    let heard: Vec<(String, String)> = sam
        .heard("chats")
        .iter()
        .map(|p| {
            (
                p.event["kind"].as_str().unwrap().to_owned(),
                p.event["chatId"].as_str().unwrap_or_default().to_owned(),
            )
        })
        .collect();
    assert_eq!(heard, [("chat_removed".to_owned(), "shared".to_owned())]);
    port.st().chats.retain(|c| c.id() != "shared");
    port.add_chat_of("shared", "Shared again", 15, "person-a", &["person-b"]);

    // What she may do, chat by chat, in one table.
    port.put_interaction(fake_interaction(
        "hers",
        "q1",
        Meth::Select,
        Some(Att::Question),
        Some(&["A", "B"]),
    ));
    port.put_interaction(fake_interaction(
        "shared",
        "q2",
        Meth::Select,
        Some(Att::Question),
        Some(&["A", "B"]),
    ));
    let send = |chat: &str, n: u8| {
        json!({"chatId": chat, "clientId": format!("7f0f2f0e-6b0e-4d52-8d3a-1f0e2d3c4b{n:02x}"),
               "text": "hi", "mode": "steer"})
    };
    // Nothing is refused for being someone else's chat: the person marks who acts, not what
    // they may touch.
    let cases: Vec<(&str, Value, Option<&str>)> = vec![
        ("chat_sub", json!({"chatId": "hers", "since": 0}), None),
        ("chat_sub", json!({"chatId": "shared", "since": 0}), None),
        ("chat_sub", json!({"chatId": "mine", "since": 0}), None),
        ("page", json!({"chatId": "mine"}), None),
        ("send", send("hers", 1), None),
        ("send", send("shared", 2), None),
        ("send", send("mine", 3), None),
        ("stop", json!({"chatId": "mine"}), None),
        (
            "answer",
            json!({"chatId": "hers", "requestId": "q1", "value": "A", "label": "A"}),
            None,
        ),
        (
            "answer",
            json!({"chatId": "shared", "requestId": "q2", "value": "A", "label": "A"}),
            None,
        ),
        ("consent_decide", json!({"id": "x"}), Some(code::BAD_ARGS)),
        (
            "create",
            json!({"personId": "person-a", "projectId": "proj-1"}),
            None,
        ),
    ];
    for (op, args, want) in cases {
        let got = sam.rpc.call("dev-a", op, args.clone()).await;
        match want {
            None => assert!(got.is_ok(), "{op} {args}: {got:?}"),
            Some(c) => assert_eq!(got.unwrap_err().code, c, "{op} {args}"),
        }
    }
    {
        let st = port.st();
        // Her words on her own chat are hers; on anyone's chat they arrive marked as hers.
        assert_eq!(
            st.sent_from,
            [None, Some("Sam".to_owned()), Some("Sam".to_owned())]
        );
        assert_eq!(
            st.answered
                .iter()
                .map(|a| a.request_id.as_str())
                .collect::<Vec<_>>(),
            ["q1", "q2"]
        );
        // A chat she starts is hers, whoever she names.
        assert_eq!(st.created[0].person_id, "person-b");
    }

    // With the household switch off she is refused, even reading; the owner's devices are not.
    port.set_household(false);
    let e = sam.rpc.call("dev-a", "hello", json!({})).await.unwrap_err();
    assert_eq!(e.code, code::NOT_ALLOWED);
    assert!(own.rpc.call("dev-a", "hello", json!({})).await.is_ok());
    port.set_household(true);
    // A person this PC has no profile for is still served: the switch is per household, not
    // per person.
    port.st().people.retain(|p| p != "person-b");
    assert!(sam.rpc.call("dev-a", "hello", json!({})).await.is_ok());

    // A PC paired with no person answers no one.
    let lone = Arc::new(FakePort::new("dev-l", "Lone"));
    let link_l = router.connect("dev-l", None, None, true);
    let lone_host = Service::new(lone, link_l, tokio::runtime::Handle::current());
    lone_host.announce();
    settle().await;
    for w in [&sam, &own] {
        let e = w.rpc.call("dev-l", "hello", json!({})).await.unwrap_err();
        assert_eq!(e.code, code::NOT_ALLOWED);
    }
}

/// One PC, both of its people: the copy on the other side holds every chat of the PC, not
/// just the watcher's own person's; each chat op is run by the person the window is showing;
/// a chat started from afar belongs to the acting person; words on someone else's chat are
/// framed; and a PC that never announced a person cannot act for them.
#[tokio::test(start_paused = true)]
async fn one_pc_acts_as_each_of_its_profiles() {
    let router = MemRouter::new();
    let port = Arc::new(FakePort::new("dev-d", "Desk"));
    port.st().people = vec!["person-p".to_owned(), "person-s".to_owned()];
    port.announce_people(&[("person-p", true, true), ("person-s", false, false)]);
    port.add_chat_of("own-p", "Pat's chat", 10, "person-p", &[]);
    port.add_chat_of("own-s", "Sam's chat on the desk", 11, "person-s", &[]);
    port.put_interaction(fake_interaction(
        "own-s",
        "q1",
        Meth::Select,
        Some(Att::Question),
        Some(&["A", "B"]),
    ));
    let link_d = router.connect("dev-d", Some("person-p"), Some("Pat"), true);
    let host = Service::new(port.clone(), link_d, tokio::runtime::Handle::current());
    host.announce();

    // The laptop's home person is Sam, and it also acts for Pat; it announced both.
    let b = side(
        &router,
        "dev-l",
        "Laptop",
        None,
        "person-s",
        "Sam",
        &[("person-s", true, true), ("person-p", false, false)],
    );
    b.db.write(|tx| {
        tx.execute_batch(
            "CREATE TABLE profiles (id TEXT PRIMARY KEY, hub_person_id TEXT, name TEXT,
                        colour TEXT, about TEXT, is_home INTEGER NOT NULL DEFAULT 0);
             INSERT INTO profiles VALUES ('p-s', 'person-s', 'Sam', 'red', '', 1);
             INSERT INTO profiles VALUES ('p-p', 'person-p', 'Pat', 'blue', '', 0);
             CREATE TABLE IF NOT EXISTS preferences (key TEXT PRIMARY KEY, value TEXT NOT NULL);",
        )
    })
    .unwrap();
    let rpc = Rpc::new(b.link.clone());
    let sorted = |v: &[Value]| -> Vec<String> {
        let mut ids: Vec<String> = v
            .iter()
            .map(|c| c["id"].as_str().unwrap_or_default().to_owned())
            .collect();
        ids.sort();
        ids
    };

    // The hello names the people the desk acts for, and the device list repeats them.
    let hello = rpc.call("dev-d", "hello", json!({})).await.unwrap();
    assert_eq!(
        hello["people"]
            .as_array()
            .unwrap()
            .iter()
            .map(|p| p["personId"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["person-p", "person-s"]
    );
    let d = device_row(&b, "dev-d");
    assert_eq!(
        d["people"]
            .as_array()
            .unwrap()
            .iter()
            .map(|p| p["personId"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["person-p", "person-s"]
    );

    // The copy holds every chat of the desk: Pat's chat shares nothing with Sam, yet it
    // arrives, because the read acts for everyone this PC covers — and that set includes
    // the desk's own person, so the list comes unfiltered.
    until(|| b.mirror.owner("own-p").is_some() && b.mirror.owner("own-s").is_some()).await;
    let for_s =
        b.db.snapshot(|tx| mirror::mirror_chat_list(tx, "dev-d", Some("p-s"), false))
            .unwrap();
    let for_p =
        b.db.snapshot(|tx| mirror::mirror_chat_list(tx, "dev-d", Some("p-p"), false))
            .unwrap();
    let every =
        b.db.snapshot(|tx| mirror::mirror_chat_list(tx, "dev-d", None, false))
            .unwrap();
    assert_eq!(sorted(&for_s), ["own-s"]);
    assert_eq!(sorted(&for_p), ["own-p"]);
    assert_eq!(sorted(&every), ["own-p", "own-s"]);
    // A view widened on purpose shows what every profile's own view chooses to hide; the
    // copy held it all along.
    let wide = ChatView {
        who: WhoFilter::Any,
        device: Some("dev-d".to_owned()),
    };
    let wide_list =
        b.db.snapshot(|tx| mirror::chats_for_view(tx, &wide, false))
            .unwrap();
    assert_eq!(sorted(&wide_list), ["own-p", "own-s"]);

    // The ops the window uses, through the dispatcher, with the check that routes them.
    let mut services = Services::new();
    services.insert(b.db.clone());
    let routes = Arc::new(RemoteSlot::default());
    routes.attach(Arc::new(b.mirror.clone()));
    services.insert(routes);
    services.insert(Arc::new(OpIntercept(Arc::new(Routes))));
    let slot = Arc::new(MirrorSlot::default());
    slot.attach(b.mirror.clone());
    services.insert(slot);
    let mut ops = OpRegistry::new();
    helm_chats::register_ops(&mut ops);
    helm_devices::register_ops(&mut ops);
    let api = Dispatcher::new(
        ops,
        OpCtx {
            services: Arc::new(services),
            events: b.sink.clone(),
        },
        false,
    );

    // As Pat: a chat started from afar belongs to Pat, whoever the request named; and
    // Pat's own words on Pat's chat arrive unframed.
    b.db.write(|tx| helm_core::prefs::set_raw(tx, "activeProfile", &json!("p-p")))
        .unwrap();
    let got = rpc
        .call_as(
            "dev-d",
            "create",
            json!({"personId": "person-s", "projectId": "proj-1"}),
            &["person-p".to_owned()],
        )
        .await
        .unwrap();
    assert_eq!(got["chatId"], "new-1");
    assert_eq!(port.st().created[0].person_id, "person-p");
    let send = |id: &str| {
        json!({"chatId": "own-p", "submissionId": id, "text": "hi",
               "action": "steer", "draftRevision": 0})
    };
    api.call_host("chat_send", send("7f0f2f0e-6b0e-4d52-8d3a-1f0e2d3c4b01"))
        .await
        .unwrap();
    // Switch the window to Sam: the same chat, now acted for by Sam — the same words
    // arrive marked as Sam's, never as Pat's.
    b.db.write(|tx| helm_core::prefs::set_raw(tx, "activeProfile", &json!("p-s")))
        .unwrap();
    api.call_host("chat_send", send("7f0f2f0e-6b0e-4d52-8d3a-1f0e2d3c4b02"))
        .await
        .unwrap();
    assert_eq!(port.st().sent_from, [None, Some("Sam".to_owned())]);
    // Back to Pat: Sam's waiting request, answered from the laptop as Pat.
    b.db.write(|tx| helm_core::prefs::set_raw(tx, "activeProfile", &json!("p-p")))
        .unwrap();
    api.call_host("chat_get", json!({"chatId": "own-s"}))
        .await
        .unwrap();
    until(|| {
        b.db.snapshot(|tx| {
            Ok::<_, rusqlite::Error>(
                tx.query_row(
                    "SELECT thread FROM mirror_chats WHERE chat_id = 'own-s'",
                    [],
                    |r| r.get::<_, Option<String>>(0),
                )
                .optional()
                .ok()
                .flatten()
                .flatten()
                .is_some_and(|t| t.contains("\"q1\"")),
            )
        })
        .unwrap()
    })
    .await;
    api.call_host(
        "interaction_answer",
        json!({"chatId": "own-s", "requestId": "q1", "value": "A"}),
    )
    .await
    .unwrap();
    assert_eq!(
        port.st()
            .answered
            .iter()
            .map(|a| a.request_id.as_str())
            .collect::<Vec<_>>(),
        ["q1"]
    );

    // A PC that never announced Pat cannot act for them: the hub turns the call away and
    // nothing reaches the desk.
    let x = Watcher::of(&router, "pc-x", "Gale", true, "person-x", "Gale");
    let e = x
        .rpc
        .call_as("dev-d", "hello", json!({}), &["person-p".to_owned()])
        .await
        .unwrap_err();
    assert_eq!(e.code, code::NOT_ALLOWED);
    assert_eq!(port.st().created.len(), 1);
}
