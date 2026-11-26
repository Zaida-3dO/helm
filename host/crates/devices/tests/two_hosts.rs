//! Two devices talking through an in-memory hub: the host side of the device wire, end to end.
//!
//! One test walks the whole flow so the setup is paid for once: what a watcher may ask, what it
//! hears back, and every refusal. Time is paused, so the 250 ms batching and the hub's 30 second
//! timeout cost nothing. No network, no database.

use helm_chats::remote::{RemoteSlot, Routes};
use helm_chats::types::{
    ChatEvent, Interaction, InteractionAttention as Att, InteractionMethod as Meth,
};
use helm_core::credentials::CredentialStore;
use helm_core::db::{Database, MigrationRegistry};
use helm_devices::host::HostPort;
use helm_devices::link::{DeviceLink, MemLink, MemRouter, Rpc};
use helm_devices::mirror::{self, ChatView, Mirror, MirrorSlot, PeopleSync, WhoFilter};
use helm_devices::testing::{fake_interaction, fake_record, fake_row, FakePort};
use helm_devices::wire::{code, DeviceFrame, DevicePerson, Frame, Push};
use helm_devices::{ChatPort, Service};
use helm_ipc::{
    ApiError, AppEvent, Dispatcher, OpClass, OpCtx, OpIntercept, OpRegistry, RecordingSink,
    Services,
};
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
    // The hello also names this device's profiles, so another window can offer "Move to profile".
    assert_eq!(hello["profiles"][0]["profileId"], "profile-local");
    assert_eq!(hello["profiles"][0]["personId"], "person-a");

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
                chat: Box::new(fake_record("c1", title, rev)),
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
    // The chat is told the PC the words were typed on, as the hub stamped the sender, and the
    // host remembers it for the chat until the person types on the host itself.
    assert_eq!(port.st().sent_on[0].name, "Laptop");
    let typed = host.typed_on("c1").expect("the typing PC is known");
    assert_eq!(
        (typed.device_uid.as_str(), typed.name.as_str()),
        ("dev-b", "Laptop")
    );
    assert_eq!(host.typed_on("other"), None);
    let mut older = fake_row("c1", 40, 40, "typed here before");
    older.role = helm_chats::types::MessageRole::User;
    older.timestamp = typed.at - 1;
    let mut newer = older.clone();
    newer.id = 41;
    newer.timestamp = typed.at + 1;
    // A message from Telegram is not the person at this PC; an older row typed here is not now.
    let mut phone = newer.clone();
    phone.details.origin = Some(helm_chats::types::SubmissionOrigin::Telegram);
    for rows in [vec![older], vec![phone]] {
        host.observe(
            &ChatEvent::Message {
                chat_id: "c1".into(),
                revision: 22,
                rows,
            }
            .to_app_event(),
        );
    }
    assert!(host.typed_on("c1").is_some());
    host.observe(
        &ChatEvent::Message {
            chat_id: "c1".into(),
            revision: 23,
            rows: vec![newer],
        }
        .to_app_event(),
    );
    assert_eq!(host.typed_on("c1"), None);
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
        ("project_create", json!({"title": "Ideas"})),
        (
            "project_set",
            json!({"projectId": "proj-1", "pinned": true}),
        ),
        (
            "option_board_answer",
            json!({"chatId": "c1", "boardId": "b1", "labels": ["A"]}),
        ),
        (
            "blob_read",
            json!({"chatId": "c1", "ref": {"file": "/work/shot.png"}}),
        ),
        ("blob_read", json!({"chatId": "c1", "ref": {"output": 7}})),
    ] {
        let e = b.rpc.call("dev-a", op, args).await.unwrap_err();
        assert_eq!(e.code, code::NOT_ALLOWED, "{op}");
    }
    assert!(b.rpc.call("dev-a", "hello", json!({})).await.is_ok());
    // What an agent showed in the chat is part of the chat: it reads with the switch off.
    for (op, args) in [
        ("shown_gallery", json!({"chatId": "c1", "galleryId": "g1"})),
        ("option_board", json!({"chatId": "c1", "boardId": "b1"})),
        (
            "blob_read",
            json!({"chatId": "c1", "ref": {"gallery": "g1", "index": 0}}),
        ),
    ] {
        assert!(b.rpc.call("dev-a", op, args).await.is_ok(), "{op}");
    }
    port.set_run(true);
    let made = b
        .rpc
        .call(
            "dev-a",
            "create",
            json!({"personId": "person-a", "projectId": "proj-1"}),
        )
        .await
        .unwrap();
    assert_eq!(made["chatId"], "new-1", "{made}");
    // The answer carries the device's record of the chat, so the asking PC can hold it at once.
    assert_eq!(made["chat"]["id"], "new-1");
    assert_eq!(made["chat"]["personId"], "person-a");
    // A chat made from afar belongs to the acting person: the hub's verified person replaces
    // whatever name the request carried.
    let made2 = b
        .rpc
        .call(
            "dev-a",
            "create",
            json!({"personId": "nobody", "projectId": "proj-1"}),
        )
        .await
        .unwrap();
    assert_eq!(made2["chatId"], "new-2", "{made2}");
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
    // Stop without resending: what the agent had not read stays unsent there too.
    b.rpc
        .call("dev-a", "stop", json!({"chatId": "c1", "noResend": true}))
        .await
        .unwrap();
    assert_eq!(port.st().stopped_no_resend, ["c1".to_owned()]);

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
        // A question of several steps goes back a step from afar; a step it does not have is
        // refused.
        (
            Some(Interaction {
                step: Some(1),
                steps: Some(3),
                ..fake_interaction(
                    "c1",
                    "steps",
                    Meth::Select,
                    Some(Att::Question),
                    Some(&pick),
                )
            }),
            "steps",
            json!({"goto": 0, "value": "half typed", "label": "Go back a step"}),
            None,
        ),
        (
            Some(Interaction {
                step: Some(1),
                steps: Some(3),
                ..fake_interaction(
                    "c1",
                    "steps2",
                    Meth::Select,
                    Some(Att::Question),
                    Some(&pick),
                )
            }),
            "steps2",
            json!({"goto": 3, "label": "Go back a step"}),
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
    assert_eq!(
        answered,
        ["perm", "plan", "trust", "secret", "pick", "steps"]
    );
    assert_eq!(port.st().answered[5].goto, Some(0));

    // The run switch gates the waiting requests too: with it off a question cannot be
    // answered from another PC, and putting it back lets the answer through.
    port.put_interaction(fake_interaction(
        "c1",
        "off",
        Meth::Select,
        Some(Att::Question),
        Some(&["A", "B"]),
    ));
    port.set_run(false);
    let e = b
        .rpc
        .call(
            "dev-a",
            "answer",
            json!({"chatId": "c1", "requestId": "off", "value": "A", "label": "A"}),
        )
        .await
        .unwrap_err();
    assert_eq!(e.code, code::NOT_ALLOWED);
    port.set_run(true);
    b.rpc
        .call(
            "dev-a",
            "answer",
            json!({"chatId": "c1", "requestId": "off", "value": "A", "label": "A"}),
        )
        .await
        .unwrap();

    // The chat menu's actions ride the same wire: each is answered, and the chat takes it once.
    let menu: Vec<(&str, Value, Value)> = vec![
        (
            "fork",
            json!({"chatId": "c1", "forkId": "f-1"}),
            json!({"id": "c1-copy", "revision": 9}),
        ),
        ("compact", json!({"chatId": "c1"}), json!(null)),
        (
            "roll_over",
            json!({"chatId": "c1", "model": "opus", "effort": "high"}),
            json!({"started": true}),
        ),
        (
            "cost",
            json!({"chatId": "c1"}),
            json!({"chatId": "c1", "totalTokens": 1234}),
        ),
        (
            "export_markdown",
            json!({"chatId": "c1"}),
            json!({"messages": 3, "path": "c1.md"}),
        ),
        (
            "organize",
            json!({"chatId": "c1", "pinned": true, "parentChatId": null, "toEnd": true}),
            json!([{ "chatId": "c1", "groupProjectId": "proj-1", "position": 3, "pinned": true, "parentChatId": null }]),
        ),
        ("mark_unread", json!({"chatId": "c1"}), json!({})),
        ("mute", json!({"chatId": "c1", "muted": true}), json!({})),
        (
            "move_profile",
            json!({"chatId": "c1", "profileId": "profile-p"}),
            json!({}),
        ),
    ];
    for (op, args, want) in &menu {
        assert_eq!(
            &b.rpc.call("dev-a", op, args.clone()).await.unwrap(),
            want,
            "{op}"
        );
    }
    {
        let st = port.st();
        assert_eq!(st.forked, [("c1".to_owned(), "f-1".to_owned())]);
        assert_eq!(st.compacted, ["c1"]);
        assert_eq!(st.rolled_over.len(), 1);
        assert_eq!(st.rolled_over[0].effort.as_deref(), Some("high"));
        assert_eq!(st.cost_for, ["c1"]);
        assert_eq!(st.exported, ["c1"]);
        assert_eq!(st.organized.len(), 1);
        // An explicit null parent crosses as such: the chat moves to top level.
        assert_eq!(st.organized[0].parent_chat_id, Some(None));
        assert_eq!(st.organized[0].pinned, Some(true));
        assert_eq!(st.unread_marked, ["c1"]);
        assert_eq!(st.muted.len(), 1);
        assert!(st.muted[0].muted);
        assert_eq!(st.profiles_moved.len(), 1);
    }
    // The run switch gates the menu actions like it gates a send — but cost is a read, so it
    // answers with the switch off too.
    port.set_run(false);
    for (op, args, _) in &menu {
        let got = b.rpc.call("dev-a", op, args.clone()).await;
        if *op == "cost" {
            assert!(got.is_ok(), "{op} is a read");
        } else {
            assert_eq!(got.unwrap_err().code, code::NOT_ALLOWED, "{op}");
        }
    }
    port.set_run(true);
    // The `sidebar` news of a move, pin or reorder reaches watchers as a fresh copy of the chat,
    // which is the only news such a change sends.
    let heard = b.heard("chats").len();
    host.observe(&AppEvent::new("sidebar").with("chatIds", json!(["c1"])));
    until(|| b.heard("chats").len() == heard + 1).await;
    assert_eq!(b.heard("chats")[heard].event["chat"]["id"], "c1");

    // What the composer and the chat view ask of a chat that runs on another PC: the permission
    // pick, the reply length, the side-panel eye, the @ mentions, warming, stopping one running
    // tool, withdrawing and recovering a queued message, the checkpoint read, the flags and item
    // links beside the chat, and the new-chat defaults of the PC that runs it. Each is answered
    // there, and the recorder says which function ran.
    let view: Vec<(&str, Value)> = vec![
        ("set_permission", json!({"chatId": "c1", "mode": "plan"})),
        ("verbosity_get", json!({"chatId": "c1"})),
        ("verbosity_set", json!({"chatId": "c1", "level": "high"})),
        ("context_on", json!({"chatId": "c1"})),
        (
            "quick_open",
            json!({"chatId": "c1", "query": "main", "reload": true}),
        ),
        // The side panel around the chat, and the task board of the PC that runs it.
        ("chat_folder", json!({"chatId": "c1"})),
        // A picture the person sent, read where it is stored.
        (
            "attachment_get",
            json!({"chatId": "c1", "attachmentId": "img-1", "thumb": true}),
        ),
        // The pages pinned here, and one page's document and files, a chunk at a time.
        ("pages_list", json!({})),
        ("pages_files", json!({"pageId": "pg-1"})),
        ("pages_read", json!({"pageId": "pg-1", "offset": 0})),
        (
            "pages_read",
            json!({"pageId": "pg-1", "path": "a.png", "offset": 2}),
        ),
        (
            "file_view",
            json!({"chatId": "c1", "path": "/work/src/main.rs"}),
        ),
        (
            "file_stat",
            json!({"chatId": "c1", "paths": ["/work/a.rs", "/work/gone.rs"]}),
        ),
        (
            "archive_list",
            json!({"chatId": "c1", "path": "/work/x.zip"}),
        ),
        (
            "path_resolve",
            json!({"chatId": "c1", "path": "src/main.rs"}),
        ),
        ("changes", json!({"chatId": "c1"})),
        (
            "diff",
            json!({"chatId": "c1", "path": "a.rs", "side": "unstaged"}),
        ),
        ("stage", json!({"chatId": "c1", "path": "a.rs"})),
        ("unstage", json!({"chatId": "c1", "path": "a.rs"})),
        ("board_connection", json!({"chatId": "c1"})),
        (
            "board_read",
            json!({"chatId": "c1", "action": "projects", "requestId": "tr-1", "timeoutMs": 5000}),
        ),
        ("board_cancel", json!({"chatId": "c1", "requestId": "tr-1"})),
        ("warm", json!({"chatId": "c1"})),
        ("tool_stop", json!({"chatId": "c1", "toolCallId": "t-1"})),
        (
            "submission",
            json!({"chatId": "c1", "submissionId": "s-1", "action": "withdraw"}),
        ),
        ("checkpoint", json!({"chatId": "c1", "messageId": 2})),
        ("attention", json!({"chatIds": ["c1"]})),
        ("item_links", json!({"chatIds": ["c1"]})),
        ("snooze", json!({"chatId": "c1", "until": 123})),
        ("address", json!({"chatId": "c1"})),
        ("defaults_get", json!({"chatId": "c1"})),
        (
            "defaults_set",
            json!({"chatId": "c1", "model": "opus", "responseVerbosity": "high"}),
        ),
    ];
    for (op, args) in &view {
        assert!(b.rpc.call("dev-a", op, args.clone()).await.is_ok(), "{op}");
    }
    {
        let st = port.st();
        assert_eq!(st.permission_set[0].mode, "plan");
        assert_eq!(st.verbosity_set[0].level.as_deref(), Some("high"));
        assert_eq!(st.context_asked, ["c1"]);
        assert_eq!(st.quick_opened[0].query, "main");
        assert!(st.quick_opened[0].reload);
        assert_eq!(
            st.attachments_asked,
            [("c1".to_owned(), "img-1".to_owned())]
        );
        // The person the hub says is asking is who the pages are for.
        let asked: Vec<&str> = st.pages_asked.iter().map(|(w, _)| w.as_str()).collect();
        assert_eq!(
            asked,
            [
                "list",
                "files pg-1",
                "read pg-1 None",
                "read pg-1 Some(\"a.png\")"
            ]
        );
        assert!(st.pages_asked.iter().all(|(_, p)| p == "person-a"));
        assert_eq!(st.folders_asked, ["c1"]);
        assert_eq!(
            st.panel_reads.iter().map(|(op, _)| *op).collect::<Vec<_>>(),
            [
                "file_read",
                "file_stat",
                "archive_list",
                "path_resolve",
                "changes",
                "diff"
            ]
        );
        // The stage pair arrives with the path and which way it went; a `from` rides along when given.
        assert_eq!(st.stages.len(), 2);
        assert!(st.stages[0].1 && !st.stages[1].1);
        assert_eq!(st.stages[0].0.path, "a.rs");
        assert_eq!(st.boards_connection_for, ["c1"]);
        assert_eq!(st.boards_read[0].params["action"], "projects");
        // The chat the call named does not stay inside the board parameters.
        assert!(st.boards_read[0].params.get("chatId").is_none());
        assert_eq!(st.boards_cancelled[0].request_id, "tr-1");
        assert_eq!(st.warmed, ["c1"]);
        assert_eq!(st.tool_stopped[0].tool_call_id, "t-1");
        assert_eq!(st.submissions[0].action, "withdraw");
        assert_eq!(st.checkpoint_for[0].message_id, 2);
        assert_eq!(st.item_links_for[0], ["c1".to_owned()]);
        assert_eq!(st.snoozed[0].until, Some(123));
        assert_eq!(st.addressed, ["c1"]);
        assert_eq!(st.defaults_saved.as_ref().unwrap()["model"], "opus");
        // The defaults were read and saved for the person asking, the same one both times.
        assert_eq!(st.defaults_people.len(), 2);
        assert_eq!(st.defaults_people[0], st.defaults_people[1]);
    }
    // A queued action other than withdraw or recover is refused at the wire.
    let e = b
        .rpc
        .call(
            "dev-a",
            "submission",
            json!({"chatId": "c1", "submissionId": "s-3", "action": "delete"}),
        )
        .await
        .unwrap_err();
    assert_eq!(e.code, code::BAD_ARGS);
    // Reads answer with the run switch off; every write — including each of the new ones — does
    // not. The switch covers every op that runs or changes a chat from another PC.
    port.set_run(false);
    for (op, args) in [
        ("verbosity_get", json!({"chatId": "c1"})),
        ("context_on", json!({"chatId": "c1"})),
        ("checkpoint", json!({"chatId": "c1", "messageId": 2})),
        ("attention", json!({"chatIds": ["c1"]})),
        ("item_links", json!({"chatIds": ["c1"]})),
        ("defaults_get", json!({"chatId": "c1"})),
        // The folder's name and the board answer with the switch off too.
        ("chat_folder", json!({"chatId": "c1"})),
        // So does a picture in the transcript: it is part of what the person can already read.
        (
            "attachment_get",
            json!({"chatId": "c1", "attachmentId": "img-1"}),
        ),
        ("board_connection", json!({"chatId": "c1"})),
        (
            "board_read",
            json!({"chatId": "c1", "action": "ready", "requestId": "tr-2", "timeoutMs": 5000}),
        ),
        ("board_cancel", json!({"chatId": "c1", "requestId": "tr-2"})),
        // The `/` menu's list comes from the chat's own worker: a read, nothing of this PC.
        ("skills", json!({"chatId": "c1"})),
    ] {
        assert!(
            b.rpc.call("dev-a", op, args).await.is_ok(),
            "{op} is a read"
        );
    }
    for (op, args) in [
        ("set_permission", json!({"chatId": "c1", "mode": "plan"})),
        // It reads this PC's disk, so it needs the switch even though it changes nothing.
        ("quick_open", json!({"chatId": "c1", "query": "x"})),
        // So does every read of a pinned page: it opens files on this PC.
        ("pages_list", json!({})),
        ("pages_files", json!({"pageId": "pg-1"})),
        ("pages_read", json!({"pageId": "pg-1", "offset": 0})),
        // So does every read of the project folder's files and changes.
        ("file_view", json!({"chatId": "c1", "path": "/work/a.rs"})),
        (
            "file_stat",
            json!({"chatId": "c1", "paths": ["/work/a.rs"]}),
        ),
        (
            "archive_list",
            json!({"chatId": "c1", "path": "/work/x.zip"}),
        ),
        ("path_resolve", json!({"chatId": "c1", "path": "a.rs"})),
        ("changes", json!({"chatId": "c1"})),
        (
            "diff",
            json!({"chatId": "c1", "path": "a.rs", "side": "head"}),
        ),
        ("verbosity_set", json!({"chatId": "c1", "level": "low"})),
        ("warm", json!({"chatId": "c1"})),
        ("tool_stop", json!({"chatId": "c1", "toolCallId": "t-2"})),
        (
            "submission",
            json!({"chatId": "c1", "submissionId": "s-2", "action": "recover"}),
        ),
        ("snooze", json!({"chatId": "c1", "until": 9})),
        ("address", json!({"chatId": "c1"})),
        ("defaults_set", json!({"chatId": "c1", "model": "sonnet"})),
        ("stage", json!({"chatId": "c1", "path": "a.rs"})),
        ("unstage", json!({"chatId": "c1", "path": "a.rs"})),
        // Promoting or discarding a side chat changes the chat there.
        ("side_resolve", json!({"chatId": "c1", "action": "discard"})),
    ] {
        let e = b.rpc.call("dev-a", op, args).await.unwrap_err();
        assert_eq!(e.code, code::NOT_ALLOWED, "{op}");
    }
    port.set_run(true);

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
            "c1".to_owned(),
            "r2".to_owned(),
            "deny".to_owned(),
            "device:Laptop".to_owned()
        )]
    );
    // The card states read from afar too: a tool row's chip on a copy asks for them so it
    // can say Allowed or Refused as one does on a chat that runs here.
    let states = laptop
        .rpc
        .call("dev-a", "consent_states", json!({"chatId": "c1"}))
        .await
        .unwrap();
    let seen: Vec<(String, String)> = states["states"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| {
            (
                s["id"].as_str().unwrap().to_owned(),
                s["state"].as_str().unwrap().to_owned(),
            )
        })
        .collect();
    assert_eq!(
        seen,
        [
            ("r1".to_owned(), "pending".to_owned()),
            ("r2".to_owned(), "denied".to_owned())
        ]
    );

    // A second answer to the same card is refused: the first one won.
    let e = tablet
        .rpc
        .call("dev-a", "consent_decide", decide("r2"))
        .await
        .unwrap_err();
    assert_eq!(e.code, "conflict");

    // Answered on the chat's home PC itself: the window's own click moves the same store, and the
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
    act_for: &[(&str, bool, bool, bool)],
) -> Side {
    let dir = tempfile::tempdir().unwrap();
    let mut reg = MigrationRegistry::new();
    helm_devices::register_migrations(&mut reg);
    helm_pages::register_migrations(&mut reg);
    let db = Arc::new(Database::open(&dir.path().join("watcher.sqlite3"), &reg).unwrap());
    db.write(|tx| tx.execute("UPDATE device_self SET uid = ?1", [uid]))
        .unwrap();
    let link = router.connect(uid, Some(person), Some(person_name), true);
    if !act_for.is_empty() {
        link.set_people(act_for.iter().map(|(p, _, _, _)| (*p).to_owned()).collect());
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
            .map(|(p, telegram, voice, speaker)| DevicePerson {
                person_id: (*p).to_owned(),
                telegram: *telegram,
                voice: *voice,
                speaker: *speaker,
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

/// The ids of the requests waiting in this PC's copy of a chat's thread state.
fn copied_interactions(s: &Side, chat: &str) -> Vec<String> {
    let stored =
        s.db.snapshot(|tx| {
            tx.query_row(
                "SELECT thread FROM mirror_chats WHERE chat_id = ?1",
                [chat],
                |r| r.get::<_, Option<String>>(0),
            )
            .optional()
        })
        .unwrap()
        .flatten();
    stored
        .and_then(|t| serde_json::from_str::<Value>(&t).ok())
        .and_then(|t| t.get("interactions").cloned())
        .and_then(|v| v.as_array().cloned())
        .unwrap_or_default()
        .iter()
        .filter_map(|i| i["id"].as_str().map(str::to_owned))
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
    // A project made or pinned from this PC is on the device at once, and this PC lists it
    // without waiting for the next connection: the board arrives with its folder, the pin shows.
    let made = b
        .mirror
        .project_create(
            "dev-a",
            "person-a",
            "Ideas",
            Some("About ideas".into()),
            Some("proj-1".into()),
        )
        .await
        .unwrap();
    assert_eq!(made["projectId"], "proj-new-1");
    b.mirror
        .project_set("dev-a", "person-a", "proj-1", Some(true), None)
        .await
        .unwrap();
    assert_eq!(
        port.st().projects_set,
        [helm_devices::wire::ProjectSetArgs {
            project_id: "proj-1".into(),
            pinned: Some(true),
            folder_project_id: None,
        }]
    );
    let projects =
        b.db.snapshot(|tx| mirror::mirror_projects(tx, "dev-a"))
            .unwrap();
    let find = |id: &str| projects.iter().find(|p| p["id"] == id).unwrap().clone();
    assert_eq!(
        (
            find("proj-new-1")["kind"].as_str(),
            find("proj-new-1")["folderId"].as_str()
        ),
        (Some("board"), Some("proj-1"))
    );
    assert_eq!(find("proj-1")["pinned"], true);
    // A PC that does not let others run its chats takes neither, and says so.
    port.set_run(false);
    let e = b
        .mirror
        .project_set("dev-a", "person-a", "proj-1", Some(false), None)
        .await
        .unwrap_err();
    assert_eq!(e.code, "not_allowed");
    port.set_run(true);
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
    let pages = Arc::new(helm_pages::PageServer::new(
        helm_panel::private::PrivateRoots::default(),
    ));
    services.insert(pages.clone());
    let routes = Arc::new(RemoteSlot::default());
    routes.attach(Arc::new(b.mirror.clone()));
    services.insert(routes);
    services.insert(Arc::new(OpIntercept(Arc::new(Routes))));
    let slot = Arc::new(MirrorSlot::default());
    slot.attach(b.mirror.clone());
    services.insert(slot);
    // The save dialog of this PC: it picks a file in the watcher's folder and says which.
    let saves = b.db.path().with_file_name("saved");
    std::fs::create_dir_all(&saves).unwrap();
    let suggested = Arc::new(Mutex::new(Vec::<String>::new()));
    let (into, named) = (saves.clone(), suggested.clone());
    services.insert(Arc::new(helm_chats::output_save::FileSaver(Arc::new(
        move |name: &str| {
            named.lock().unwrap().push(name.to_owned());
            Ok(Some(into.join(name)))
        },
    ))));
    let mut ops = OpRegistry::new();
    helm_chats::register_ops(&mut ops);
    helm_devices::register_ops(&mut ops);
    // `chat_organize` is registered by a crate this test does not link. The name must be
    // known for the interceptor to answer it; the handler must never run for a copied chat.
    ops.blocking("chat_organize", OpClass::Maintenance, |_, _| {
        Err(ApiError::internal(
            "the local organize handler ran for a copied chat",
        ))
    });
    // Some rows below call ops registered by crates this test does not link. The names must
    // be known for the interceptor to answer them; none may run locally for a copied chat.
    for name in [
        "chat_verbosity_get",
        "chat_verbosity_set",
        "panel_context_get",
        "quick_open",
        "chat_folder",
        "file_read",
        "dir_list",
        "file_stat",
        "archive_list",
        "path_resolve",
        "changes",
        "diff",
        "changes_stage",
        "changes_unstage",
    ] {
        ops.blocking(name, OpClass::Blocking, move |_, _| {
            Err(ApiError::internal(
                "the local handler ran for a copied chat",
            ))
        });
    }
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
    // A copy made from an older device holds no card list (null); the window still gets a list.
    b.db.write(|tx| {
        tx.execute(
            "UPDATE mirror_chats SET consent = 'null' WHERE chat_id = 'c1'",
            [],
        )
    })
    .unwrap();
    let old = api
        .call_host("devices_consent_list", json!({"chatId": "c1"}))
        .await
        .unwrap();
    assert_eq!(old["pending"], json!([]));
    api.call_host(
        "devices_consent_decide",
        json!({"chatId": "c1", "requestId": "r1", "decision": "allow", "grantMinutes": 15}),
    )
    .await
    .unwrap();
    assert_eq!(
        port.st().consents,
        [(
            "c1".to_owned(),
            "r1".to_owned(),
            "allow".to_owned(),
            "device:Laptop".to_owned()
        )]
    );
    // The state read travels to the copy's window as well: its tool chip says Allowed.
    let states = api
        .call_host("devices_consent_state", json!({"chatId": "c1"}))
        .await
        .unwrap();
    let r1 = states["states"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["id"] == "r1")
        .unwrap();
    assert_eq!(r1["state"], "allowed");

    // The host's answer closes the card on this PC too, on the same event.
    host.observe(&AppEvent::new("consent").with("chatId", "c1"));
    until(|| b.sink.kinds().iter().filter(|k| *k == "consent").count() >= 2).await;
    let cards = api
        .call_host("devices_consent_list", json!({"chatId": "c1"}))
        .await
        .unwrap();
    assert!(cards["pending"].as_array().unwrap().is_empty());

    // Every kind of request a chat raises and waits on reaches the copy and can be answered
    // from this PC: a tool permission, a question, a plan approval, a trust prompt and a
    // secret input. Each answer reaches the chat on the PC that runs it, and the thread
    // event it raises there closes the card back here.
    let kinds: Vec<(&str, Interaction, Value)> = vec![
        (
            "a tool permission",
            fake_interaction("c1", "k-perm", Meth::Confirm, Some(Att::Approval), None),
            json!({"confirmed": true}),
        ),
        (
            "a question",
            fake_interaction(
                "c1",
                "k-ask",
                Meth::Select,
                Some(Att::Question),
                Some(&["A", "B"]),
            ),
            json!({"value": "A"}),
        ),
        (
            "a plan approval",
            fake_interaction(
                "c1",
                "k-plan",
                Meth::Select,
                Some(Att::Plan),
                Some(&["Approve plan", "Keep planning"]),
            ),
            json!({"value": "Approve plan", "confirmed": true}),
        ),
        (
            "a trust prompt",
            fake_interaction("c1", "k-trust", Meth::Confirm, Some(Att::Trust), None),
            json!({"confirmed": true}),
        ),
        (
            "a secret input",
            Interaction {
                secret: Some(true),
                ..fake_interaction("c1", "k-secret", Meth::Input, Some(Att::Question), None)
            },
            json!({"value": "hunter2"}),
        ),
    ];
    for (what, waiting, extra) in kinds {
        port.put_interaction(waiting.clone());
        host.observe(
            &AppEvent::new("thread")
                .with("chatId", "c1")
                .with("thread", json!({ "interactions": [waiting.clone()] })),
        );
        until(|| copied_interactions(&b, "c1") == vec![waiting.id.clone()]).await;
        let mut args = json!({"chatId": "c1", "requestId": waiting.id});
        args.as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        let got = api.call_host("interaction_answer", args).await;
        assert!(got.is_ok(), "{what}: {got:?}");
        host.observe(
            &AppEvent::new("thread")
                .with("chatId", "c1")
                .with("thread", json!({ "interactions": [] })),
        );
        until(|| copied_interactions(&b, "c1").is_empty()).await;
    }
    let answered_from_copy: Vec<_> = port
        .st()
        .answered
        .iter()
        .map(|a| a.request_id.clone())
        .collect();
    assert_eq!(
        answered_from_copy,
        ["k-perm", "k-ask", "k-plan", "k-trust", "k-secret"]
    );

    // The waiting-card count lives in the copy too, so the sidebar's needs-you mark can count
    // it: a question waiting on the far PC puts a count and a mark on the copy's record, and
    // closing the card clears both.
    let copy_state = || {
        b.db.snapshot(|tx| {
            tx.query_row(
                "SELECT record FROM mirror_chats WHERE chat_id = 'c1'",
                [],
                |r| r.get::<_, String>(0),
            )
        })
        .ok()
        .and_then(|s| serde_json::from_str::<Value>(&s).ok())
        .map(|v| (v["waitingCards"].as_i64(), v["needsAttention"].as_bool()))
    };
    let waiting = fake_interaction(
        "c1",
        "wait-1",
        Meth::Select,
        Some(Att::Question),
        Some(&["A", "B"]),
    );
    port.put_interaction(waiting.clone());
    host.observe(
        &AppEvent::new("thread")
            .with("chatId", "c1")
            .with("thread", json!({ "interactions": [waiting] })),
    );
    until(|| copy_state() == Some((Some(1), Some(true)))).await;
    host.observe(
        &AppEvent::new("thread")
            .with("chatId", "c1")
            .with("thread", json!({ "interactions": [] })),
    );
    until(|| copy_state() == Some((Some(0), Some(false)))).await;

    // Every chat op on a copied chat: answered from the copy, sent to the device, or answered
    // quietly. Anything the PC that runs the chat must decide answers with that PC's words.
    let table: Vec<(&str, Value, Result<Value, &str>)> = vec![
        ("transcript_page", json!({"chatId": "c1"}), Ok(json!(null))),
        // A roll-over stack lives whole on the PC that runs the chat: its calls answer
        // there, and a stack page carries its switches across the link intact.
        (
            "chat_stack",
            json!({"chatId": "c1"}),
            Ok(
                json!({"stackId": "c1", "latestId": "c1", "sessions": [{ "id": "c1", "title": "far stack", "archived": false, "createdAt": 1 }]}),
            ),
        ),
        (
            "stack_find",
            json!({"chatId": "c1", "query": "keep"}),
            Ok(json!({"rows": [{ "sessionId": "c1", "id": 7, "count": 4 }], "truncated": false})),
        ),
        (
            "transcript_page",
            json!({"chatId": "c1", "stack": true, "around": 3}),
            Ok(json!(null)),
        ),
        ("chat_warm", json!({"chatId": "c1"}), Ok(json!(null))),
        // The `/` menu of the window lists what the chat's worker reports on the PC that runs
        // the chat, so the person sees that chat's commands, not this PC's.
        (
            "skills_list",
            json!({"chatId": "c1"}),
            Ok(json!([
                { "name": "review", "description": "Review the current branch" },
                { "name": "pdf-extract", "description": "Read a PDF", "scope": "helm-skill" },
            ])),
        ),
        // The Rename dialog asks the PC that runs the chat, where the chat's words live, and
        // the options come back whole.
        (
            "title_suggest",
            json!({"chatId": "c1"}),
            Ok(
                json!({"title": "Name made where the chat runs", "titles": ["Name made where the chat runs"]}),
            ),
        ),
        (
            "model_catalogue",
            json!({"harness": "claude", "deviceUid": "dev-a"}),
            Ok(json!({"models": []})),
        ),
        (
            "chat_fork",
            json!({"chatId": "c1", "forkId": "f-1"}),
            Ok(json!(null)),
        ),
        ("chat_compact", json!({"chatId": "c1"}), Ok(json!(null))),
        (
            "chat_roll_over",
            json!({"chatId": "c1", "model": "sonnet"}),
            Ok(json!(null)),
        ),
        (
            "chat_mute",
            json!({"chatId": "c1", "muted": true}),
            Ok(json!(null)),
        ),
        ("unread_mark", json!({"chatId": "c1"}), Ok(json!(null))),
        (
            "chat_organize",
            json!({"chatId": "c1", "pinned": true, "parentChatId": null}),
            Ok(json!([])),
        ),
        // The composer and chat view: each travels to the PC that runs the chat.
        (
            "chat_set_permission_mode",
            json!({"chatId": "c1", "mode": "plan"}),
            Ok(json!(null)),
        ),
        (
            "chat_verbosity_get",
            json!({"chatId": "c1"}),
            Ok(json!({"level": "low", "own": false, "default": "low"})),
        ),
        (
            "chat_verbosity_set",
            json!({"chatId": "c1", "level": "high"}),
            Ok(json!({"level": "high", "own": true, "default": "low"})),
        ),
        (
            "panel_context_get",
            json!({"chatId": "c1"}),
            Ok(json!({"on": false})),
        ),
        (
            "quick_open",
            json!({"chatId": "c1", "query": "main"}),
            Ok(json!({"mode": "files", "items": [{ "path": "src/main.ts" }], "partial": false})),
        ),
        // The side panel around a copied chat, and the task board of the PC that runs it:
        // each is answered there, and the answer has the shape the panel and the Tasks view
        // expect. Staging answers with the Changes view, not the chat record.
        (
            "chat_folder",
            json!({"chatId": "c1"}),
            Ok(json!({ "path": "/work" })),
        ),
        // A picture in a message, read from the PC that stored it, thumbnail or full size.
        (
            "attachment_get",
            json!({"chatId": "c1", "attachmentId": "img-1", "thumb": true}),
            Ok(json!({"mime": "image/png", "bytes": "AAAA"})),
        ),
        (
            "file_read",
            json!({"chatId": "c1", "path": "/work/src/main.ts"}),
            Ok(
                json!({"path": "/work/src/main.ts", "kind": "text", "size": 5, "text": "one\n", "truncated": false}),
            ),
        ),
        (
            "dir_list",
            json!({"chatId": "c1", "path": "/work/src"}),
            Ok(
                json!({"path": "/work/src", "kind": "text", "size": 5, "text": "one\n", "truncated": false}),
            ),
        ),
        (
            "file_stat",
            json!({"chatId": "c1", "paths": ["/work/src/main.ts", "/work/gone.ts"]}),
            Ok(json!([null, null])),
        ),
        (
            "archive_list",
            json!({"chatId": "c1", "path": "/work/x.zip"}),
            Ok(json!({"op": "archive_list", "args": {"chatId": "c1", "path": "/work/x.zip"}})),
        ),
        (
            "path_resolve",
            json!({"chatId": "c1", "path": "src/main.ts"}),
            Ok(json!({"op": "path_resolve", "args": {"chatId": "c1", "path": "src/main.ts"}})),
        ),
        (
            "changes",
            json!({"chatId": "c1"}),
            Ok(json!({"repoTop": "/work", "branch": "main", "records": [], "truncated": false})),
        ),
        (
            "diff",
            json!({"chatId": "c1", "path": "a.ts", "side": "unstaged"}),
            Ok(json!({"patch": "", "fullContext": false, "truncated": false})),
        ),
        (
            "changes_stage",
            json!({"chatId": "c1", "path": "a.ts"}),
            Ok(
                json!({"changes": {"repoTop": "/work", "branch": "main", "records": [], "truncated": false}, "noop": false}),
            ),
        ),
        (
            "changes_unstage",
            json!({"chatId": "c1", "path": "a.ts", "from": "head"}),
            Ok(
                json!({"changes": {"repoTop": "/work", "branch": "main", "records": [], "truncated": false}, "noop": false}),
            ),
        ),
        (
            "devices_board_connection",
            json!({"chatId": "c1"}),
            Ok(json!({"url": "https://board.example", "hasToken": true})),
        ),
        (
            "devices_board_read",
            json!({"chatId": "c1", "action": "projects", "requestId": "tr-1", "timeoutMs": 5000}),
            Ok(
                json!({"action": "projects", "data": {"projects": [{"id": "p1", "title": "Work"}], "complete": true}}),
            ),
        ),
        (
            "devices_board_cancel",
            json!({"chatId": "c1", "requestId": "tr-1"}),
            Ok(json!({"cancelled": true})),
        ),
        (
            "tool_stop",
            json!({"chatId": "c1", "toolCallId": "t-9"}),
            Ok(json!({"stopped": true})),
        ),
        (
            "checkpoint_info",
            json!({"chatId": "c1", "messageId": 2}),
            Ok(
                json!({"chatId": "c1", "text": "back to here", "userMessages": 1, "files": [], "filesRestored": false}),
            ),
        ),
        (
            "attention_snooze",
            json!({"chatId": "c1", "until": 50}),
            Ok(json!(null)),
        ),
        (
            "attention_address",
            json!({"chatId": "c1"}),
            Ok(json!(null)),
        ),
        (
            "devices_submission_action",
            json!({"chatId": "c1", "submissionId": "s-1", "action": "withdraw"}),
            Ok(json!(null)),
        ),
        (
            "devices_composer_defaults",
            json!({"chatId": "c1", "patch": {"model": "opus", "responseVerbosity": "high"}}),
            Ok(json!({"model": "opus", "effort": "high", "responseVerbosity": "high"})),
        ),
        (
            "devices_item_links",
            json!({"chatIds": ["c1"]}),
            Ok(
                json!({"links": [{ "chatId": "c1", "itemId": "item-7", "title": "Task seven", "main": true }], "unreachable": []}),
            ),
        ),
        (
            "devices_attention",
            json!({"chatIds": ["c1"]}),
            Ok(
                json!({"needsYou": [], "flagged": [{ "chatId": "c1", "kind": "idle", "reason": "quiet but unfinished" }], "unreachable": []}),
            ),
        ),
        // What an agent showed in the chat reads from the PC that runs it.
        (
            "shown_image_thumb",
            json!({"chatId": "c1", "galleryId": "g1", "index": 1}),
            Ok(json!({"mime": "image/png", "bytes": "AAAA"})),
        ),
        (
            "shown_gallery_get",
            json!({"chatId": "c1", "galleryId": "g1"}),
            Ok(json!(null)),
        ),
        (
            "chat_stop_no_resend",
            json!({"chatId": "c1"}),
            Ok(json!(null)),
        ),
        // Reading a sent message for the edit window answers with the text stored on the PC
        // that runs the chat. (Its staged resend, and the transcript rewind that follows it,
        // are exercised at the end of the test, once every chat op has had its turn.)
        (
            "message_edit_open",
            json!({"chatId": "c1", "messageId": 1}),
            Ok(json!({"messageId": 1, "revision": 5, "text": "hello"})),
        ),
        // A permanent delete travels too; this watcher acts as the PC's own person, and the
        // install's own delete rules still answer there.
        ("chat_delete", json!({"chatId": "c1"}), Ok(json!(null))),
        // A side chat's resolve travels too: promote answers with the record there, discard
        // answers null. Both answer only for the chat's own person (the household rows).
        (
            "side_chat_resolve",
            json!({"chatId": "c1", "action": "discard"}),
            Ok(json!(null)),
        ),
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
            Ok(v) => assert_eq!(
                got.clone().unwrap_or_else(|e| panic!("{op}: {e}")),
                v,
                "{op}"
            ),
            Err(m) => assert_eq!(got.unwrap_err().message, m, "{op}"),
        }
    }
    // The model picker's rows travel the way they read on a chat that runs here: the effort/
    // thinking row changes the effort with no model named, the model row a model with its
    // effort. Each goes to the PC that runs the chat, and the copy shows it at once.
    api.call_host("chat_set_model", json!({"chatId": "c1", "effort": "low"}))
        .await
        .unwrap();
    api.call_host(
        "chat_set_model",
        json!({"chatId": "c1", "model": "sonnet", "effort": "high"}),
    )
    .await
    .unwrap();
    assert_eq!(
        port.st().model_changes,
        [
            helm_devices::wire::SetModelArgs {
                chat_id: "c1".to_owned(),
                model: None,
                effort: Some("low".to_owned()),
            },
            helm_devices::wire::SetModelArgs {
                chat_id: "c1".to_owned(),
                model: Some("sonnet".to_owned()),
                effort: Some("high".to_owned()),
            },
        ]
    );
    let got = api
        .call_host("chat_get", json!({"chatId": "c1"}))
        .await
        .unwrap();
    assert_eq!(
        (
            got["chat"]["model"].as_str(),
            got["chat"]["effort"].as_str()
        ),
        (Some("sonnet"), Some("high"))
    );
    // The sidebar's open-item state travels the same way: the PC that runs the chat stores
    // it, and this PC's copy shows it at once. An explicit null is a real value there too:
    // the copy folds again.
    api.call_host(
        "chat_set_sidebar_state",
        json!({"chatId": "c1", "state": {"v": 1, "expanded": true, "openCards": [7]}}),
    )
    .await
    .unwrap();
    let got = api
        .call_host("chat_get", json!({"chatId": "c1"}))
        .await
        .unwrap();
    assert_eq!(got["chat"]["sidebarState"]["expanded"], json!(true));
    assert_eq!(got["chat"]["sidebarState"]["openCards"], json!([7]));
    // A partial update replaces only the field it carries, the way the PC that runs the chat
    // stores it: a card change keeps the row open, and a row fold keeps the opened cards.
    api.call_host(
        "chat_set_sidebar_state",
        json!({"chatId": "c1", "state": {"v": 1, "openCards": [8, 9]}}),
    )
    .await
    .unwrap();
    let got = api
        .call_host("chat_get", json!({"chatId": "c1"}))
        .await
        .unwrap();
    assert_eq!(
        got["chat"]["sidebarState"]["expanded"],
        json!(true),
        "a card change says nothing about the row"
    );
    assert_eq!(got["chat"]["sidebarState"]["openCards"], json!([8, 9]));
    api.call_host(
        "chat_set_sidebar_state",
        json!({"chatId": "c1", "state": {"v": 1, "expanded": false}}),
    )
    .await
    .unwrap();
    let got = api
        .call_host("chat_get", json!({"chatId": "c1"}))
        .await
        .unwrap();
    assert!(
        got["chat"]["sidebarState"]["expanded"].is_null(),
        "a folded row leaves expanded out, as it is stored"
    );
    assert_eq!(
        got["chat"]["sidebarState"]["openCards"],
        json!([8, 9]),
        "and the fold says nothing about the cards"
    );
    api.call_host(
        "chat_set_sidebar_state",
        json!({"chatId": "c1", "state": null}),
    )
    .await
    .unwrap();
    let got = api
        .call_host("chat_get", json!({"chatId": "c1"}))
        .await
        .unwrap();
    assert!(
        got["chat"]["sidebarState"].is_null(),
        "the null reset crossed too"
    );
    assert_eq!(
        port.st().sidebar_state_set,
        [
            helm_devices::wire::SidebarStateArgs {
                chat_id: "c1".to_owned(),
                state: json!({"v": 1, "expanded": true, "openCards": [7]}),
            },
            helm_devices::wire::SidebarStateArgs {
                chat_id: "c1".to_owned(),
                state: json!({"v": 1, "openCards": [8, 9]}),
            },
            helm_devices::wire::SidebarStateArgs {
                chat_id: "c1".to_owned(),
                state: json!({"v": 1, "expanded": false}),
            },
            helm_devices::wire::SidebarStateArgs {
                chat_id: "c1".to_owned(),
                state: Value::Null,
            },
        ]
    );
    // The lightning switch travels the same way: it changes the chat on the PC that runs it,
    // so the run switch gates it, and this PC's copy keeps its pick in step. An explicit null
    // is a real value there too: the chat drops its own pick and follows that PC's default.
    port.set_run(false);
    let e = api
        .call_host("chat_set_fast_mode", json!({"chatId": "c1", "on": true}))
        .await
        .unwrap_err();
    assert_eq!(e.code, "not_allowed");
    port.set_run(true);
    api.call_host("chat_set_fast_mode", json!({"chatId": "c1", "on": true}))
        .await
        .unwrap();
    let got = api
        .call_host("chat_get", json!({"chatId": "c1"}))
        .await
        .unwrap();
    assert_eq!(got["chat"]["fastMode"], json!(true));
    api.call_host("chat_set_fast_mode", json!({"chatId": "c1", "on": null}))
        .await
        .unwrap();
    let got = api
        .call_host("chat_get", json!({"chatId": "c1"}))
        .await
        .unwrap();
    assert!(
        got["chat"]["fastMode"].is_null(),
        "the null pick crossed too"
    );
    assert_eq!(
        port.st().fast_mode_set,
        [
            helm_devices::wire::FastModeArgs {
                chat_id: "c1".to_owned(),
                on: Some(true),
            },
            helm_devices::wire::FastModeArgs {
                chat_id: "c1".to_owned(),
                on: None,
            },
        ]
    );
    // The Rename dialog's ask reached the PC that runs the chat.
    assert_eq!(port.st().titles_asked, ["c1"]);
    // The pages pinned on the other PC list here, and one opens from a copy brought across: the
    // document and the files it names, served in a sealed session of this PC. Its messages go to
    // the chat the pin names, and nothing else of the other PC is reachable.
    let listed = api.call_host("devices_pages", json!({})).await.unwrap();
    assert_eq!(
        (
            listed["devices"][0]["name"].as_str(),
            listed["devices"][0]["pages"][0]["title"].as_str(),
            listed["devices"][0]["pages"][0]["chatId"].as_str(),
            listed["devices"][0]["problem"].is_null()
        ),
        (Some("Desk"), Some("Plan"), Some("c1"), true)
    );
    let opened = api
        .call_host(
            "devices_page_open",
            json!({"deviceUid": "dev-a", "pageId": "pg-1"}),
        )
        .await
        .unwrap();
    let session = opened["session"].as_str().unwrap();
    assert_eq!(
        (
            opened["title"].as_str(),
            opened["pins"][0]["chatId"].as_str(),
            opened["stateRevision"].as_i64()
        ),
        (Some("Plan"), Some("c1"), Some(0))
    );
    // The network follows the pin, and the pin lives there: the copy runs online, as it would
    // on the PC that owns it.
    assert_eq!(opened["network"], json!(true));
    let doc = pages.respond("GET", &format!("/{session}/index.html"));
    assert!(
        doc.headers
            .iter()
            .any(|(k, v)| *k == "content-security-policy" && v.contains("wss:")),
        "the brought-across copy is opened with the network on"
    );
    assert!(String::from_utf8(doc.body)
        .unwrap()
        .ends_with("<p>plan</p>"));
    assert_eq!(
        pages.respond("GET", &format!("/{session}/a.png")).body,
        [1, 2, 3]
    );
    for bad in ["b.png", "../a.png", "plan.html"] {
        assert_eq!(
            pages.respond("GET", &format!("/{session}/{bad}")).status,
            404,
            "{bad}"
        );
    }
    let gone = api
        .call_host(
            "devices_page_open",
            json!({"deviceUid": "dev-a", "pageId": "pg-9"}),
        )
        .await
        .unwrap_err();
    assert_eq!(gone.code, "not_found");
    // With the other PC's switch off the list says so in words and the page does not open.
    port.set_run(false);
    let off = api.call_host("devices_pages", json!({})).await.unwrap();
    assert_eq!(off["devices"][0]["problem"]["code"], "not_allowed");
    assert_eq!(off["devices"][0]["pages"], json!([]));
    let e = api
        .call_host(
            "devices_page_open",
            json!({"deviceUid": "dev-a", "pageId": "pg-1"}),
        )
        .await
        .unwrap_err();
    assert_eq!(e.code, "not_allowed");
    port.set_run(true);

    // Pictures too big for one frame cross in chunks and are joined here: a gallery's full
    // picture and a message's picture, two megabytes each, read from where the last chunk ended.
    use base64::Engine as _;
    let b64 = base64::engine::general_purpose::STANDARD;
    let big = helm_devices::testing::fake_picture(helm_devices::testing::FAKE_PICTURE_BYTES);
    for (op, args) in [
        (
            "shown_image_get",
            json!({"chatId": "c1", "galleryId": "g1", "index": 0}),
        ),
        (
            "attachment_get",
            json!({"chatId": "c1", "attachmentId": "img-1"}),
        ),
    ] {
        port.st().blob_reads.clear();
        let got = api.call_host(op, args).await.unwrap();
        assert_eq!(got["mime"], "image/png", "{op}");
        assert_eq!(
            b64.decode(got["bytes"].as_str().unwrap()).unwrap(),
            big,
            "{op}"
        );
        let offsets: Vec<u64> = port.st().blob_reads.iter().map(|(_, o)| *o).collect();
        assert_eq!(offsets, [0, 655_360, 1_310_720, 1_966_080], "{op}");
    }
    // A picture a reply names by its path on that PC shows here, from inside the chat's
    // project folder only.
    let shot = api
        .call_host(
            "local_image_get",
            json!({"chatId": "c1", "path": "/work/shot.png"}),
        )
        .await
        .unwrap();
    assert_eq!(
        b64.decode(shot["bytes"].as_str().unwrap()).unwrap(),
        helm_devices::testing::fake_picture(64)
    );
    let e = api
        .call_host(
            "local_image_get",
            json!({"chatId": "c1", "path": "/elsewhere/shot.png"}),
        )
        .await
        .unwrap_err();
    assert_eq!(e.message, "That path is outside the chat's project folder.");
    // The file viewer's own read of a picture that is too big for one frame: the read answers
    // with the picture marked oversize and no bytes, and the whole picture then crosses in
    // chunks — the viewer sees what a local read would have answered, with no marker left.
    port.st().blob_reads.clear();
    let shot = api
        .call_host(
            "file_read",
            json!({"chatId": "c1", "path": "/work/big.png"}),
        )
        .await
        .unwrap();
    assert_eq!(shot["kind"], "image", "{shot}");
    assert_eq!(
        shot["size"],
        json!(helm_devices::testing::FAKE_PICTURE_BYTES)
    );
    assert_eq!(shot["mime"], "image/png");
    assert!(
        shot.get("oversize").is_none(),
        "the marker is not the viewer's"
    );
    assert_eq!(b64.decode(shot["bytes"].as_str().unwrap()).unwrap(), big,);
    let offsets: Vec<u64> = port.st().blob_reads.iter().map(|(_, o)| *o).collect();
    assert_eq!(offsets, [0, 655_360, 1_310_720, 1_966_080]);
    // And the local limit is the local answer: a picture past what any read previews is not
    // previewed, and no chunks are fetched for it.
    port.st().blob_reads.clear();
    let e = api
        .call_host(
            "file_read",
            json!({"chatId": "c1", "path": "/work/huge.png"}),
        )
        .await
        .unwrap_err();
    assert_eq!(
        e.message,
        "The image is larger than 8 MiB, so it is not previewed."
    );
    assert!(port.st().blob_reads.is_empty());
    // A picture one frame can carry still crosses inside the view, no chunks asked.
    port.st().blob_reads.clear();
    let shot = api
        .call_host(
            "file_read",
            json!({"chatId": "c1", "path": "/work/shot.png"}),
        )
        .await
        .unwrap();
    assert_eq!(shot["kind"], "image");
    assert_eq!(
        b64.decode(shot["bytes"].as_str().unwrap()).unwrap(),
        helm_devices::testing::fake_picture(64),
    );
    assert!(port.st().blob_reads.is_empty());
    // An option board is read and answered from here; the first answer stands.
    let board = api
        .call_host(
            "option_board_state",
            json!({"chatId": "c1", "boardId": "b1"}),
        )
        .await
        .unwrap();
    assert!(board["answer"].is_null());
    let answered = api
        .call_host(
            "option_board_answer",
            json!({"chatId": "c1", "boardId": "b1", "labels": ["A"]}),
        )
        .await
        .unwrap();
    assert_eq!(answered["answer"]["labels"], json!(["A"]));
    let again = api
        .call_host(
            "option_board_answer",
            json!({"chatId": "c1", "boardId": "b1", "labels": ["B"]}),
        )
        .await
        .unwrap_err();
    assert_eq!(again.code, "conflict");
    // A tool's whole output and a project file are saved on this PC, where the person chose.
    let saved = api
        .call_host(
            "message_output_save",
            json!({"chatId": "c1", "messageId": 7}),
        )
        .await
        .unwrap();
    let path = std::path::PathBuf::from(saved["path"].as_str().unwrap());
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        helm_devices::testing::FAKE_OUTPUT
    );
    let copy = api
        .call_host(
            "devices_file_save",
            json!({"chatId": "c1", "path": "/work/shot.png"}),
        )
        .await
        .unwrap();
    assert_eq!(
        std::fs::read(copy["path"].as_str().unwrap()).unwrap(),
        helm_devices::testing::fake_picture(64)
    );
    assert_eq!(
        *suggested.lock().unwrap(),
        ["bash-output.txt".to_owned(), "shot.png".to_owned()]
    );
    let e = api
        .call_host(
            "devices_file_save",
            json!({"chatId": "c1", "path": "/elsewhere/keys.txt"}),
        )
        .await
        .unwrap_err();
    assert_eq!(e.message, "That path is outside the chat's project folder.");
    assert_eq!(port.st().stopped_no_resend, ["c1".to_owned()]);

    // A chat that is not a copy is left to its own handler.
    let own = api
        .call_host("chat_fork", json!({"chatId": "mine"}))
        .await
        .unwrap_err();
    assert!(!own.message.contains("runs on"), "{own:?}");
    // The menu ops reached the device once each, and the copy shows the change at once.
    {
        let st = port.st();
        assert_eq!(st.forked, [("c1".to_owned(), "f-1".to_owned())]);
        assert_eq!(st.compacted, ["c1"]);
        assert_eq!(st.rolled_over.len(), 1);
        assert_eq!(st.rolled_over[0].model.as_deref(), Some("sonnet"));
        assert_eq!(st.unread_marked, ["c1"]);
        assert!(st.muted.iter().all(|m| m.muted) && st.muted.len() == 1);
        assert_eq!(st.organized.len(), 1);
        assert_eq!(st.organized[0].parent_chat_id, Some(None));
        assert_eq!(st.permission_set[0].mode, "plan");
        assert_eq!(st.verbosity_set[0].level.as_deref(), Some("high"));
        assert_eq!(st.context_asked, ["c1"]);
        assert_eq!(st.quick_opened[0].query, "main");
        // Warming now travels too: a chat typed into from afar warms the model on its own PC.
        assert_eq!(st.warmed, ["c1"]);
        assert_eq!(st.tool_stopped[0].tool_call_id, "t-9");
        assert_eq!(st.checkpoint_for[0].message_id, 2);
        assert_eq!(st.submissions[0].action, "withdraw");
        assert_eq!(st.defaults_saved.as_ref().unwrap()["model"], "opus");
        assert_eq!(st.item_links_for[0], ["c1".to_owned()]);
        assert_eq!(st.snoozed[0].until, Some(50));
        assert_eq!(st.addressed, ["c1"]);
        // The panel reads all arrived as the panel's own ops; `dir_list` is answered by the
        // file-view read, as it is here. The three last are the file viewer's own reads of
        // pictures on the other PC: the big one, the past-the-limit one, and the small one.
        // Staging came through as the stage pair, and the
        // board reads named the chat outside the board's own parameters, not inside them.
        assert_eq!(st.folders_asked, ["c1"]);
        assert_eq!(
            st.panel_reads.iter().map(|(op, _)| *op).collect::<Vec<_>>(),
            [
                "file_read",
                "file_read",
                "file_stat",
                "archive_list",
                "path_resolve",
                "changes",
                "diff",
                "file_read",
                "file_read",
                "file_read"
            ]
        );
        assert_eq!(st.stages.len(), 2);
        assert!(st.stages[0].1 && !st.stages[1].1);
        assert_eq!(st.stages[1].0.from.as_deref(), Some("head"));
        assert_eq!(st.boards_connection_for, ["c1"]);
        assert_eq!(st.boards_read[0].chat_id, "c1");
        assert!(st.boards_read[0].params.get("chatId").is_none());
        assert_eq!(st.boards_cancelled[0].request_id, "tr-1");
    }
    // A fork answers with the new chat's record, and the copy holds it at once, so the control
    // that opens it finds it.
    let forked = api
        .call_host("chat_fork", json!({"chatId": "c1", "forkId": "f-2"}))
        .await
        .unwrap();
    assert_eq!(forked["id"], "c1-copy");
    assert!(b
        .db
        .snapshot(|tx| tx.query_row(
            "SELECT 1 FROM mirror_chats WHERE chat_id = 'c1-copy'",
            [],
            |_| Ok(())
        ))
        .is_ok());
    let rec =
        b.db.snapshot(|tx| {
            tx.query_row(
                "SELECT record FROM mirror_chats WHERE chat_id = 'c1'",
                [],
                |r| r.get::<_, String>(0),
            )
        })
        .unwrap();
    let rec: Value = serde_json::from_str(&rec).unwrap();
    assert_eq!(
        (rec["muted"].as_bool(), rec["pinned"].as_bool()),
        (Some(true), Some(true)),
        "the copy shows the mute and the pin without waiting for a push"
    );
    assert_eq!(rec["position"], 3);
    assert!(rec["parentChatId"].is_null());

    // A send reaches the device once, as the person's own words.
    let id = "7f0f2f0e-6b0e-4d52-8d3a-1f0e2d3c4b5a";
    let send = json!({"chatId": "c1", "submissionId": id, "text": "hi", "action": "steer", "draftRevision": 0});
    let sent = api.call_host("chat_send", send.clone()).await.unwrap();
    assert_eq!(sent["submission"]["state"], "accepted");
    assert_eq!(
        port.st().sent,
        [("c1".to_owned(), id.to_owned(), "hi".to_owned())]
    );
    // A page's send crosses unchanged: the origin and the card words ride along, so the chat on
    // the PC that runs it shows the page's card and knows the page wrote the message.
    let page = api
        .call_host(
            "chat_send",
            json!({"chatId": "c1", "submissionId": "7f0f2f0e-6b0e-4d52-8d3a-1f0e2d3c4b6a",
                   "text": "Approve these", "action": "default", "draftRevision": 0,
                   "origin": "page", "pageTitle": "Approvals", "pageOverview": "five elements"}),
        )
        .await
        .unwrap();
    assert_eq!(page["submission"]["origin"], "page");
    {
        let st = port.st();
        let words = st
            .sent_page
            .last()
            .unwrap()
            .as_ref()
            .expect("the page words cross");
        assert_eq!(
            (words.title.as_deref(), words.overview.as_deref()),
            (Some("Approvals"), Some("five elements"))
        );
        assert_eq!(
            st.sent_page.first(),
            Some(&None),
            "the plain send carried no page"
        );
    }

    // A message's kind rides the same wire: the PC that runs the chat stamps the row with it, so
    // a chat-to-chat message keeps its kind after the crossing.
    let crossed = api
        .call_host(
            "chat_send",
            json!({"chatId": "c1", "submissionId": "7f0f2f0e-6b0e-4d52-8d3a-1f0e2d3c4b7a",
                   "text": "steer across", "action": "steer", "draftRevision": 0, "kind": "steer"}),
        )
        .await
        .unwrap();
    assert_eq!(crossed["submission"]["state"], "accepted");
    let kinds: Vec<_> = port
        .st()
        .sent_kind
        .iter()
        .map(|k| k.as_deref().unwrap_or("-").to_owned())
        .collect();
    assert_eq!(
        kinds.last().map(String::as_str),
        Some("steer"),
        "the kind crosses: {kinds:?}"
    );
    assert_eq!(
        kinds.first().map(String::as_str),
        Some("-"),
        "the plain send carried no kind"
    );

    // A picture and a file ride with a message: staged on this PC, they cross in ordered
    // chunks and reach the chat on the PC that runs it just as a local attachment would.
    let stage = b.db.path().parent().unwrap().join("mirror-attachments");
    std::fs::create_dir_all(&stage).unwrap();
    let img: Vec<u8> = (0..650_000u32).map(|i| (i % 251) as u8).collect();
    std::fs::write(stage.join("att-img"), &img).unwrap();
    std::fs::write(stage.join("att-text"), b"a long paste").unwrap();
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
        tx.execute(
            "INSERT INTO mirror_attachments(id, chat_id, kind, name, mime, target, bytes, created_at)
             VALUES ('att-text', 'c1', 'text', 'Pasted text.txt', 'text/plain', 'att-text', 12, 3)",
            [],
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
                "attachments": ["att-img", "att-file", "att-text"],
            }),
        )
        .await
        .unwrap();
    assert_eq!(with_files["submission"]["state"], "accepted");
    {
        let st = port.st();
        let files = st.sent_files.last().unwrap();
        assert_eq!(files.len(), 3);
        // A pasted text crosses as a plain-text file.
        assert_eq!(
            (files[2].kind.as_str(), files[2].mime.as_str()),
            ("file", "text/plain")
        );
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
    // A chip taken out is taken back on this PC too: the row and the saved picture go.
    std::fs::write(stage.join("att-gone"), b"x").unwrap();
    b.db.write(|tx| {
        tx.execute(
            "INSERT INTO mirror_attachments(id, chat_id, kind, name, mime, target, bytes, created_at)
             VALUES ('att-gone', 'c1', 'image', 'x.png', 'image/png', 'att-gone', 1, 1)",
            [],
        )
    })
    .unwrap();
    api.call_host(
        "devices_discard",
        json!({"chatId": "c1", "attachmentId": "att-gone"}),
    )
    .await
    .unwrap();
    assert_eq!(count(&b, "SELECT COUNT(*) FROM mirror_attachments"), 0);
    assert!(!stage.join("att-gone").exists());
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
    // Still the four that got through (hi, the attached one, the page's, the kinded one), no fifth.
    assert_eq!(port.st().sent.len(), 4);

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
    assert_eq!(port.st().sent.len(), 4);
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
    assert_eq!(port.st().sent.len(), 4);
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

    // The two-phase edit end to end. The staged resend crosses to the PC that runs the chat,
    // and the transcript rewinds there: the edited message and everything after it leave, and
    // the resent text stands in their place. The device's refresh news — what the chat's actor
    // emits once the edit is applied — is what makes the copy follow: it clears its rows and
    // re-reads from the start.
    api.call_host(
        "message_resend",
        json!({
            "chatId": "c1", "messageId": 1, "revision": 5,
            "submissionId": "7f0f2f0e-6b0e-4d52-8d3a-1f0e2d3c4b0a",
            "text": "hello again",
        }),
    )
    .await
    .unwrap();
    assert_eq!(port.st().edit_opens, [("c1".to_owned(), 1)]);
    assert_eq!(
        port.st().resends,
        [helm_devices::wire::ResendArgs {
            chat_id: "c1".to_owned(),
            message_id: 1,
            revision: 5,
            submission_id: "7f0f2f0e-6b0e-4d52-8d3a-1f0e2d3c4b0a".to_owned(),
            text: "hello again".to_owned(),
        }]
    );
    assert_eq!(port.st().deleted, ["c1"]);
    // The `/` menu asked the PC that runs the chat once, and the side chat was resolved
    // there, not here.
    assert_eq!(port.st().skills_asked, ["c1"]);
    assert_eq!(
        port.st().side_resolved,
        [helm_devices::wire::SideResolveArgs {
            chat_id: "c1".to_owned(),
            action: "discard".to_owned(),
            confirmed: None,
        }]
    );
    assert_eq!(
        port.st()
            .rows
            .get("c1")
            .map(|r| r.iter().map(|m| (m.id, m.text.clone())).collect::<Vec<_>>()),
        Some(vec![(6, "hello again".to_owned())]),
    );
    host.observe(&refresh());
    until(|| rows_of(&b, "c1") == vec![(6, "hello again".to_owned())]).await;
}

/// The hello on the production port, through the real op dispatcher: the catalogue op is
/// called the way the app calls it, and a failed catalogue leaves the hello standing without one.
#[tokio::test(start_paused = true)]
async fn hello_works_on_the_production_port_through_the_real_ops() {
    let dir = tempfile::tempdir().unwrap();
    let mut reg = MigrationRegistry::new();
    helm_devices::register_migrations(&mut reg);
    helm_chats::register_migrations(&mut reg);
    helm_pages::register_migrations(&mut reg);
    let db = Arc::new(Database::open(&dir.path().join("host.sqlite3"), &reg).unwrap());
    // The profiles table is the profiles crate's, read here directly: make just enough of it.
    db.write(|tx| {
        tx.execute_batch(
            "CREATE TABLE profiles (id TEXT PRIMARY KEY, hub_person_id TEXT, name TEXT, colour TEXT, about TEXT);
             INSERT INTO profiles VALUES ('p-a', 'person-a', 'Alex', 'red', '');
             INSERT INTO profiles VALUES ('p-b', 'person-b', 'Bo', 'blue', '');
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
    services.insert(Arc::new(helm_pages::PageServer::new(
        helm_panel::private::PrivateRoots::default(),
    )));
    services.insert(Arc::new(
        helm_core::profile::Profile::open(&dir.path().join("prof")).unwrap(),
    ));
    let mut ops = OpRegistry::new();
    helm_chats::register_ops(&mut ops);
    helm_panel::register_ops(&mut ops);
    ops.blocking("board_connection_get", OpClass::Blocking, |_, _| {
        Ok(json!({ "url": "http://127.0.0.1:9/secret-board", "hasToken": true }))
    });
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

    // The side panel around a chat that runs here, read from another PC: the files of a real
    // folder on disk, through the real panel ops. Everything stays inside the chat's project
    // folder; anything past it is refused in words, however the path is written.
    let work = dir.path().join("work");
    std::fs::create_dir_all(work.join("src")).unwrap();
    std::fs::write(work.join("src/main.ts"), "one\n").unwrap();
    let away = dir.path().join("away");
    std::fs::create_dir_all(&away).unwrap();
    std::fs::write(away.join("secret.txt"), "not yours").unwrap();
    db.write(|tx| {
        tx.execute(
            "INSERT INTO projects (id, kind, name, path, color, created_at)
             VALUES ('proj-w', 'folder', 'Repo', ?1, 'green', 0)",
            [work.to_string_lossy().to_string()],
        )?;
        tx.execute(
            "INSERT INTO chats (id, project_id, created_at, updated_at) VALUES ('panel-1', 'proj-w', 0, 0)",
            [],
        )?;
        Ok::<_, rusqlite::Error>(())
    })
    .unwrap();
    let inside = work.join("src/main.ts").to_string_lossy().into_owned();
    let out = away.join("secret.txt").to_string_lossy().into_owned();
    let view = |op: &'static str, args: serde_json::Value| {
        port.panel_read(helm_devices::serve::PanelRead { local_op: op, args })
    };
    let got = view("file_read", json!({ "chatId": "panel-1", "path": inside }))
        .await
        .unwrap();
    assert_eq!(
        (got["kind"].as_str(), got["text"].as_str()),
        (Some("text"), Some("one\n"))
    );
    let e = view(
        "file_read",
        json!({ "chatId": "panel-1", "path": out.clone() }),
    )
    .await
    .unwrap_err();
    assert_eq!(
        (e.code.as_str(), e.message.as_str()),
        ("refused", "That path is outside the chat's project folder.")
    );
    // The way out written into an inside path is refused the same way, and so is a change stamp
    // for a file outside the folder.
    let sneaky = format!(
        "{}{}..{}away{}secret.txt",
        work.to_string_lossy(),
        std::path::MAIN_SEPARATOR,
        std::path::MAIN_SEPARATOR,
        std::path::MAIN_SEPARATOR
    );
    let e = view("file_read", json!({ "chatId": "panel-1", "path": sneaky }))
        .await
        .unwrap_err();
    assert_eq!(e.code, "refused");
    let e = view(
        "file_stat",
        json!({ "chatId": "panel-1", "paths": [out.clone()] }),
    )
    .await
    .unwrap_err();
    assert_eq!(e.code, "refused");
    let workp = work.to_string_lossy().into_owned();
    assert_eq!(
        port.chat_folder("panel-1".to_owned()).await.unwrap()["path"]
            .as_str()
            .unwrap(),
        workp
    );
    // The board connection of this PC answers without its address: the watcher learns only
    // that a board is set up. (The board ops belong to a crate this test does not link, so
    // `board_connection_get` is stood in for — the address it hands back must not cross.)
    let conn = port.board_connection("panel-1".to_owned()).await.unwrap();
    assert_eq!(
        (conn["url"].as_str(), conn["hasToken"].as_bool()),
        (Some("board"), Some(true))
    );
    assert!(!conn.to_string().contains("9/secret-board"), "{conn}");
    // Quick Open: a watcher's search never names a file outside the chat's project folder.
    let q = port
        .quick_open(helm_devices::wire::QuickOpenArgs {
            chat_id: "panel-1".to_owned(),
            query: "main".to_owned(),
            reload: false,
        })
        .await
        .unwrap();
    assert!(q["items"]
        .as_array()
        .unwrap()
        .iter()
        .all(|it| !it["path"].as_str().unwrap_or_default().contains("away")));
    // A path-shaped mention answers nothing from afar: only the fuzzy search under the chat's
    // folder does.
    for query in ["C:\\", "~/", "../..", "./x", "/etc/", "a/b"] {
        let q = port
            .quick_open(helm_devices::wire::QuickOpenArgs {
                chat_id: "panel-1".to_owned(),
                query: query.to_owned(),
                reload: false,
            })
            .await
            .unwrap();
        assert_eq!(q["items"].as_array().unwrap().len(), 0, "{query}");
    }

    // A picture sent with a message is served to the watcher from this PC's store, for this
    // chat only.
    let mut png = b"\x89PNG\r\n\x1a\n\0\0\0\x0dIHDR".to_vec();
    png.extend(2u32.to_be_bytes());
    png.extend(2u32.to_be_bytes());
    png.extend([8, 6, 0, 0, 0]);
    let store = dir.path().join("prof").join("attachments");
    let staged = helm_chats::attachments::stage_image(&store, "a.png", "image/png", &png).unwrap();
    db.write(|tx| helm_chats::attachments::record_image(tx, "panel-1", &staged, 0))
        .unwrap();
    let seen = |chat: &str, thumb: Option<bool>| {
        port.attachment(helm_devices::wire::AttachmentGetArgs {
            chat_id: chat.to_owned(),
            attachment_id: staged.id.clone(),
            thumb,
        })
    };
    assert_eq!(seen("panel-1", None).await.unwrap()["mime"], "image/png");
    assert_eq!(seen("c-other", None).await.unwrap_err().code, "not_found");

    // Anything bigger than a frame is read in chunks from where the last one ended: a picture
    // of two megabytes in the chat's project folder arrives whole and in order, and the same
    // read of the message's picture works too. Outside the folder, or a file that holds keys,
    // is refused whatever the path says.
    use base64::Engine as _;
    let b64 = base64::engine::general_purpose::STANDARD;
    let big = helm_devices::testing::fake_picture(2 * 1024 * 1024);
    std::fs::write(work.join("big.png"), &big).unwrap();
    std::fs::write(work.join(".env"), "KEY=1").unwrap();
    let blob = |r: serde_json::Value, offset: u64| {
        port.blob_read(
            serde_json::from_value(json!({ "chatId": "panel-1", "ref": r, "offset": offset }))
                .unwrap(),
        )
    };
    let file = json!({ "file": work.join("big.png").to_string_lossy() });
    let mut joined = Vec::new();
    while joined.len() < big.len() {
        let got = blob(file.clone(), joined.len() as u64).await.unwrap();
        assert_eq!(
            (
                got["offset"].as_u64(),
                got["total"].as_u64(),
                got["mime"].as_str()
            ),
            (
                Some(joined.len() as u64),
                Some(big.len() as u64),
                Some("image/png")
            )
        );
        joined.extend(b64.decode(got["data"].as_str().unwrap()).unwrap());
    }
    assert_eq!(joined, big);
    let sent = blob(json!({ "attachment": staged.id }), 0).await.unwrap();
    assert_eq!(sent["total"].as_u64(), Some(png.len() as u64));
    for (bad, want) in [
        (json!({ "file": out.clone() }), "refused"),
        (json!({ "file": sneaky.clone() }), "refused"),
        (
            json!({ "file": work.join(".env").to_string_lossy() }),
            "refused",
        ),
        (
            json!({ "file": work.join("src").to_string_lossy() }),
            "bad_args",
        ),
        (json!({ "output": 999 }), "not_found"),
    ] {
        assert_eq!(blob(bad.clone(), 0).await.unwrap_err().code, want, "{bad}");
    }

    // Pinned pages cross to a PC for the person they are filed under, and only inside the page's
    // own folder: the document and the files it names, a chunk at a time. The PC's own person
    // also sees the pages pinned before profiles; a pin of someone else is not found.
    std::fs::write(
        work.join("plan.html"),
        "<img src=\"pic.png\"><link href=\"style.css\" rel=stylesheet>",
    )
    .unwrap();
    std::fs::write(work.join("pic.png"), [9u8; 5]).unwrap();
    std::fs::write(work.join("style.css"), "a{}").unwrap();
    std::fs::write(work.join("unused.png"), [1u8]).unwrap();
    std::fs::write(away.join("secret.png"), [2u8]).unwrap();
    let plan = work.join("plan.html").to_string_lossy().into_owned();
    db.write(|tx| {
        for (id, profile, title) in [
            ("pg-a", "p-a", "Plan"),
            ("pg-b", "p-b", "Theirs"),
            ("pg-old", "", "Before profiles"),
        ] {
            helm_pages::store::pin(
                tx,
                id,
                profile,
                &format!("{id}-key"),
                &plan,
                Some(title),
                Some("panel-1"),
                1,
            )
            .unwrap();
        }
        Ok::<_, rusqlite::Error>(())
    })
    .unwrap();
    let who = |person: &str| helm_devices::serve::PageScope {
        person_id: person.to_owned(),
    };
    let titles = |v: serde_json::Value| -> Vec<String> {
        let mut t: Vec<String> = v["pages"]
            .as_array()
            .unwrap()
            .iter()
            .map(|p| p["title"].as_str().unwrap().to_owned())
            .collect();
        t.sort();
        t
    };
    // Each person lists the pages pinned under their own profiles and those pinned before
    // profiles, never another person's; a pin still opens by its id, so a link to it works.
    for (person, want) in [
        ("person-a", ["Before profiles", "Plan"]),
        ("person-b", ["Before profiles", "Theirs"]),
    ] {
        assert_eq!(
            titles(port.pages_list(who(person)).await.unwrap()),
            want,
            "{person}"
        );
    }
    let page = |id: &str| helm_devices::wire::PageIdArgs {
        page_id: id.to_owned(),
    };
    let files = port
        .pages_files(who("person-a"), page("pg-a"))
        .await
        .unwrap();
    let names: Vec<&str> = files["files"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["path"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["pic.png", "style.css"]);
    assert!(port
        .pages_files(who("person-b"), page("pg-a"))
        .await
        .is_ok());
    let read = |id: &str, path: Option<&str>, offset: u64| {
        port.pages_read(
            who("person-a"),
            helm_devices::wire::PagesReadArgs {
                page_id: id.to_owned(),
                path: path.map(str::to_owned),
                offset,
            },
        )
    };
    let got = read("pg-a", Some("pic.png"), 3).await.unwrap();
    assert_eq!(
        (got["size"].as_u64(), got["eof"].as_bool()),
        (Some(5), Some(true))
    );
    for bad in [
        "unused.png",
        "unused.html",
        "../away/secret.png",
        "plan.html",
        "src/../pic.png",
    ] {
        assert!(read("pg-a", Some(bad), 0).await.is_err(), "{bad}");
    }
    assert!(read("pg-b", None, 0).await.is_ok());
    assert_eq!(
        read("pg-none", None, 0).await.unwrap_err().code,
        "not_found"
    );

    // A board is a place to start a chat from afar: the chat runs in the folder the board starts
    // its chats in and is shown on the board. The projects crate's tables and the ops that use
    // them are stood in for, so what the host asks of them can be read back.
    db.write(|tx| {
        tx.execute_batch(
            "CREATE TABLE project_local (
                 project_id TEXT PRIMARY KEY,
                 archived INTEGER NOT NULL DEFAULT 0, closed INTEGER NOT NULL DEFAULT 0,
                 primary_folder_project_id TEXT, revision INTEGER NOT NULL DEFAULT 0);
             CREATE TABLE project_pins (
                 project_id TEXT NOT NULL, profile_id TEXT NOT NULL, sort INTEGER,
                 pinned_at INTEGER NOT NULL DEFAULT 0, PRIMARY KEY (project_id, profile_id));
             CREATE TABLE board_projects (project_id TEXT PRIMARY KEY, stale INTEGER NOT NULL DEFAULT 0);
             INSERT INTO board_projects(project_id) VALUES ('brd-1');
             INSERT INTO project_local(project_id, primary_folder_project_id, revision)
             VALUES ('brd-1', 'proj-w', 4);
             INSERT INTO projects (id, kind, name, path, color, created_at)
             VALUES ('brd-2', 'board', 'No folder yet', NULL, '', 0);
             INSERT INTO board_projects(project_id) VALUES ('brd-2');",
        )
    })
    .unwrap();
    let calls = Arc::new(Mutex::new(Vec::<(String, Value)>::new()));
    let mut stand_in = OpRegistry::new();
    for name in [
        "chat_create",
        "profiles_assign_chats",
        "project_update",
        "board_project_create",
        "project_create_folder",
    ] {
        let calls = calls.clone();
        let op = name.to_owned();
        stand_in.blocking(name, OpClass::Blocking, move |_, args| {
            calls.lock().unwrap().push((op.clone(), args));
            Ok(json!({ "id": "made-1" }))
        });
    }
    let board_on = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let on = board_on.clone();
    stand_in.blocking("board_connection_get", OpClass::Blocking, move |_, _| {
        let set = on.load(std::sync::atomic::Ordering::SeqCst);
        Ok(json!({ "url": if set { "board" } else { "" }, "hasToken": set }))
    });
    let remote = HostPort::new(Arc::new(Dispatcher::new(
        stand_in,
        OpCtx {
            services: Arc::new({
                let mut s = Services::new();
                s.insert(db.clone());
                s
            }),
            events: Arc::new(RecordingSink::default()),
        },
        false,
    )));
    let create = |project: &str| helm_devices::wire::CreateArgs {
        person_id: "person-a".into(),
        project_id: project.into(),
        model: None,
        effort: None,
        title: None,
        muted: None,
    };
    assert_eq!(
        remote.create(create("brd-1")).await.unwrap()["chatId"],
        "made-1"
    );
    let first = calls.lock().unwrap().remove(0);
    assert_eq!(first.0, "chat_create");
    assert_eq!(first.1["cwd"], workp);
    assert_eq!(first.1["projectId"], "brd-1");
    calls.lock().unwrap().clear();
    // A board with no folder yet says so in words instead of starting a chat nowhere.
    let e = remote.create(create("brd-2")).await.unwrap_err();
    assert!(e.message.contains("folder"), "{}", e.message);

    // A new project from afar is a board on the task board when one is connected, with the
    // folder asked for, and a folder when none is; its id and kind come back.
    use helm_devices::wire::{ProjectCreateArgs, ProjectSetArgs};
    let new_project = |title: &str, folder: Option<&str>| ProjectCreateArgs {
        title: title.into(),
        description: Some("About".into()),
        folder_project_id: folder.map(str::to_owned),
    };
    let made = remote
        .project_create(new_project(" Ideas ", None))
        .await
        .unwrap();
    assert_eq!(made, json!({"projectId": "made-1", "kind": "folder"}));
    assert_eq!(
        calls.lock().unwrap().remove(0),
        ("project_create_folder".to_owned(), json!({"name": "Ideas"}))
    );
    board_on.store(true, std::sync::atomic::Ordering::SeqCst);
    remote
        .project_create(new_project("Ideas", Some("proj-w")))
        .await
        .unwrap();
    assert_eq!(
        calls.lock().unwrap().remove(0),
        (
            "board_project_create".to_owned(),
            json!({"title": "Ideas", "description": "About", "folder": workp})
        )
    );
    // A folder that is not one of this PC's, or a nameless project, is refused in words.
    for bad in [new_project("Ideas", Some("brd-1")), new_project("  ", None)] {
        assert!(remote.project_create(bad).await.is_err());
    }
    // Pinning and choosing a board's folder go to the projects op with the project's current
    // revision; anything else is refused before it gets there.
    let set = |id: &str, pinned: Option<bool>, folder: Option<&str>| ProjectSetArgs {
        project_id: id.into(),
        pinned,
        folder_project_id: folder.map(str::to_owned),
    };
    remote
        .project_set(set("brd-2", Some(true), Some("proj-w")))
        .await
        .unwrap();
    assert_eq!(
        calls.lock().unwrap().remove(0),
        (
            "project_update".to_owned(),
            json!({"projectId": "brd-2", "revision": 0, "pinned": true, "primaryFolderProjectId": "proj-w"})
        )
    );
    remote
        .project_set(set("brd-1", Some(false), None))
        .await
        .unwrap();
    assert_eq!(calls.lock().unwrap().remove(0).1["revision"], 4);
    for bad in [
        set("brd-1", None, None),
        set("proj-w", None, Some("proj-1")),
        set("brd-1", None, Some("brd-2")),
        set("nope", Some(true), None),
    ] {
        assert!(remote.project_set(bad).await.is_err());
    }
    assert!(calls.lock().unwrap().is_empty());

    // Another person of the household does the same as the PC's own person: a new project is a
    // board when a task board is connected, a board that already has a folder can be pointed at
    // another, and pinning works.
    remote
        .project_create(new_project("Sam's", Some("proj-w")))
        .await
        .unwrap();
    assert_eq!(
        calls.lock().unwrap().remove(0),
        (
            "board_project_create".to_owned(),
            json!({"title": "Sam's", "description": "About", "folder": workp})
        )
    );
    remote
        .project_set(set("brd-1", None, Some("proj-w")))
        .await
        .unwrap();
    assert_eq!(
        calls.lock().unwrap().remove(0),
        (
            "project_update".to_owned(),
            json!({"projectId": "brd-1", "revision": 4, "primaryFolderProjectId": "proj-w"})
        )
    );
    remote
        .project_set(set("brd-2", Some(true), Some("proj-w")))
        .await
        .unwrap();
    remote
        .project_set(set("brd-1", Some(true), None))
        .await
        .unwrap();
    assert_eq!(calls.lock().unwrap().len(), 2);
    calls.lock().unwrap().clear();

    // Each person's hello names their own pins and the ones made before profiles, never
    // another person's; a pin made from afar is asked for under the asking person's profile.
    db.write(|tx| {
        tx.execute_batch(
            "INSERT INTO project_pins(project_id, profile_id) VALUES ('proj-w', '');
             INSERT INTO project_pins(project_id, profile_id) VALUES ('brd-1', 'p-b');
             INSERT INTO project_pins(project_id, profile_id) VALUES ('brd-2', 'p-a');",
        )
    })
    .unwrap();
    let pins = |h: helm_devices::wire::Hello| {
        let mut v: Vec<String> = h
            .projects
            .into_iter()
            .filter(|p| p.pinned)
            .map(|p| p.id)
            .collect();
        v.sort();
        v
    };
    for (person, want) in [
        ("person-a", vec!["brd-2", "proj-w"]),
        ("person-b", vec!["brd-1", "proj-w"]),
        ("person-x", vec!["proj-w"]),
        ("", vec!["brd-1", "brd-2", "proj-w"]),
    ] {
        let got = pins(remote.hello_as(person.into()).await.unwrap());
        assert_eq!(got, want, "{person}");
    }
    // The ask names the asking person's profile (none for a person with no profile here), and the
    // projects op files the pin or takes it off for that profile alone.
    for (person, on, profile) in [
        ("person-b", true, Some("p-b")),
        ("person-a", false, Some("p-a")),
        ("person-x", true, None),
    ] {
        remote
            .project_set_as(person.into(), set("brd-2", Some(on), None))
            .await
            .unwrap();
        let mut want = json!({"projectId": "brd-2", "revision": 0, "pinned": on});
        if let Some(profile) = profile {
            want["pinProfileId"] = json!(profile);
        }
        assert_eq!(
            calls.lock().unwrap().remove(0),
            ("project_update".to_owned(), want),
            "{person}"
        );
    }
    // Once the op has filed person-b's pin, both people have it and neither lost theirs.
    db.write(|tx| {
        tx.execute_batch("INSERT INTO project_pins(project_id, profile_id) VALUES ('brd-2', 'p-b')")
    })
    .unwrap();
    let got = pins(remote.hello_as("person-b".into()).await.unwrap());
    assert_eq!(got, ["brd-1", "brd-2", "proj-w"]);
    let got = pins(remote.hello_as("person-a".into()).await.unwrap());
    assert_eq!(got, ["brd-2", "proj-w"]);
    calls.lock().unwrap().clear();

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
async fn a_household_member_reads_the_whole_list_and_is_refused_nothing() {
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

    // The list holds every chat of the PC for her as for the owner's device.
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
    assert_eq!(ids(list.unwrap()), ["mine", "hers", "shared"]);
    let list = own
        .rpc
        .call("dev-a", "chats_sub", json!({"since": 0}))
        .await;
    assert_eq!(ids(list.unwrap()), ["mine", "hers", "shared"]);
    // A change to any chat reaches her as it reaches the owner's device.
    host.observe(
        &ChatEvent::Chat {
            chat: Box::new(fake_record("mine", "Renamed", 13)),
        }
        .to_app_event(),
    );
    host.observe(
        &ChatEvent::Chat {
            chat: Box::new(fake_record("shared", "Renamed too", 14)),
        }
        .to_app_event(),
    );
    until(|| own.heard("chats").len() == 2 && sam.heard("chats").len() == 2).await;
    settle().await;
    let kinds = |w: &Watcher| -> Vec<String> {
        w.heard("chats")
            .iter()
            .map(|p| p.event["kind"].as_str().unwrap().to_owned())
            .collect()
    };
    assert_eq!(kinds(&sam), kinds(&own));
    assert_eq!(kinds(&sam), ["chat", "chat"]);
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
    let resend = |chat: &str, text: &str| {
        json!({"chatId": chat, "messageId": 1, "revision": 1,
               "submissionId": "7f0f2f0e-6b0e-4d52-8d3a-1f0e2d3c4b99", "text": text})
    };
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
        ("project_create", json!({"title": "Sam's project"}), None),
        (
            "project_set",
            json!({"projectId": "proj-1", "pinned": true}),
            None,
        ),
        // Unknown fields are refused, as for every op.
        (
            "project_set",
            json!({"projectId": "proj-1", "path": "/etc"}),
            Some(code::BAD_ARGS),
        ),
        // Erasing a chat for good is the one exception: her own chat she may delete from
        // afar, another person's not — even when it is shared with her.
        ("delete", json!({"chatId": "hers"}), None),
        ("delete", json!({"chatId": "mine"}), Some(code::REFUSED)),
        ("delete", json!({"chatId": "shared"}), Some(code::REFUSED)),
        // A side chat's promote or discard is the owner's too: it shows a hidden chat in
        // that PC's sidebar, or erases it for good.
        (
            "side_resolve",
            json!({"chatId": "hers", "action": "discard", "confirmed": true}),
            None,
        ),
        (
            "side_resolve",
            json!({"chatId": "mine", "action": "discard", "confirmed": true}),
            Some(code::REFUSED),
        ),
        (
            "side_resolve",
            json!({"chatId": "shared", "action": "promote"}),
            Some(code::REFUSED),
        ),
        // An action that is neither of the two is refused before anything is asked.
        (
            "side_resolve",
            json!({"chatId": "hers", "action": "delete"}),
            Some(code::BAD_ARGS),
        ),
        // Rewriting a sent message rewinds the chat, so it is for the chat's owner and those it
        // is shared with; opening one for edit is the same door.
        ("resend", resend("hers", "edit hers"), None),
        ("resend", resend("shared", "edit shared"), None),
        ("resend", resend("mine", "edit mine"), Some(code::REFUSED)),
        (
            "edit_open",
            json!({"chatId": "mine", "messageId": 1}),
            Some(code::REFUSED),
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
        // What she resends: plain on her own chat, marked as hers on one shared with her, and
        // never on a chat that is neither.
        assert_eq!(st.resends.len(), 2);
        assert_eq!(st.resends[0].text, "edit hers");
        assert!(
            st.resends[1]
                .text
                .starts_with("[Message from Sam (household)")
                && st.resends[1].text.ends_with("edit shared"),
            "{}",
            st.resends[1].text
        );
        // Her project ops are served like anyone's.
        assert_eq!(st.projects_created.len(), 1);
        assert_eq!(st.projects_set.len(), 1);
    }
    // She can hand any chat to a profile or change its sharing, as the PC's own person can;
    // only the new-chat defaults (they include the default permission mode) stay the PC's own
    // person's.
    port.add_chat_of("his", "Pat's", 16, "person-a", &[]);
    for (op, args, ok) in [
        (
            "move_profile",
            json!({"chatId": "his", "profileId": "p"}),
            true,
        ),
        (
            "share",
            json!({"chatId": "his", "personId": "person-b", "shared": true}),
            true,
        ),
        (
            "move_profile",
            json!({"chatId": "hers", "profileId": "p"}),
            true,
        ),
        // A chat merely shared with her: she can leave it, move it or change its sharing.
        (
            "share",
            json!({"chatId": "shared", "personId": "person-b", "shared": false}),
            true,
        ),
        (
            "share",
            json!({"chatId": "shared", "personId": "person-a", "shared": false}),
            true,
        ),
        (
            "move_profile",
            json!({"chatId": "shared", "profileId": "p"}),
            true,
        ),
        (
            "defaults_set",
            json!({"chatId": "hers", "model": "opus"}),
            false,
        ),
    ] {
        let got = sam.rpc.call("dev-a", op, args).await;
        match ok {
            true => assert!(got.is_ok(), "{op}: {got:?}"),
            false => assert_eq!(got.unwrap_err().code, code::NOT_ALLOWED, "{op}"),
        }
    }
    assert_eq!(port.st().profiles_moved.len(), 3);

    // A card for a change Helm applies itself (settings, pairing, the update folder) can only
    // be approved by this PC's own person; a plain card is answerable by anyone with the chat.
    for (id, applying) in [("ap1", true), ("ap2", true), ("pl1", false)] {
        port.put_consent(
            "hers",
            json!({"id": id, "kind": "k", "label": "L", "summary": "s", "state": "pending",
                   "grantMinutes": 30, "applying": applying}),
        );
    }
    let decide = |id: &str| json!({"chatId": "hers", "requestId": id, "decision": "allow"});
    let e = sam
        .rpc
        .call("dev-a", "consent_decide", decide("ap1"))
        .await
        .unwrap_err();
    assert_eq!(e.code, code::NOT_ALLOWED);
    assert!(own
        .rpc
        .call("dev-a", "consent_decide", decide("ap2"))
        .await
        .is_ok());
    assert!(sam
        .rpc
        .call("dev-a", "consent_decide", decide("pl1"))
        .await
        .is_ok());

    // The switch for other devices gates a household member's run ops too: with it off, every op
    // that runs or changes a chat is refused, card answers and uploads included; reads stay.
    port.set_run(false);
    for (op, args) in [
        (
            "consent_decide",
            json!({"chatId": "hers", "requestId": "r", "decision": "allow"}),
        ),
        ("send", send("hers", 9)),
        (
            "attach_put",
            json!({"chatId": "hers", "uploadId": "7f0f2f0e-6b0e-4d52-8d3a-1f0e2d3c4b99", "seq": 0, "eof": true, "data": "", "kind": "file", "name": "a.txt", "mime": ""}),
        ),
        ("stop", json!({"chatId": "hers"})),
    ] {
        let e = sam.rpc.call("dev-a", op, args).await.unwrap_err();
        assert_eq!(e.code, code::NOT_ALLOWED, "{op}");
    }
    assert!(sam.rpc.call("dev-a", "hello", json!({})).await.is_ok());
    port.set_run(true);

    // Uploads belong to the device that began them, and one device may hold only so much.
    use base64::Engine as _;
    let chunk = base64::engine::general_purpose::STANDARD.encode(vec![7u8; 640 * 1024]);
    let part = |id: &str, seq: u32, eof: bool| {
        json!({"chatId": "hers", "uploadId": id, "seq": seq, "eof": eof, "data": chunk,
               "kind": "file", "name": "a.bin", "mime": ""})
    };
    let (u1, u2, u3) = (
        "7f0f2f0e-6b0e-4d52-8d3a-1f0e2d3c4b01",
        "7f0f2f0e-6b0e-4d52-8d3a-1f0e2d3c4b02",
        "7f0f2f0e-6b0e-4d52-8d3a-1f0e2d3c4b03",
    );
    sam.rpc
        .call("dev-a", "attach_put", part(u1, 0, false))
        .await
        .unwrap();
    let e = own
        .rpc
        .call("dev-a", "attach_put", part(u1, 1, false))
        .await
        .unwrap_err();
    assert_eq!(e.code, code::REFUSED);
    for id in [u1, u2] {
        let start = if id == u1 { 1 } else { 0 };
        for seq in start..40 {
            sam.rpc
                .call("dev-a", "attach_put", part(id, seq, seq == 39))
                .await
                .unwrap();
        }
    }
    let e = sam
        .rpc
        .call("dev-a", "attach_put", part(u3, 0, true))
        .await
        .unwrap_err();
    assert_eq!(e.code, code::REFUSED);
    own.rpc
        .call("dev-a", "attach_put", part(u3, 0, true))
        .await
        .unwrap();
    // A person this PC has no profile for is still served.
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
    port.announce_people(&[
        ("person-p", true, true, true),
        ("person-s", false, false, false),
    ]);
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
        &[
            ("person-s", true, true, true),
            ("person-p", false, false, false),
        ],
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
    // `chat_organize` is registered by a crate this test does not link. The name must be
    // known for the interceptor to answer it; the handler must never run for a copied chat.
    ops.blocking("chat_organize", OpClass::Maintenance, |_, _| {
        Err(ApiError::internal(
            "the local organize handler ran for a copied chat",
        ))
    });
    // Some rows below call ops registered by crates this test does not link. The names must
    // be known for the interceptor to answer them; none may run locally for a copied chat.
    for name in [
        "chat_verbosity_get",
        "chat_verbosity_set",
        "panel_context_get",
        "quick_open",
        "chat_folder",
        "file_read",
        "dir_list",
        "file_stat",
        "archive_list",
        "path_resolve",
        "changes",
        "diff",
        "changes_stage",
        "changes_unstage",
    ] {
        ops.blocking(name, OpClass::Blocking, move |_, _| {
            Err(ApiError::internal(
                "the local handler ran for a copied chat",
            ))
        });
    }
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

    // The new-chat op works from afar as the window uses it: the chat starts on the desk for
    // the profile the window shows, the desk's record of it is held in the copy at once, and
    // its first words cross the link without waiting for the desk's next list news.
    let made = api
        .call_host(
            "devices_create_chat",
            json!({"deviceUid": "dev-d", "projectId": "proj-1"}),
        )
        .await
        .unwrap();
    assert_eq!(made["chatId"], "new-2", "{made}");
    assert!(
        b.mirror.owner("new-2").is_some(),
        "the copy missed the new chat"
    );
    api.call_host(
        "chat_send",
        json!({"chatId": "new-2", "submissionId": "7f0f2f0e-6b0e-4d52-8d3a-1f0e2d3c4b03",
               "text": "First words across the link", "action": "steer", "draftRevision": 0}),
    )
    .await
    .unwrap();
    {
        let st = port.st();
        let c = st.created.last().unwrap();
        assert_eq!(c.person_id, "person-p");
        assert_eq!(c.project_id, "proj-1");
        assert!(
            st.sent
                .iter()
                .any(|(chat, _, text)| chat == "new-2" && text == "First words across the link"),
            "{:?}",
            st.sent
        );
    }

    // A PC that never announced Pat cannot act for them: the hub turns the call away and
    // nothing reaches the desk.
    let x = Watcher::of(&router, "pc-x", "Gale", true, "person-x", "Gale");
    let e = x
        .rpc
        .call_as("dev-d", "hello", json!({}), &["person-p".to_owned()])
        .await
        .unwrap_err();
    assert_eq!(e.code, code::NOT_ALLOWED);
    assert_eq!(port.st().created.len(), 2);
}

/// The copy of another PC holds every chat of that PC from the first sync, whoever the laptop
/// acts for; a profile linked later changes nothing about which chats arrive, and the hub line
/// does not drop.
#[tokio::test(start_paused = true)]
async fn the_copy_holds_every_chat_of_the_pc_whoever_the_laptop_acts_for() {
    let router = MemRouter::new();
    let port = Arc::new(FakePort::new("dev-d", "Desk"));
    port.st().people = vec!["person-p".to_owned(), "person-s".to_owned()];
    port.add_chat_of("own-s", "Sam's chat", 10, "person-s", &[]);
    port.add_chat_of("own-t", "Tia's chat", 11, "person-t", &[]);
    let link_d = router.connect("dev-d", Some("person-p"), Some("Pat"), true);
    let host = Service::new(port.clone(), link_d, tokio::runtime::Handle::current());
    host.announce();
    let b = side(
        &router,
        "dev-l",
        "Laptop",
        None,
        "person-s",
        "Sam",
        &[("person-s", true, true, true)],
    );
    until(|| b.mirror.owner("own-s").is_some() && b.mirror.owner("own-t").is_some()).await;

    // The laptop now announces Tia as well; the tap hears the profile change, and the copy
    // still holds both chats.
    b.link
        .set_people(vec!["person-s".to_owned(), "person-t".to_owned()]);
    b.link.send(Frame::Device(DeviceFrame {
        uid: "dev-l".to_owned(),
        name: "Laptop".to_owned(),
        run: true,
        people: ["person-s", "person-t"]
            .iter()
            .map(|p| DevicePerson {
                person_id: (*p).to_owned(),
                telegram: false,
                voice: false,
                speaker: false,
            })
            .collect(),
    }));
    settle().await;
    let tap = helm_devices::EventTap::default();
    tap.attach_mirror(b.mirror.clone());
    tap.observe(&AppEvent::new("profiles_changed"));
    settle().await;
    assert!(b.mirror.owner("own-s").is_some() && b.mirror.owner("own-t").is_some());
}

/// A server on the host's loopback, opened from the other PC's: the window's ops on the viewing
/// PC, the person and switch checks on the host, and the end when the host drops off the hub.
/// Real sockets, so real time.
#[tokio::test]
async fn a_port_of_one_pc_opens_on_another_and_closes_when_that_pc_goes() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let echo = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let echo_port = echo.local_addr().unwrap().port();
    tokio::spawn(async move {
        while let Ok((mut s, _)) = echo.accept().await {
            tokio::spawn(async move {
                let (mut r, mut w) = s.split();
                let _ = tokio::io::copy(&mut r, &mut w).await;
            });
        }
    });

    let router = MemRouter::new();
    let port = Arc::new(FakePort::new("dev-a", "Desk"));
    port.add_chat("c1", "First", 10);
    port.add_chat_of("c2", "Theirs", 11, "person-b", &[]);
    let link_a = router.connect("dev-a", Some("person-a"), Some("Alex"), true);
    let host = Service::new(
        port.clone(),
        link_a.clone(),
        tokio::runtime::Handle::current(),
    );
    host.announce();
    let b = side(&router, "dev-b", "Laptop", None, "person-a", "Alex", &[]);
    let viewer = Service::new(
        Arc::new(FakePort::new("dev-b", "Laptop")),
        b.link.clone(),
        tokio::runtime::Handle::current(),
    );
    let changes = Arc::new(Mutex::new(0usize));
    let seen = changes.clone();
    viewer.on_forwards_changed(move || *seen.lock().unwrap() += 1);
    let mut services = Services::new();
    services.insert(b.db.clone());
    let tap = Arc::new(helm_devices::EventTap::default());
    tap.attach(viewer.clone());
    services.insert(tap);
    let slot = Arc::new(MirrorSlot::default());
    slot.attach(b.mirror.clone());
    services.insert(slot);
    let mut ops = OpRegistry::new();
    helm_devices::register_ops(&mut ops);
    let api = Dispatcher::new(
        ops,
        OpCtx {
            services: Arc::new(services),
            events: b.sink.clone(),
        },
        false,
    );
    until(|| b.mirror.owner("c1").is_some()).await;

    // Opened from the chat: the echo server's port is taken on this machine, so the forward
    // listens on another free port here and says which.
    let open = json!({"chatId": "c1", "port": echo_port, "title": "Preview"});
    let f = api
        .call_host("devices_forward_open", open.clone())
        .await
        .unwrap();
    assert_eq!(
        (
            f["deviceName"].as_str(),
            f["port"].as_u64(),
            f["title"].as_str()
        ),
        (Some("Desk"), Some(echo_port as u64), Some("Preview"))
    );
    let local = f["localPort"].as_u64().unwrap() as u16;
    assert_ne!(local, echo_port);
    let mut sock = tokio::net::TcpStream::connect(("127.0.0.1", local))
        .await
        .unwrap();
    sock.write_all(b"ping").await.unwrap();
    let mut back = [0u8; 4];
    sock.read_exact(&mut back).await.unwrap();
    assert_eq!(&back, b"ping");
    // Asked again, the same forward answers.
    let again = api.call_host("devices_forward_open", open).await.unwrap();
    assert_eq!(again["forwardId"], f["forwardId"]);
    let listed = api.call_host("devices_forwards", json!({})).await.unwrap();
    assert_eq!(listed["forwards"].as_array().unwrap().len(), 1);
    assert_eq!(*changes.lock().unwrap(), 1);

    // On the host: another person may forward from their own chat but not from this PC's
    // person's, and nothing is forwarded with the switch off.
    let tablet = Watcher::of(&router, "dev-c", "Tablet", true, "person-b", "Blake");
    settle().await;
    let e = tablet
        .rpc
        .call(
            "dev-a",
            "forward_open",
            json!({"chatId": "c1", "port": echo_port}),
        )
        .await
        .unwrap_err();
    assert_eq!(e.code, code::REFUSED);
    let theirs = tablet
        .rpc
        .call(
            "dev-a",
            "forward_open",
            json!({"chatId": "c2", "port": echo_port}),
        )
        .await
        .unwrap();
    tablet
        .rpc
        .call(
            "dev-a",
            "forward_close",
            json!({"forwardId": theirs["forwardId"]}),
        )
        .await
        .unwrap();
    port.set_run(false);
    let e = tablet
        .rpc
        .call(
            "dev-a",
            "forward_open",
            json!({"chatId": "c2", "port": echo_port}),
        )
        .await
        .unwrap_err();
    assert_eq!(e.code, code::NOT_ALLOWED);
    port.set_run(true);

    // The host drops off the hub: the forward ends here, its connection closes and the window
    // hears it.
    link_a.set_up(false);
    let mut rest = Vec::new();
    let read = tokio::time::timeout(Duration::from_secs(5), sock.read_to_end(&mut rest)).await;
    assert!(read.is_ok(), "the connection stayed open");
    until(|| *changes.lock().unwrap() == 2).await;
    let listed = api.call_host("devices_forwards", json!({})).await.unwrap();
    assert_eq!(listed["forwards"], json!([]));
}

/// Settings → MCP's "Copy from another PC": the other PC answers with its own export — every
/// secret value, saved secret id and credential-file path blanked — and this PC saves what
/// arrives. A second copy of the same list skips the ids already kept; a household member may
/// not read another PC's tool servers at all.
#[tokio::test(start_paused = true)]
async fn mcp_servers_copy_across_without_their_secrets() {
    fn store(dir: &std::path::Path) -> helm_mcp::config::McpStore {
        let creds = CredentialStore::new(
            &dir.join("credentials"),
            "testhash",
            Arc::new(helm_platform::keychain::MemoryKeychain::default()),
        );
        helm_mcp::config::McpStore::new(dir, creds)
    }
    let router = MemRouter::new();
    let port = Arc::new(FakePort::new("dev-a", "Desk"));
    let link_a = router.connect("dev-a", Some("person-a"), Some("Alex"), true);
    let host = Service::new(port.clone(), link_a, tokio::runtime::Handle::current());
    host.announce();
    settle().await;

    // The serving PC's real store, with each secret kind once, exported by the MCP crate's own
    // copy export: what the wire carries is exactly what a real Helm answers its person's
    // other PC with.
    let src = tempfile::tempdir().unwrap();
    let src_store = store(src.path());
    let tokfile = src.path().join("tok.txt");
    std::fs::write(&tokfile, "file-token\n").unwrap();
    use helm_mcp::types as mt;
    let mut headers = std::collections::BTreeMap::new();
    headers.insert(
        "Authorization".to_string(),
        mt::ValueRef {
            profiles: Default::default(),
            kind: mt::RefKind::Literal,
            value: Some("topsecret-token".into()),
            secret_id: None,
            prefix: Some("Bearer ".into()),
        },
    );
    let mut env = std::collections::BTreeMap::new();
    env.insert(
        "SECRET_LIT".to_string(),
        mt::ValueRef {
            profiles: Default::default(),
            kind: mt::RefKind::Literal,
            value: Some("envsecret".into()),
            secret_id: None,
            prefix: None,
        },
    );
    env.insert(
        "VAR_NAME".to_string(),
        mt::ValueRef {
            profiles: Default::default(),
            kind: mt::RefKind::Environment,
            value: Some("KEEP_ME".into()),
            secret_id: None,
            prefix: None,
        },
    );
    env.insert(
        "TOKFILE".to_string(),
        mt::ValueRef {
            profiles: Default::default(),
            kind: mt::RefKind::File,
            value: Some(tokfile.to_string_lossy().into_owned()),
            secret_id: None,
            prefix: None,
        },
    );
    src_store
        .mutate(None, |l| {
            l.push(mt::McpServer {
                id: "hub".into(),
                name: "Hub".into(),
                enabled: true,
                transport: mt::Transport {
                    kind: mt::TransportKind::Http,
                    command: None,
                    args: vec![],
                    env: std::collections::BTreeMap::new(),
                    url: Some("https://hub.example/mcp".into()),
                    headers,
                    oauth: None,
                },
                allow_private_http: false,
                preset: None,
            });
            l.push(mt::McpServer {
                id: "prog".into(),
                name: "Program".into(),
                enabled: true,
                transport: mt::Transport {
                    kind: mt::TransportKind::Stdio,
                    command: Some("node.exe".into()),
                    args: vec!["server.js".into()],
                    env,
                    url: None,
                    headers: std::collections::BTreeMap::new(),
                    oauth: None,
                },
                allow_private_http: false,
                preset: None,
            });
            Ok(())
        })
        .unwrap();
    port.st().mcp_servers = src_store
        .export_copy()
        .unwrap()
        .iter()
        .map(|s| serde_json::to_value(s).unwrap())
        .collect();

    // The watcher side: its own database, mirror and MCP store, with the window's ops.
    let w = side(&router, "dev-b", "Laptop", None, "person-a", "Alex", &[]);
    // Give the mirror the hub's list (its own device frame starts the broadcast) before it
    // routes anything to the other PC.
    settle().await;
    let dst = tempfile::tempdir().unwrap();
    let dst_store = store(dst.path());
    let mut services = Services::new();
    services.insert(w.db.clone());
    let slot = Arc::new(MirrorSlot::default());
    slot.attach(w.mirror.clone());
    services.insert(slot);
    services.insert(Arc::new(dst_store));
    let mut ops = OpRegistry::new();
    helm_devices::register_ops(&mut ops);
    helm_mcp::register_ops(&mut ops);
    let api = Dispatcher::new(
        ops,
        OpCtx {
            services: Arc::new(services),
            events: w.sink.clone(),
        },
        false,
    );

    let listed = api
        .call_host("devices_mcp_list", json!({"deviceUid": "dev-a"}))
        .await
        .unwrap();
    assert_eq!(listed["servers"].as_array().unwrap().len(), 2);
    assert_eq!(port.st().mcp_list_asks, 1);
    // The copy arrives with no secret of the other PC: no value, no saved id, no file path.
    let tokpath = tokfile.to_string_lossy().into_owned();
    let wire = serde_json::to_string(&listed).unwrap();
    for gone in [
        "topsecret-token",
        "envsecret",
        "file-token",
        "lit_",
        tokpath.as_str(),
    ] {
        assert!(!wire.contains(gone), "the copy must not carry {gone}");
    }
    // The shape crosses: names, the env variable it reads here, the token prefix.
    assert!(
        wire.contains("KEEP_ME") && wire.contains("Bearer") && wire.contains("node.exe"),
        "{wire}"
    );

    // Import lands clean, and a second copy of the same list skips the ids kept here.
    let imported = api
        .call_host("mcp_copy_import", json!({"servers": listed["servers"]}))
        .await
        .unwrap();
    assert_eq!(imported["servers"].as_array().unwrap().len(), 2);
    let again = api
        .call_host(
            "mcp_copy_import",
            json!({"servers": listed["servers"], "revision": imported["revision"]}),
        )
        .await
        .unwrap();
    assert_eq!(again["servers"].as_array().unwrap().len(), 2);
    assert_eq!(again["revision"], imported["revision"]);
    // What saved here holds no secret either, and the blanked slots wait for their token.
    let text = std::fs::read_to_string(dst.path().join("mcp-servers.json")).unwrap();
    for gone in ["topsecret", "envsecret", "lit_", tokpath.as_str()] {
        assert!(!text.contains(gone), "the saved copy must not carry {gone}");
    }
    // A household member who is not this PC's person may not read its tool servers.
    let other = Watcher::of(&router, "dev-c", "Phone", true, "person-b", "Bo");
    let e = other
        .rpc
        .call("dev-a", "mcp_list_get", json!({}))
        .await
        .unwrap_err();
    assert_eq!(e.code, code::NOT_ALLOWED);
}
