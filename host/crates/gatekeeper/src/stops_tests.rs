//! The stop judge through a stand-in chat runtime: which turn ends it holds, and what it does
//! with each answer the helper can give. Every step runs on the test's own thread, so each
//! outcome is read right after the step that caused it.

use super::*;
use crate::store::GateAsk;
use crate::{route, workmode};
use helm_core::db::MigrationRegistry;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

/// A chat runtime that answers from a script and records what it was asked to do.
#[derive(Default)]
struct Fake {
    answers: Mutex<VecDeque<Result<Value, String>>>,
    jobs: Mutex<Vec<Value>>,
    verdicts: Mutex<Vec<StopOutcome>>,
    /// Verdicts find no hold: the user wrote, or the deadline passed, first.
    late: AtomicBool,
    sent: Mutex<Vec<(String, String, String)>>,
    needs_user: AtomicBool,
}

impl Host for Fake {
    fn run_helper(&self, job: &Value) -> Result<Value, String> {
        self.jobs.lock().unwrap().push(job.clone());
        self.answers
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_else(|| Err("nothing scripted".into()))
    }
    fn verdict(&self, _: &str, _: &str, o: StopOutcome) -> bool {
        self.verdicts.lock().unwrap().push(o);
        !self.late.load(Ordering::SeqCst)
    }
    fn send(&self, chat: &str, id: &str, text: &str) -> Result<(), String> {
        self.sent
            .lock()
            .unwrap()
            .push((chat.into(), id.into(), text.into()));
        Ok(())
    }
    fn waiting_on_user(&self, _: &str) -> bool {
        self.needs_user.load(Ordering::SeqCst)
    }
    fn changed(&self) {}
    fn shutting_down(&self) -> bool {
        false
    }
    fn wait_for_slot(&self) -> Duration {
        Duration::from_millis(50)
    }
    fn helper_ms(&self) -> Option<u64> {
        None
    }
}

struct Pct(Mutex<Option<f64>>);
impl UsageProbe for Pct {
    fn used_pct(&self, _: i64) -> Option<f64> {
        *self.0.lock().unwrap()
    }
}

struct T {
    _dir: tempfile::TempDir,
    db: Arc<Database>,
    gate: Arc<StopGate>,
    fake: Arc<Fake>,
    usage: Arc<Pct>,
    chat: String,
}

const STOPPING: &str = "Phase one is in. Shall I start phase two?";

fn t() -> T {
    let dir = tempfile::tempdir().unwrap();
    let mut reg = MigrationRegistry::new();
    helm_chats::register_migrations(&mut reg);
    helm_waits::register_migrations(&mut reg);
    reg.initializer("gatekeeper", crate::store::init);
    let db = Arc::new(Database::open(&dir.path().join("t.db"), &reg).unwrap());
    let fake = Arc::new(Fake::default());
    let usage = Arc::new(Pct(Mutex::new(Some(40.0))));
    let gate = StopGate::build(
        db.clone(),
        Arc::new(helm_ipc::NullSink),
        fake.clone(),
        Some(usage.clone()),
    );
    let chat = db
        .write(|tx| helm_chats::store::create_chat(tx, "C:/w", None, None, 1))
        .unwrap();
    let t = T {
        _dir: dir,
        db,
        gate,
        fake,
        usage,
        chat,
    };
    t.say(&t.chat, STOPPING);
    t
}

impl T {
    fn new_chat(&self) -> String {
        self.db
            .write(|tx| helm_chats::store::create_chat(tx, "C:/w", None, None, 1))
            .unwrap()
    }

    fn sql(&self, sql: &str, args: &[&str]) {
        let args: Vec<&dyn rusqlite::ToSql> =
            args.iter().map(|a| a as &dyn rusqlite::ToSql).collect();
        self.db
            .write(|tx| tx.execute(sql, args.as_slice()))
            .unwrap();
    }

    fn say(&self, chat: &str, text: &str) {
        let n = helm_core::now_ms();
        self.db
            .write(|tx| {
                tx.execute(
                    "INSERT INTO messages (chat_id, source_key, role, text, timestamp)
                     VALUES (?1, ?2, 'assistant', ?3, ?4)",
                    params![chat, format!("a:{}", uuid::Uuid::new_v4()), text, n],
                )
            })
            .unwrap();
    }

    /// A message the user side sent, from `origin`, at `at`.
    fn user_says(&self, origin: &str, at: i64) {
        let id = uuid::Uuid::new_v4().to_string();
        self.db
            .write(|tx| {
                tx.execute(
                    "INSERT INTO submissions (id, chat_id, text, mode, state, created_at)
                     VALUES (?1, ?2, 'ok', 'prompt', 'complete', ?3)",
                    params![id, self.chat, at],
                )?;
                tx.execute(
                    "INSERT INTO submission_meta (submission_id, origin) VALUES (?1, ?2)",
                    params![id, origin],
                )?;
                tx.execute(
                    "INSERT INTO messages (chat_id, source_key, role, text, timestamp)
                     VALUES (?1, ?2, 'user', 'ok', ?3)",
                    params![self.chat, format!("u:{id}"), at],
                )
            })
            .unwrap();
    }

    fn mode(&self, m: Option<workmode::Mode>) {
        self.db.write(|tx| workmode::set_app(tx, m)).unwrap();
    }

    fn end(run: &str) -> TurnEnd {
        TurnEnd {
            run_id: run.into(),
            ended_at: 0,
        }
    }

    /// Offer a turn end of `chat`; whether it was held.
    fn held(&self, chat: &str, run: &str) -> bool {
        matches!(self.gate.begin(chat, &Self::end(run)), Offered::Hold(_))
    }

    /// Hold a turn end of the main chat and judge it, with `answer` from the helper.
    fn run(&self, run: &str, answer: Option<Result<Value, String>>) -> GateAsk {
        if let Some(a) = answer {
            self.fake.answers.lock().unwrap().push_back(a);
        }
        let Offered::Hold(h) = self.gate.begin(&self.chat, &Self::end(run)) else {
            panic!("{run} was not held");
        };
        self.gate.finish(*h);
        self.ask(run)
    }

    fn all(&self) -> Vec<GateAsk> {
        self.db
            .snapshot(|tx| store::list(tx, "", "all", "", 500, helm_core::now_ms() + 1))
            .unwrap()
    }

    fn ask(&self, run: &str) -> GateAsk {
        self.all()
            .into_iter()
            .find(|r| r.request_ref == run && r.channel == "stop")
            .unwrap_or_else(|| panic!("no record for {run}"))
    }

    fn chain(&self) -> i64 {
        self.db
            .write(|tx| store::chain(tx, &self.chat, helm_core::now_ms()))
            .unwrap()
            .0
    }

    fn nudge(&self, question: &str) {
        self.db
            .write(|tx| store::add_nudge(tx, &self.chat, question, helm_core::now_ms()))
            .unwrap();
    }

    fn flag(&self) -> Option<(String, String, String)> {
        self.db
            .snapshot(|tx| {
                tx.query_row(
                    "SELECT question, reason, priority FROM chat_flags WHERE chat_id = ?1",
                    [&self.chat],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                )
                .optional()
            })
            .unwrap()
    }

    fn last_verdict(&self) -> StopOutcome {
        self.fake.verdicts.lock().unwrap().last().cloned().unwrap()
    }

    fn sent(&self) -> Vec<String> {
        self.fake
            .sent
            .lock()
            .unwrap()
            .iter()
            .map(|s| s.2.clone())
            .collect()
    }

    fn helper_calls(&self) -> usize {
        self.fake.jobs.lock().unwrap().len()
    }
}

fn carry_on(question: &str, default: &str) -> Value {
    json!({"class": "continue", "question": question, "default": default,
           "reason": "the plan names it next", "confidence": 0.9})
}

#[test]
fn only_a_stop_in_run_mode_that_the_table_lets_through_is_held() {
    let t = t();
    let c = t.chat.clone();
    // Check in is today's behaviour: nothing is held, nothing is recorded.
    assert!(!t.held(&c, "r0"));
    assert!(t.all().is_empty());
    t.mode(Some(workmode::Mode::Run));
    assert!(t.held(&c, "r1"), "a stop in run mode is held");
    assert_eq!(t.ask("r1").verdict, "checking");
    assert!(!t.held(&c, "r1"), "a replay of the same run stacks nothing");
    assert!(!t.held("no-such-chat", "r2"));

    t.say(&c, "All done. The tests pass.");
    assert!(!t.held(&c, "r3"), "a message that does not ask");
    t.say(&c, STOPPING);
    assert!(t.held(&c, "r4"));
    let now = helm_core::now_ms();
    t.user_says("telegram", now + 10);
    assert!(!t.held(&c, "r5"), "a live phone conversation");
    t.user_says("voice", now + 20);
    assert!(!t.held(&c, "r5"));
    t.user_says("app", now + 30);
    assert!(
        !t.held(&c, "r5"),
        "a note from Helm does not end the conversation"
    );
    t.user_says("typed", now + 40);
    assert!(t.held(&c, "r6"), "the user typing at the PC brings it back");

    let wait = helm_waits::model::Wait {
        id: "w1".into(),
        chat_id: c.clone(),
        target: helm_waits::model::Target::Process { pid: 1 },
        note: None,
        created_at: 1,
        expires_at: now + 3_600_000,
        state: Value::Null,
    };
    t.db.write(|tx| helm_waits::store::add(tx, &wait))
        .unwrap()
        .unwrap();
    assert!(!t.held(&c, "r7"), "a declared wait");
    t.sql("DELETE FROM agent_waits", &[]);

    let other = t.new_chat();
    t.say(&other, STOPPING);
    assert!(t.held(&other, "r8"));
    t.sql(
        "UPDATE chats SET provider = 'local-x' WHERE id = ?1",
        &[&other],
    );
    assert!(!t.held(&other, "r9"), "a local chat's words never leave");
    t.sql(
        "UPDATE chats SET provider = NULL, side_source_id = ?2 WHERE id = ?1",
        &[&other, &c],
    );
    assert!(!t.held(&other, "r10"), "a side chat");

    // Past its own limits the judge steps aside and says so, and so it does while paused.
    let used = {
        let l = t.gate.gate.lock();
        l.running + l.waiting
    };
    for n in 0..(stoproute::STOP_CAPS.running + stoproute::STOP_CAPS.waiting - used) {
        assert!(t.held(&c, &format!("q{n}")), "q{n}");
    }
    assert!(!t.held(&c, "over"));
    assert_eq!(t.ask("over").pass_reason.as_deref(), Some("limit"));
    for _ in 0..route::FAILURE_TRIP {
        t.gate.gate.lock().note_failure(helm_core::now_ms());
    }
    assert!(!t.held(&c, "paused"));
    assert_eq!(t.ask("paused").pass_reason.as_deref(), Some("paused"));
}

#[test]
fn what_the_judge_does_with_each_answer() {
    let t = t();
    t.mode(Some(workmode::Mode::Run));

    // A confident "carry on": the chat is released, counted and sent a note from Helm.
    let a = t.run(
        "a",
        Some(Ok(carry_on(
            "Start phase two?",
            "Start phase two in small commits",
        ))),
    );
    assert_eq!((a.verdict.as_str(), a.state.as_str()), ("decided", "open"));
    assert_eq!(
        a.choice.as_deref(),
        Some("Start phase two in small commits")
    );
    assert_eq!(a.question, "Start phase two?");
    assert_eq!(t.last_verdict(), StopOutcome::Continue);
    let sent = t.fake.sent.lock().unwrap().clone();
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].1, a.id, "one message per ask");
    assert!(sent[0]
        .2
        .starts_with("[Helm \u{b7} Run to completion] Carry on without asking."));
    assert_eq!(t.chain(), 1);
    let job = t.fake.jobs.lock().unwrap()[0].clone();
    assert_eq!(job["kind"], "stop");
    assert_eq!(job["facts"]["usedPct"], 40.0);
    assert_eq!(job["facts"]["childrenBusy"], 0);
    assert!(job["last"].as_str().unwrap().contains("phase two"));

    // The same question again: the chat is let stop and flagged, and the run is over.
    let b = t.run("b", Some(Ok(carry_on("start PHASE two", "Start it"))));
    assert_eq!(
        (b.verdict.as_str(), b.stop_class.as_deref()),
        ("escalated", Some("asked"))
    );
    match t.last_verdict() {
        StopOutcome::Stop { kind, line } => {
            assert_eq!(kind, StopKind::Asked);
            assert!(line.contains("Asked the same thing again"), "{line}");
            assert!(
                line.contains("1 choice was made for you on the way"),
                "{line}"
            );
        }
        o => panic!("{o:?}"),
    }
    let (_, reason, priority) = t.flag().unwrap();
    assert!(reason.starts_with("Stopped to ask:"), "{reason}");
    assert_eq!(priority, "normal");
    assert_eq!(
        (t.chain(), t.sent().len()),
        (0, 1),
        "the run is over, and no second nudge"
    );
    t.sql("DELETE FROM chat_flags", &[]);

    // A blocker: stopped, flagged high with the class word first.
    let c = t.run(
        "c",
        Some(Ok(
            json!({"class": "blocker", "reason": "needs your sign-in to the package registry",
                       "question": "Sign in?", "confidence": 0.9}),
        )),
    );
    assert_eq!(c.stop_class.as_deref(), Some("blocker"));
    assert_eq!(
        t.flag().unwrap(),
        (
            "Sign in?".into(),
            "Blocker: needs your sign-in to the package registry".into(),
            "high".into()
        )
    );
    t.sql("DELETE FROM chat_flags", &[]);

    // The word list beats the model: a step that cannot be undone is a blocker.
    let d = t.run(
        "d",
        Some(Ok(carry_on(
            "Start phase two?",
            "Delete the old backups first",
        ))),
    );
    assert_eq!(d.stop_class.as_deref(), Some("blocker"));
    t.sql("DELETE FROM chat_flags", &[]);

    // Three nudges is the most; a finished run says how many choices were made.
    for n in 0..3 {
        t.nudge(&format!("question {n}"));
    }
    let e = t.run("e", Some(Ok(carry_on("Start phase three?", "Start it"))));
    assert_eq!(e.stop_class.as_deref(), Some("asked"));
    assert!(e.reason.unwrap().starts_with("Still asking after 3 nudges"));
    t.sql("DELETE FROM chat_flags", &[]);
    for n in 0..2 {
        t.nudge(&format!("question {n}"));
    }
    let f = t.run(
        "f",
        Some(Ok(json!({"class": "done", "reason": "all finished"}))),
    );
    assert_eq!(
        (f.verdict.as_str(), f.pass_reason.as_deref()),
        ("passed", Some("done"))
    );
    assert_eq!(
        t.last_verdict(),
        StopOutcome::Done {
            line: Some("Run finished: 2 choices were made for you on the way.".into())
        }
    );
    assert_eq!(t.chain(), 0, "a finished run starts the count again");

    // A verdict that finds no hold (the user wrote first) changes nothing else.
    t.fake.late.store(true, Ordering::SeqCst);
    let before = t.sent().len();
    let g = t.run("g", Some(Ok(carry_on("Start phase two?", "Start it"))));
    assert_eq!(
        (g.verdict.as_str(), g.state.as_str()),
        ("decided", "withdrawn")
    );
    assert_eq!(
        (t.sent().len(), t.chain()),
        (before, 0),
        "no nudge after a late verdict"
    );
    let h = t.run(
        "h",
        Some(Ok(json!({"class": "blocker", "reason": "needs a sign-in"}))),
    );
    assert_eq!(h.state, "withdrawn");
    assert!(t.flag().is_none(), "no flag after a late verdict");
    t.fake.late.store(false, Ordering::SeqCst);

    // A chat that already needs the user is done, and no model is asked.
    let calls = t.helper_calls();
    t.fake.needs_user.store(true, Ordering::SeqCst);
    let i = t.run("i", None);
    assert_eq!(
        (i.verdict.as_str(), i.pass_reason.as_deref()),
        ("passed", Some("needs_you"))
    );
    t.fake.needs_user.store(false, Ordering::SeqCst);
    assert_eq!(t.helper_calls(), calls);

    // Chats it started that still work: wait for them properly, again with no model.
    let child = t.new_chat();
    t.sql(
        "UPDATE chats SET created_by_chat = ?2, status = 'running' WHERE id = ?1",
        &[&child, &t.chat],
    );
    let j = t.run("j", None);
    assert_eq!(
        (j.verdict.as_str(), j.decided_by.as_deref()),
        ("decided", Some("rule"))
    );
    assert!(t
        .sent()
        .last()
        .unwrap()
        .contains("1 chats you started are still working"));
    assert_eq!(t.helper_calls(), calls);
    t.sql("UPDATE chats SET status = 'idle' WHERE id = ?1", &[&child]);
    t.db.write(|tx| store::end_chain(tx, &t.chat)).unwrap();

    // The plan limit is checked against the reading and the local work that remains.
    let limit = json!({"class": "limit", "reason": "the plan is used up"});
    let k = t.run("k", Some(Ok(limit.clone())));
    assert_eq!(k.verdict, "decided", "usage is at 40%: carry on");
    assert!(t.sent().last().unwrap().contains("40%"));
    t.db.write(|tx| store::end_chain(tx, &t.chat)).unwrap();
    *t.usage.0.lock().unwrap() = Some(91.0);
    let l = t.run("l", Some(Ok(limit.clone())));
    assert_eq!(l.stop_class.as_deref(), Some("limit"));
    t.sql("DELETE FROM chat_flags", &[]);
    let local = t.new_chat();
    t.sql(
        "UPDATE chats SET provider = 'local-x', status = 'running' WHERE id = ?1",
        &[&local],
    );
    let m = t.run("m", Some(Ok(limit.clone())));
    assert_eq!(m.verdict, "decided", "local work remains");
    assert!(t.sent().last().unwrap().contains("local-model chats"));
    t.db.write(|tx| store::end_chain(tx, &t.chat)).unwrap();
    t.sql("UPDATE chats SET status = 'idle' WHERE id = ?1", &[&local]);
    *t.usage.0.lock().unwrap() = None;
    let n = t.run("n", Some(Ok(limit)));
    assert_eq!(
        n.stop_class.as_deref(),
        Some("limit"),
        "no reading: let it stop"
    );
    t.sql("DELETE FROM chat_flags", &[]);

    // The user writing in the chat ends a run of nudges; a note from Helm does not.
    for q in 0..3 {
        t.nudge(&format!("again {q}"));
    }
    assert_eq!(t.chain(), 3);
    t.user_says("typed", helm_core::now_ms() + 60_000);
    assert_eq!(t.chain(), 0);
    t.db.write(|tx| store::add_nudge(tx, &t.chat, "fresh", helm_core::now_ms() + 120_000))
        .unwrap();
    t.user_says("app", helm_core::now_ms() + 180_000);
    assert_eq!(t.chain(), 1);

    // Three answers that cannot be used pause the judge, and each lets the turn finish.
    let t2 = self::t();
    t2.mode(Some(workmode::Mode::Run));
    let bad = [
        Err("the helper did not start".to_string()),
        Ok(json!({"error": "timed out"})),
        Ok(json!({"class": "maybe"})),
    ];
    for (i, answer) in bad.into_iter().enumerate() {
        let r = t2.run(&format!("x{i}"), Some(answer));
        assert_eq!(
            (r.verdict.as_str(), r.state.as_str()),
            ("failed", "open"),
            "x{i}"
        );
        assert_eq!(t2.last_verdict(), StopOutcome::Failed);
    }
    assert!(
        !t2.held(&t2.chat.clone(), "x3"),
        "paused after three failures"
    );
}
