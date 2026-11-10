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
use helm_devices::link::{DeviceLink, MemLink, MemRouter, Rpc};
use helm_devices::mirror::{self, Mirror, PeopleSync};
use helm_devices::testing::{fake_interaction, fake_record, fake_row, FakePort};
use helm_devices::wire::{code, DeviceFrame, Frame, Push};
use helm_devices::Service;
use helm_ipc::{Dispatcher, OpCtx, OpIntercept, OpRegistry, RecordingSink, Services};
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
            code::REFUSED,
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
        (
            &b,
            "dev-a",
            "create",
            json!({"personId": "nobody", "projectId": "proj-1"}),
            code::UNKNOWN_PERSON,
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
    b.rpc
        .call("dev-a", "stop", json!({"chatId": "c1"}))
        .await
        .unwrap();
    assert_eq!(port.st().stopped, ["c1".to_owned()]);

    // Answers: a question, a plan and a tool permission may be answered from afar, with the
    // words the request shows; a trust prompt, a secret, a doctored label or a second answer may not.
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
            Some(code::REFUSED),
        ),
        (
            Some(secret),
            "secret",
            json!({"value": "x", "label": "Token"}),
            Some(code::REFUSED),
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
    assert_eq!(answered, ["perm", "plan", "pick"]);

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

/// A device that only watches: its own database, its end of the hub, and the events it raises.
struct Side {
    mirror: Mirror,
    db: Arc<Database>,
    sink: Arc<RecordingSink>,
    link: Arc<MemLink>,
    _dir: tempfile::TempDir,
}

fn side(router: &MemRouter, uid: &str, name: &str, people: Option<Arc<PeopleSync>>) -> Side {
    let dir = tempfile::tempdir().unwrap();
    let mut reg = MigrationRegistry::new();
    helm_devices::register_migrations(&mut reg);
    let db = Arc::new(Database::open(&dir.path().join("watcher.sqlite3"), &reg).unwrap());
    db.write(|tx| tx.execute("UPDATE device_self SET uid = ?1", [uid]))
        .unwrap();
    let link = router.connect(uid, Some("person-a"), Some("Alex"), true);
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
    let b = side(&router, "dev-b", "Laptop", Some(hook));

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
    let routes = Arc::new(RemoteSlot::default());
    routes.attach(Arc::new(b.mirror.clone()));
    services.insert(routes);
    services.insert(Arc::new(OpIntercept(Arc::new(Routes))));
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
    assert_eq!(port.st().sent.len(), 1);
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
    assert_eq!(port.st().sent.len(), 1);
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

#[tokio::test(start_paused = true)]
async fn a_household_member_sees_and_runs_only_their_own_and_shared_chats() {
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
    let cases: Vec<(&str, Value, Option<&str>)> = vec![
        ("chat_sub", json!({"chatId": "hers", "since": 0}), None),
        ("chat_sub", json!({"chatId": "shared", "since": 0}), None),
        (
            "chat_sub",
            json!({"chatId": "mine", "since": 0}),
            Some(code::NOT_FOUND),
        ),
        ("page", json!({"chatId": "mine"}), Some(code::NOT_FOUND)),
        ("send", send("hers", 1), None),
        ("send", send("shared", 2), None),
        ("send", send("mine", 3), Some(code::NOT_FOUND)),
        ("stop", json!({"chatId": "mine"}), Some(code::NOT_FOUND)),
        (
            "answer",
            json!({"chatId": "hers", "requestId": "q1", "value": "A", "label": "A"}),
            None,
        ),
        (
            "answer",
            json!({"chatId": "shared", "requestId": "q2", "value": "A", "label": "A"}),
            Some(code::REFUSED),
        ),
        ("consent_decide", json!({"id": "x"}), Some(code::REFUSED)),
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
        // Her words on her own chat are hers; on a shared chat they arrive marked as hers.
        assert_eq!(st.sent_from, [None, Some("Sam".to_owned())]);
        assert_eq!(
            st.answered
                .iter()
                .map(|a| a.request_id.as_str())
                .collect::<Vec<_>>(),
            ["q1"]
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
    // Someone with no profile on this PC is refused.
    port.st().people.retain(|p| p != "person-b");
    let e = sam.rpc.call("dev-a", "hello", json!({})).await.unwrap_err();
    assert_eq!(e.code, code::NOT_ALLOWED);

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
