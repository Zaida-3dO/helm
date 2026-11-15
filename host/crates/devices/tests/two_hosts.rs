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
            json!({"chat": {"id": "c1-copy"}}),
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
    // not. The switch covers every op this slice added.
    port.set_run(false);
    for (op, args) in [
        ("verbosity_get", json!({"chatId": "c1"})),
        ("context_on", json!({"chatId": "c1"})),
        ("quick_open", json!({"chatId": "c1", "query": "x"})),
        ("checkpoint", json!({"chatId": "c1", "messageId": 2})),
        ("attention", json!({"chatIds": ["c1"]})),
        ("item_links", json!({"chatIds": ["c1"]})),
        ("defaults_get", json!({"chatId": "c1"})),
        // The panel and board reads answer with the switch off too: watching a chat includes
        // looking at its files and its board.
        ("chat_folder", json!({"chatId": "c1"})),
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
        ("board_connection", json!({"chatId": "c1"})),
        (
            "board_read",
            json!({"chatId": "c1", "action": "ready", "requestId": "tr-2", "timeoutMs": 5000}),
        ),
        ("board_cancel", json!({"chatId": "c1", "requestId": "tr-2"})),
    ] {
        assert!(
            b.rpc.call("dev-a", op, args).await.is_ok(),
            "{op} is a read"
        );
    }
    for (op, args) in [
        ("set_permission", json!({"chatId": "c1", "mode": "plan"})),
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
            Ok(v) => assert_eq!(
                got.clone().unwrap_or_else(|e| panic!("{op}: {e}")),
                v,
                "{op}"
            ),
            Err(m) => assert_eq!(got.unwrap_err().message, m, "{op}"),
        }
    }
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
        // file-view read, as it is here. Staging came through as the stage pair, and the
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
                "diff"
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

/// A profile linked after the first sync widens the copy: the app's event tap, handed the
/// mirror, asks the other PC again with the wider read set, and the chat of the new person
/// arrives without the hub line dropping.
#[tokio::test(start_paused = true)]
async fn a_profile_linked_later_brings_its_chats_into_the_copy() {
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
        &[("person-s", true, true)],
    );
    until(|| b.mirror.owner("own-s").is_some()).await;
    assert!(b.mirror.owner("own-t").is_none());

    // The laptop now announces Tia as well; the tap hears the profile change.
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
            })
            .collect(),
    }));
    settle().await;
    let tap = helm_devices::EventTap::default();
    tap.attach_mirror(b.mirror.clone());
    tap.observe(&AppEvent::new("profiles_changed"));
    until(|| b.mirror.owner("own-t").is_some()).await;
}
