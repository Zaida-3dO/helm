//! Two devices talking through an in-memory hub: the host side of the device wire, end to end.
//!
//! One test walks the whole flow so the setup is paid for once: what a watcher may ask, what it
//! hears back, and every refusal. Time is paused, so the 250 ms batching and the hub's 30 second
//! timeout cost nothing. No network, no database.

use helm_chats::types::{ChatEvent, Interaction, InteractionAttention as Att, InteractionMethod as Meth};
use helm_devices::link::{DeviceLink, MemLink, MemRouter, Rpc};
use helm_devices::testing::{fake_interaction, fake_record, fake_row, FakePort};
use helm_devices::wire::{code, DeviceFrame, Frame, Push};
use helm_devices::Service;
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
    fn new(router: &MemRouter, uid: &str, name: &str, peer: bool) -> Watcher {
        let link = router.connect(uid, Some("person-b"), Some("Sam"), peer);
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
        (&b, "dev-a", "consent_decide", json!({"id": "x"}), code::REFUSED),
        (&b, "dev-a", "chat_sub", json!({"chatId": "c1", "surprise": 1}), code::BAD_ARGS),
        (&b, "dev-a", "chat_sub", json!({"chatId": "nope"}), code::NOT_FOUND),
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
        (&b, "dev-a", "rename", json!({"chatId": "c1", "title": big}), code::TOO_LARGE),
        (&b, "dev-a", "create", json!({"personId": "nobody", "projectId": "proj-1"}), code::UNKNOWN_PERSON),
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
    assert_eq!((c1["id"].as_str(), c1["personId"].as_str()), (Some("c1"), Some("person-a")));
    assert_eq!(c1["draft"], "");
    assert!(c1.get("profileId").is_none());
    assert_eq!(list["cursor"], 100);
    for (title, rev) in [("Renamed", 11), ("Renamed again", 12)] {
        host.observe(&ChatEvent::Chat { chat: fake_record("c1", title, rev) }.to_app_event());
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
    assert_eq!((sub["rows"].as_array().unwrap().len(), &sub["more"]), (1, &json!(false)));
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
        rows.iter().map(|r| (r["id"].as_i64().unwrap(), r["text"].as_str().unwrap())).collect::<Vec<_>>(),
        [(2, "world!!"), (3, "new")]
    );
    host.observe(&ChatEvent::Pending { chat_id: "c1".into(), revision: 14, pending: vec![] }.to_app_event());
    host.observe(&refresh());
    until(|| b.heard("chat:c1").len() == 3).await;
    let kinds: Vec<_> = b.heard("chat:c1").iter().map(|p| (p.seq, p.event["kind"].as_str().unwrap().to_owned())).collect();
    assert_eq!(kinds, [(1, "message".to_owned()), (2, "pending".to_owned()), (3, "refresh".to_owned())]);
    // A gap is mended by subscribing again from the revision held: the count starts over.
    b.rpc.call("dev-a", "chat_sub", json!({"chatId": "c1", "since": 14})).await.unwrap();
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
    assert_eq!(b.rpc.call("dev-a", "send", send.clone()).await.unwrap(), json!({"accepted": true}));
    assert_eq!(
        b.rpc.call("dev-a", "send", send).await.unwrap(),
        json!({"accepted": true, "duplicate": true})
    );
    assert_eq!(port.st().sent, [("c1".to_owned(), id.to_owned(), "hi".to_owned())]);
    let lost = "0b1c2d3e-4f50-4a6b-8c7d-9e0f1a2b3c4d";
    let e = b
        .rpc
        .call("dev-a", "send", json!({"chatId": "nope", "clientId": lost, "text": "hi", "mode": "steer"}))
        .await
        .unwrap_err();
    assert_eq!(e.code, code::NOT_FOUND);
    assert!(!port.st().claimed.contains(lost));

    // With the switch off, only reading works.
    port.set_run(false);
    for (op, args) in [
        ("send", json!({"chatId": "c1", "clientId": lost, "text": "hi", "mode": "steer"})),
        ("stop", json!({"chatId": "c1"})),
        ("rename", json!({"chatId": "c1", "title": "x"})),
        ("create", json!({"personId": "person-a", "projectId": "proj-1"})),
        ("archive", json!({"chatId": "c1", "archived": true})),
    ] {
        let e = b.rpc.call("dev-a", op, args).await.unwrap_err();
        assert_eq!(e.code, code::NOT_ALLOWED, "{op}");
    }
    assert!(b.rpc.call("dev-a", "hello", json!({})).await.is_ok());
    port.set_run(true);
    assert_eq!(
        b.rpc.call("dev-a", "create", json!({"personId": "person-a", "projectId": "proj-1"})).await.unwrap(),
        json!({"chatId": "new-1"})
    );
    b.rpc.call("dev-a", "stop", json!({"chatId": "c1"})).await.unwrap();
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
        (Some(fake_interaction("c1", "perm", Meth::Confirm, Some(Att::Approval), None)), "perm",
         json!({"confirmed": true, "label": "Allow"}), None),
        (Some(fake_interaction("c1", "perm2", Meth::Confirm, Some(Att::Approval), None)), "perm2",
         json!({"confirmed": true, "label": "Deny"}), Some(code::REFUSED)),
        (Some(fake_interaction("c1", "plan", Meth::Select, Some(Att::Plan), Some(&plan))), "plan",
         json!({"value": "Approve plan", "confirmed": true, "label": "Approve plan"}), None),
        (Some(fake_interaction("c1", "plan2", Meth::Select, Some(Att::Plan), Some(&plan))), "plan2",
         json!({"value": "Keep planning", "confirmed": true, "label": "Keep planning"}), Some(code::REFUSED)),
        (Some(fake_interaction("c1", "trust", Meth::Confirm, Some(Att::Trust), None)), "trust",
         json!({"confirmed": true, "label": "Allow"}), Some(code::REFUSED)),
        (Some(secret), "secret", json!({"value": "x", "label": "Token"}), Some(code::REFUSED)),
        (Some(fake_interaction("c1", "pick", Meth::Select, Some(Att::Question), Some(&pick))), "pick",
         json!({"value": "B", "label": "B"}), None),
        (Some(fake_interaction("c1", "pick2", Meth::Select, Some(Att::Question), Some(&pick))), "pick2",
         json!({"value": "C", "label": "C"}), Some(code::REFUSED)),
        (None, "gone", json!({"confirmed": true, "label": "Allow"}), Some(code::REFUSED)),
        (None, "perm", json!({"confirmed": true, "label": "Allow"}), Some(code::REFUSED)),
    ];
    for (waiting, request, extra, want) in answers {
        if let Some(i) = waiting {
            port.put_interaction(i);
        }
        let mut args = json!({"chatId": "c1", "requestId": request});
        args.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
        let got = b.rpc.call("dev-a", "answer", args).await;
        match want {
            None => assert!(got.is_ok(), "{request}: {got:?}"),
            Some(c) => assert_eq!(got.unwrap_err().code, c, "{request}"),
        }
    }
    let answered: Vec<_> = port.st().answered.iter().map(|a| a.request_id.clone()).collect();
    assert_eq!(answered, ["perm", "plan", "pick"]);

    // A device that drops off the hub loses its subscriptions, and asks again when it is back.
    assert!(host.watchers().contains(&"dev-b".to_owned()));
    b.link.set_up(false);
    settle().await;
    assert!(!host.watchers().contains(&"dev-b".to_owned()));
    assert_eq!(b.rpc.call("dev-a", "hello", json!({})).await.unwrap_err().code, code::OFFLINE);
    b.link.set_up(true);
    b.announce("dev-b");
    settle().await;
    b.rpc.call("dev-a", "chats_sub", json!({"since": 100})).await.unwrap();
    assert!(host.watchers().contains(&"dev-b".to_owned()));
}
