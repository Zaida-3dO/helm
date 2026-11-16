//! The stop judge's decision table, kept pure so one table can test all of it.
//!
//! A chat ends a turn and may be about to wait for the user. [`pre_model`] says which turn ends
//! the judge never looks at (nothing is asked of a model for them), [`parse_answer`] checks what
//! the model said, and [`route`] turns that into what Helm does, with the facts it can check for
//! itself:
//!
//! - what the model calls a blocker, a failure or a stop the user asked for is a stop;
//! - "carry on" needs the model's confidence to reach the gate's bar, and anything that matches
//!   the hard-to-undo word list ([`crate::route::hard_to_undo`]) is a blocker whatever the model
//!   said;
//! - "the plan limit" is checked against the usage reading and the local work that remains;
//! - a chain of nudges is bounded: three, and never the same question twice.
//!
//! Anything unclear lets the chat stop, which is what happens with no judge at all.

use crate::route::{hard_to_undo, Caps};
use helm_chats::seams::StopKind;
use serde_json::Value;

/// The stop judge's own limits: one judgement at a time, ten waiting, thirty an hour.
pub const STOP_CAPS: Caps = Caps {
    running: 1,
    waiting: 10,
    per_hour: 30,
};

/// Nudges in one run before the chat is let stop.
pub const MAX_NUDGES: i64 = 3;

/// How much of the end of a message is read for a question, a stop line or a hard-to-undo step.
pub const TAIL_CHARS: usize = 600;

const STOP_PHRASES: &[&str] = &[
    "shall i",
    "should i",
    "would you like",
    "let me know",
    "your go",
    "go-ahead",
    "go ahead",
    "confirm",
    "approve",
    "wait for your",
    "before i proceed",
    "need you to",
];

/// The last `max` characters of `text`.
pub fn tail(text: &str, max: usize) -> String {
    let n = text.chars().count();
    text.chars().skip(n.saturating_sub(max)).collect()
}

/// Whether a chat's last message reads like a stop: a question mark near the end, a "Stopped:"
/// line, or one of a fixed list of phrases. A cheap test that keeps most turn ends away from the
/// model; a miss is today's behaviour.
pub fn looks_like_stop(last: &str) -> bool {
    let end = tail(last, TAIL_CHARS);
    if end.contains('?') {
        return true;
    }
    let lower = end.to_lowercase();
    lower
        .lines()
        .any(|l| l.trim_start().starts_with("stopped:"))
        || STOP_PHRASES.iter().any(|p| lower.contains(p))
}

/// What the host knows of a turn end before it asks any model.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Pre<'a> {
    /// The chat's working mode is run to completion.
    pub run_mode: bool,
    /// The chat runs on a local model: its words never go to Claude.
    pub local_chat: bool,
    pub side_chat: bool,
    /// The user's latest message came from the phone (Telegram or voice): a live conversation,
    /// where the question is the reply.
    pub live_conversation: bool,
    /// The chat's last assistant message.
    pub last: &'a str,
    /// The chat declared a wait on something.
    pub open_wait: bool,
}

/// Why a turn end is not looked at.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Skip {
    CheckIn,
    LocalChat,
    SideChat,
    LiveConversation,
    NotAStop,
    OpenWait,
}

/// The first row that matches wins. `None`: the turn end goes on to the limits and the judge.
pub fn pre_model(p: &Pre<'_>) -> Option<Skip> {
    if !p.run_mode {
        Some(Skip::CheckIn)
    } else if p.local_chat {
        Some(Skip::LocalChat)
    } else if p.side_chat {
        Some(Skip::SideChat)
    } else if p.live_conversation {
        Some(Skip::LiveConversation)
    } else if !looks_like_stop(p.last) {
        Some(Skip::NotAStop)
    } else if p.open_wait {
        Some(Skip::OpenWait)
    } else {
        None
    }
}

/// What the model calls the end of the turn.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Class {
    Done,
    Continue,
    Blocker,
    Limit,
    Failure,
    AskedByUser,
}

/// The model's answer, checked.
#[derive(Clone, Debug, PartialEq)]
pub struct Answer {
    pub class: Class,
    /// What the chat was about to ask.
    pub question: String,
    /// The reasonable, easy-to-undo default to take instead.
    pub default: String,
    pub reason: String,
    pub confidence: f64,
}

fn text_of(o: &serde_json::Map<String, Value>, key: &str, max: usize) -> String {
    o.get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim()
        .chars()
        .take(max)
        .collect()
}

/// The helper's line, checked: anything shaped wrong counts as a failure to judge.
pub fn parse_answer(v: &Value) -> Option<Answer> {
    let o = v.as_object()?;
    let class = match o
        .get("class")?
        .as_str()?
        .trim()
        .to_ascii_lowercase()
        .as_str()
    {
        "done" => Class::Done,
        "continue" => Class::Continue,
        "blocker" => Class::Blocker,
        "limit" => Class::Limit,
        "failure" => Class::Failure,
        "asked_by_user" | "asked" => Class::AskedByUser,
        _ => return None,
    };
    Some(Answer {
        class,
        question: text_of(o, "question", 400),
        default: text_of(o, "default", 400),
        reason: text_of(o, "reason", 240),
        confidence: o
            .get("confidence")
            .and_then(Value::as_f64)
            .unwrap_or(0.0)
            .clamp(0.0, 1.0),
    })
}

/// What the plan-usage reading says.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Usage {
    /// No reading the host trusts.
    Unknown,
    /// Known, and under the wind-down line.
    Below(f64),
    /// At or past the wind-down line.
    Near(f64),
}

/// What the host checks for itself after the model has answered.
#[derive(Clone, Debug)]
pub struct Facts<'a> {
    /// The gate's confidence bar.
    pub bar: f64,
    pub usage: Usage,
    /// Local-model chats working or with a message waiting.
    pub local_working: i64,
    /// Nudges already sent in this run, and the question the last one answered.
    pub nudges: i64,
    pub last_nudge_question: &'a str,
    /// The chat's last assistant message.
    pub last: &'a str,
}

/// The words a nudge carries.
#[derive(Clone, Debug, PartialEq)]
pub enum Nudge {
    /// Take the default for the question.
    Default { question: String, default: String },
    /// The model called it the plan limit, but usage is below the line.
    BelowLimit { pct: f64 },
    /// The plan limit is near, but work that can run on a local model remains.
    UseLocal,
    /// Chats this one started are still working and it declared no wait on them.
    Children { count: i64 },
}

impl Nudge {
    /// What the nudge answered, for the record and for noticing a repeat.
    pub fn question(&self) -> String {
        match self {
            Self::Default { question, .. } => question.clone(),
            Self::BelowLimit { .. } => "Is the plan limit reached?".into(),
            Self::UseLocal => "Is the plan limit reached with local work left?".into(),
            Self::Children { .. } => "Are the chats you started finished?".into(),
        }
    }

    /// What it told the chat to do.
    pub fn default(&self) -> String {
        match self {
            Self::Default { default, .. } => default.clone(),
            Self::BelowLimit { pct } => {
                format!("Plan usage is at {pct:.0}%, below the limit; carry on.")
            }
            Self::UseLocal => "Hand what can run locally to local-model chats, wait for them with \
                app_wait_for and end your turn."
                .into(),
            Self::Children { count } => format!(
                "{count} chats you started are still working; declare a wait on them with \
                 app_wait_for and end your turn."
            ),
        }
    }
}

/// What Helm does with a held turn end.
#[derive(Clone, Debug, PartialEq)]
pub enum Route {
    /// Carry on: send the nudge.
    Continue(Nudge),
    /// Let the chat stop, and say why.
    Stop { kind: StopKind, line: String },
    /// The chat finished.
    Done,
}

/// Words compared across two questions: letters and digits only, lower case.
pub fn normalise(q: &str) -> String {
    q.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

fn stop(kind: StopKind, line: impl Into<String>) -> Route {
    Route::Stop {
        kind,
        line: line.into(),
    }
}

/// The first of the model's reason or question that says something.
fn why(a: &Answer, fallback: &str) -> String {
    [&a.reason, &a.question]
        .into_iter()
        .find(|s| !s.is_empty())
        .cloned()
        .unwrap_or_else(|| fallback.to_owned())
}

/// The route for one answered turn end.
pub fn route(a: &Answer, f: &Facts<'_>) -> Route {
    let carry_on = match a.class {
        Class::Done => return Route::Done,
        Class::Blocker => return stop(StopKind::Blocker, why(a, "Only you can clear this.")),
        Class::Failure => return stop(StopKind::Failure, why(a, "The work keeps failing.")),
        Class::AskedByUser => return stop(StopKind::Asked, why(a, "You asked it to wait.")),
        Class::Limit => match f.usage {
            Usage::Near(_) if f.local_working == 0 => {
                return stop(
                    StopKind::Limit,
                    why(a, "The plan's usage limit is reached."),
                )
            }
            Usage::Unknown => {
                return stop(
                    StopKind::Limit,
                    why(a, "The plan's usage limit is reached."),
                )
            }
            Usage::Near(_) => Nudge::UseLocal,
            Usage::Below(pct) => Nudge::BelowLimit { pct },
        },
        Class::Continue => {
            if a.confidence < f.bar || a.default.is_empty() || a.question.is_empty() {
                return stop(
                    StopKind::Asked,
                    if a.question.is_empty() {
                        why(a, "It stopped to ask.")
                    } else {
                        a.question.clone()
                    },
                );
            }
            Nudge::Default {
                question: a.question.clone(),
                default: a.default.clone(),
            }
        }
    };
    // Carrying on is checked against what the host can see for itself.
    let end = tail(f.last, TAIL_CHARS);
    if [a.question.as_str(), a.default.as_str(), end.as_str()]
        .iter()
        .any(|t| hard_to_undo(t))
    {
        return stop(
            StopKind::Blocker,
            if a.question.is_empty() {
                "The next step looks hard to undo, so it is yours to decide.".to_owned()
            } else {
                format!(
                    "The next step looks hard to undo, so it is yours to decide: {}",
                    a.question
                )
            },
        );
    }
    bound(carry_on, f)
}

/// A nudge is sent only while the run is within its limits: three nudges, and never the same
/// question twice running.
pub fn bound(nudge: Nudge, f: &Facts<'_>) -> Route {
    let question = nudge.question();
    if f.nudges >= MAX_NUDGES {
        return stop(
            StopKind::Asked,
            format!("Still asking after {MAX_NUDGES} nudges: \u{ab}{question}\u{bb}"),
        );
    }
    if !f.last_nudge_question.is_empty() && normalise(&question) == normalise(f.last_nudge_question)
    {
        return stop(
            StopKind::Asked,
            format!("Asked the same thing again: \u{ab}{question}\u{bb}"),
        );
    }
    Route::Continue(nudge)
}

/// The message a nudge sends. It comes from Helm, never the user, and approves nothing.
pub fn nudge_text(n: &Nudge) -> String {
    match n {
        Nudge::Default { question, default } => format!(
            "[Helm \u{b7} Run to completion] Carry on without asking. Go ahead with: \u{ab}{default}\u{bb}. \
             (You were about to ask: \u{ab}{question}\u{bb}.) Mention this choice in your final summary \
             so the user can change it, and keep it easy to undo. {STOP_ONLY}"
        ),
        Nudge::BelowLimit { pct } => format!(
            "[Helm \u{b7} Run to completion] Plan usage is at {pct:.0}%, below the limit; carry on. {STOP_ONLY}"
        ),
        Nudge::UseLocal => format!(
            "[Helm \u{b7} Run to completion] Work that can run on a local model remains. Hand what can \
             run locally to local-model chats, wait for them with app_wait_for and end your turn. {STOP_ONLY}"
        ),
        Nudge::Children { .. } => format!("[Helm \u{b7} Run to completion] {} {STOP_ONLY}", n.default()),
    }
}

const STOP_ONLY: &str = "Stop only for a real blocker, a plan limit with no local work left, or a \
failure that persists after retries, and say which in a line starting \"Stopped:\".";

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn pre<'a>() -> Pre<'a> {
        Pre {
            run_mode: true,
            local_chat: false,
            side_chat: false,
            live_conversation: false,
            last: "The report is ready. Shall I go ahead and file it?",
            open_wait: false,
        }
    }

    fn facts<'a>() -> Facts<'a> {
        Facts {
            bar: 0.75,
            usage: Usage::Below(40.0),
            local_working: 0,
            nudges: 0,
            last_nudge_question: "",
            last: "Phase one is in. Shall I start phase two?",
        }
    }

    fn answer(class: Class) -> Answer {
        Answer {
            class,
            question: "Start phase two?".into(),
            default: "Start phase two and keep the commits small".into(),
            reason: "the plan names phase two next".into(),
            confidence: 0.9,
        }
    }

    #[test]
    fn the_table_before_any_model() {
        type Edit = fn(&mut Pre<'static>);
        let cases: Vec<(&str, Edit, Option<Skip>)> = vec![
            ("a question in run mode goes on", |_| {}, None),
            (
                "check in mode is today's behaviour",
                |p| p.run_mode = false,
                Some(Skip::CheckIn),
            ),
            (
                "a local chat's words never leave",
                |p| p.local_chat = true,
                Some(Skip::LocalChat),
            ),
            ("a side chat", |p| p.side_chat = true, Some(Skip::SideChat)),
            (
                "a live phone conversation",
                |p| p.live_conversation = true,
                Some(Skip::LiveConversation),
            ),
            (
                "a message that does not ask",
                |p| p.last = "All done. The tests pass.",
                Some(Skip::NotAStop),
            ),
            (
                "a declared wait",
                |p| p.open_wait = true,
                Some(Skip::OpenWait),
            ),
            // The first matching row wins.
            (
                "check in beats the rest",
                |p| {
                    p.run_mode = false;
                    p.local_chat = true
                },
                Some(Skip::CheckIn),
            ),
        ];
        for (name, edit, want) in cases {
            let mut p = pre();
            edit(&mut p);
            assert_eq!(pre_model(&p), want, "{name}");
        }

        // What reads like a stop: a question mark or a phrase near the end, or a "Stopped:"
        // line, and nothing earlier than the last 600 characters.
        let far = format!("Should I? {}", "x".repeat(700));
        for (text, want) in [
            ("Ready?", true),
            ("Let me know when to merge.", true),
            ("I need you to sign in.", true),
            ("Working.\nStopped: blocker \u{2014} sign-in", true),
            ("Done. Everything is merged.", false),
            (far.as_str(), false),
            ("", false),
        ] {
            assert_eq!(looks_like_stop(text), want, "{text:?}");
        }
    }

    #[test]
    fn what_the_model_said_is_checked() {
        let ok = parse_answer(&json!({
            "class": "continue", "question": " Start? ", "default": "Yes", "reason": "fits",
            "confidence": 3
        }))
        .unwrap();
        assert_eq!(ok.class, Class::Continue);
        assert_eq!(ok.question, "Start?");
        assert_eq!(ok.confidence, 1.0, "confidence is clamped");
        for (word, class) in [
            ("done", Class::Done),
            ("blocker", Class::Blocker),
            ("limit", Class::Limit),
            ("failure", Class::Failure),
            ("asked_by_user", Class::AskedByUser),
            ("asked", Class::AskedByUser),
        ] {
            assert_eq!(parse_answer(&json!({"class": word})).unwrap().class, class);
        }
        assert!(parse_answer(&json!({"class": "maybe"})).is_none());
        assert!(parse_answer(&json!({"verdict": "decide"})).is_none());
        assert!(parse_answer(&json!("junk")).is_none());
    }

    #[test]
    fn the_route() {
        let go = |q: &str, d: &str| {
            Route::Continue(Nudge::Default {
                question: q.into(),
                default: d.into(),
            })
        };
        let kind = |r: &Route| match r {
            Route::Stop { kind, .. } => Some(*kind),
            _ => None,
        };
        type Edit = fn(&mut Answer, &mut Facts<'static>);
        let cases: Vec<(&str, Edit, Option<StopKind>, &str)> = vec![
            ("done", |a, _| a.class = Class::Done, None, "done"),
            (
                "blocker",
                |a, _| a.class = Class::Blocker,
                Some(StopKind::Blocker),
                "stop",
            ),
            (
                "failure",
                |a, _| a.class = Class::Failure,
                Some(StopKind::Failure),
                "stop",
            ),
            (
                "asked by the user",
                |a, _| a.class = Class::AskedByUser,
                Some(StopKind::Asked),
                "stop",
            ),
            ("a confident continue carries on", |_, _| {}, None, "go"),
            (
                "below the bar lets it stop",
                |a, _| a.confidence = 0.5,
                Some(StopKind::Asked),
                "stop",
            ),
            (
                "no default lets it stop",
                |a, _| a.default.clear(),
                Some(StopKind::Asked),
                "stop",
            ),
            // The word list beats the model, wherever the hard step is written.
            (
                "a hard question",
                |a, _| a.question = "Shall I delete the old backups?".into(),
                Some(StopKind::Blocker),
                "stop",
            ),
            (
                "a hard default",
                |a, _| a.default = "Publish it now".into(),
                Some(StopKind::Blocker),
                "stop",
            ),
            (
                "a hard step in the last message",
                |_, f| f.last = "The build is ready to deploy. Shall I start phase two?",
                Some(StopKind::Blocker),
                "stop",
            ),
            (
                "a hard word far above the question is not read",
                |_, f| {
                    f.last = Box::leak(
                        format!("Deploy notes. {} Shall I start phase two?", "x".repeat(700))
                            .into_boxed_str(),
                    )
                },
                None,
                "go",
            ),
            // The plan limit is checked against the facts.
            (
                "the limit, near, with no local work",
                |a, f| {
                    a.class = Class::Limit;
                    f.usage = Usage::Near(91.0)
                },
                Some(StopKind::Limit),
                "stop",
            ),
            (
                "the limit, usage unknown",
                |a, f| {
                    a.class = Class::Limit;
                    f.usage = Usage::Unknown
                },
                Some(StopKind::Limit),
                "stop",
            ),
            (
                "the limit, but usage is low",
                |a, _| a.class = Class::Limit,
                None,
                "below",
            ),
            (
                "the limit, near, with local work left",
                |a, f| {
                    a.class = Class::Limit;
                    f.usage = Usage::Near(91.0);
                    f.local_working = 2
                },
                None,
                "local",
            ),
            // Bounded: three, and never the same question twice.
            (
                "a fourth nudge",
                |_, f| f.nudges = 3,
                Some(StopKind::Asked),
                "stop",
            ),
            (
                "the third nudge still goes",
                |_, f| f.nudges = 2,
                None,
                "go",
            ),
            (
                "the same question as the last nudge",
                |_, f| f.last_nudge_question = "start phase two",
                Some(StopKind::Asked),
                "stop",
            ),
        ];
        for (name, edit, want_kind, shape) in cases {
            let (mut a, mut f) = (answer(Class::Continue), facts());
            edit(&mut a, &mut f);
            let got = route(&a, &f);
            assert_eq!(kind(&got), want_kind, "{name}: {got:?}");
            match (shape, &got) {
                ("done", Route::Done) | ("stop", Route::Stop { .. }) => {}
                ("go", r) => assert_eq!(
                    *r,
                    go(
                        "Start phase two?",
                        "Start phase two and keep the commits small"
                    ),
                    "{name}"
                ),
                ("below", Route::Continue(Nudge::BelowLimit { pct })) => {
                    assert_eq!(*pct, 40.0, "{name}")
                }
                ("local", Route::Continue(Nudge::UseLocal)) => {}
                _ => panic!("{name}: {got:?}"),
            }
        }
        // The stop says what to tell the user; a repeat names the question.
        let mut f = facts();
        f.nudges = 3;
        match route(&answer(Class::Continue), &f) {
            Route::Stop { line, .. } => {
                assert!(line.contains("Still asking after 3 nudges"), "{line}")
            }
            r => panic!("{r:?}"),
        }
    }

    #[test]
    fn a_nudge_is_plain_words_from_helm() {
        let t = nudge_text(&Nudge::Default {
            question: "Start phase two?".into(),
            default: "Start it".into(),
        });
        assert!(t.starts_with("[Helm \u{b7} Run to completion] Carry on without asking."));
        assert!(t.contains("\u{ab}Start it\u{bb}") && t.contains("\u{ab}Start phase two?\u{bb}"));
        assert!(t.contains("starting \"Stopped:\""));
        assert_eq!(normalise("Start  PHASE two?"), normalise("start phase two"));
    }
}
