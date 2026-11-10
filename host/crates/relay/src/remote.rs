//! The Remote page's two tools: what waits for the user, and an answer to it.
//!
//! The hub serves a small page (phone or laptop) that talks to Helm over the relay line. Reading
//! chats, sending a message and stopping a turn are the tools Telegram and other agents already
//! have ([`crate::gate`]). These two are only for the page: the hub stamps a call from it with its
//! scope, and [`crate::gate::admit`] refuses them for every other caller.
//!
//! - **`app_list_waiting`** lists every prompt that waits for the user, with enough of it to
//!   decide: the chat, what kind it is (a permission prompt, a plan, a question), a short text and
//!   the options. It is a read, so it works with the switch off; each item then says it cannot be
//!   answered yet.
//! - **`app_answer_waiting`** answers one of them, through the same function a Telegram tap uses
//!   ([`crate::sink::answer_request`]) and under the same limits. It is refused while the switch
//!   in Settings → Hub is off. A permission prompt or plan is answered only by choosing one of its
//!   options by number (never by typed text); when its details are too long to read on a phone
//!   only the options that allow nothing are offered. A question is answered by choosing an
//!   option or, for a free-text question, by typing. Secret inputs, folder-trust prompts and
//!   consent requests are never listed and never answered from here, and an answer from the page
//!   is never consent: the chat gets a line saying where the answer came from, and nothing else.
//! - A request answers once. It is answered only while it still waits, and a second answer within
//!   a short time says so even before the chat has caught up.

use crate::approve::{self, Waiting};
use crate::client::ToolFailure;
use crate::exec::{refused, Wiring};
use crate::sink::{self, answer_request, AnswerError, ChatSink};
use crate::telegram;
use helm_chats::types::{
    AttentionKind, ContextKind, Interaction, InteractionAttention, InteractionMethod,
};
use helm_chats::ChatRuntime;
use helm_core::profile::Profile;
use serde_json::{json, Value};
use std::collections::hash_map::Entry;
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Said when the switch is off.
pub const SWITCH_OFF: &str = "Answering from the Remote page is off. Turn on \"Allow answering \
    waiting prompts from the Remote page\" in Settings > Hub.";
/// Said when the request is gone.
pub const GONE: &str = "That request no longer waits, so nothing was answered.";
/// The most prompts listed at once.
const MAX_LISTED: usize = 200;
/// The most options shown for one question.
const MAX_QUESTION_OPTIONS: usize = 12;
/// The longest typed answer.
pub const MAX_TYPED: usize = 4000;
/// How long a request stays marked as answered, until the chat has caught up.
const ANSWERED_FOR: Duration = Duration::from_secs(30);

/// Requests answered a moment ago (or being answered now), by chat and request id.
#[derive(Default)]
pub struct Answered(Mutex<HashMap<(String, String), Instant>>);

impl Answered {
    /// Claim a request for answering. `false` when it was answered, or is being answered, now.
    fn claim(&self, chat: &str, request: &str, now: Instant) -> bool {
        let mut m = self.0.lock().unwrap_or_else(|p| p.into_inner());
        m.retain(|_, t| now.duration_since(*t) < ANSWERED_FOR);
        match m.entry((chat.to_owned(), request.to_owned())) {
            Entry::Occupied(_) => false,
            Entry::Vacant(v) => {
                v.insert(now);
                true
            }
        }
    }

    /// Give a claim back: the answer did not go through, so the request may be tried again.
    fn release(&self, chat: &str, request: &str) {
        self.0
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .remove(&(chat.to_owned(), request.to_owned()));
    }
}

fn plain(i: &Interaction) -> String {
    let mut s = String::new();
    for part in [i.title.as_deref(), i.message.as_deref()]
        .into_iter()
        .flatten()
    {
        let p = part.trim();
        if !p.is_empty() && !s.contains(p) {
            if !s.is_empty() {
                s.push('\n');
            }
            s.push_str(p);
        }
    }
    if s.is_empty() {
        s.push_str("A question");
    }
    approve::cut(&s, 1500)
}

/// A free-text or multiple-choice question the page may answer: not a secret.
fn question(i: &Interaction) -> bool {
    sink::is_answerable(i) && i.secret != Some(true)
}

/// One waiting prompt as the page shows it, or `None` when the page must not show it.
pub fn view(w: &Waiting, on: bool) -> Option<Value> {
    let i = &w.interaction;
    if i.secret == Some(true) {
        return None;
    }
    let mut v = json!({
        "chatId": w.chat_id,
        "chatTitle": approve::cut(&w.chat_title, 200),
        "requestId": i.id,
    });
    if let Some(o) = approve::offer(w) {
        let plan = i.attention == Some(InteractionAttention::Plan);
        v["kind"] = json!(if plan { "plan" } else { "permission" });
        v["text"] = json!(o.text);
        v["options"] = Value::Array(
            o.ask
                .options
                .iter()
                .map(|l| json!({ "label": l, "allows": approve::allows(l) }))
                .collect(),
        );
        v["tooLong"] = json!(!o.full);
        v["typed"] = json!(false);
        v["answerable"] = json!(on);
        return Some(v);
    }
    if question(i) {
        let options: Vec<Value> = i
            .options
            .iter()
            .flatten()
            .take(MAX_QUESTION_OPTIONS)
            .map(|l| json!({ "label": approve::cut(l, 200), "allows": false }))
            .collect();
        let typed = i.method == InteractionMethod::Input
            || (i.allow_other == Some(true) && i.method == InteractionMethod::Select);
        v["kind"] = json!("question");
        v["text"] = json!(plain(i));
        v["answerable"] = json!(on && (!options.is_empty() || typed));
        v["options"] = Value::Array(options);
        v["tooLong"] = json!(false);
        v["typed"] = json!(typed);
        return Some(v);
    }
    None
}

/// `app_list_waiting`: the prompts that wait, in the attention summary's order. The summary has
/// no cap on chats that wait on the user, so neither has this list (only [`MAX_LISTED`] prompts).
pub fn list_waiting(w: &Wiring) -> Result<Value, ToolFailure> {
    let rt = w
        .services
        .get::<ChatRuntime>()
        .ok_or_else(|| refused("Helm's chats are not available."))?;
    let on = w
        .services
        .get::<Profile>()
        .is_some_and(|p| telegram::load_remote_answers(&telegram::policy_path(&p.dir)));
    let summary = rt
        .attention()
        .map_err(|_| refused("What waits could not be read."))?;
    let mut waiting = vec![];
    'rows: for r in &summary.needs_you {
        if !matches!(
            r.kind,
            AttentionKind::Approval | AttentionKind::Plan | AttentionKind::Question
        ) {
            continue;
        }
        let Some(t) = rt.live_thread(&r.chat_id) else {
            continue;
        };
        for interaction in t.interactions {
            let w = Waiting {
                chat_id: r.chat_id.clone(),
                chat_title: r.title.clone(),
                interaction,
            };
            if let Some(v) = view(&w, on) {
                waiting.push(v);
                if waiting.len() >= MAX_LISTED {
                    break 'rows;
                }
            }
        }
    }
    let mut out = json!({ "answering": on, "waiting": waiting });
    if !on {
        out["hint"] = json!(SWITCH_OFF);
    }
    Ok(out)
}

/// What an answer means for one waiting request: the arguments for `interaction_answer`, the
/// words for the line in the chat, and which waiting requests it may answer.
#[derive(Debug)]
pub struct Plan {
    pub args: Value,
    pub label: String,
    pub accepts: fn(&Interaction) -> bool,
}

fn accepts_question(i: &Interaction) -> bool {
    question(i)
}

/// Said when the page's label is not the option it chose by number.
pub const OPTIONS_CHANGED: &str = "The options of that prompt are not the ones the page showed, \
    so nothing was answered. Refresh and look again.";

/// Work out the answer to `i` from what the page sent: `index` picks an option and `label` is the
/// text the page showed for it (they must agree, so a number is never read against a list the
/// page did not see), `text` types one. `title` is the chat's title, as the listing used it.
/// `Err` is the refusal; nothing has been answered.
pub fn plan(
    chat_id: &str,
    title: &str,
    i: &Interaction,
    index: Option<usize>,
    label: Option<&str>,
    text: Option<&str>,
) -> Result<Plan, String> {
    let w = Waiting {
        chat_id: chat_id.to_owned(),
        chat_title: title.to_owned(),
        interaction: i.clone(),
    };
    if let Some(o) = approve::offer(&w) {
        // A permission or a plan is decided by an option the page showed, never by typed text.
        let Some(n) = index.filter(|_| text.is_none()) else {
            return Err(
                "A permission or plan is answered by choosing one of its options, by number."
                    .into(),
            );
        };
        let Some(shown) = o.ask.options.get(n).cloned() else {
            return Err(format!(
                "That prompt has {} options, so option {n} does not exist.",
                o.ask.options.len()
            ));
        };
        if label != Some(shown.as_str()) {
            return Err(OPTIONS_CHANGED.into());
        }
        return Ok(Plan {
            args: approve::answer_args(&o.ask, n),
            label: shown,
            accepts: approve::askable,
        });
    }
    if !question(i) {
        return Err(
            "That request cannot be answered from the Remote page; answer it in Helm.".into(),
        );
    }
    // The options as `view` showed them: the first few, each cut.
    let options: Vec<String> = i
        .options
        .iter()
        .flatten()
        .take(MAX_QUESTION_OPTIONS)
        .map(|l| approve::cut(l, 200))
        .collect();
    let typed_ok = i.method == InteractionMethod::Input
        || (i.allow_other == Some(true) && i.method == InteractionMethod::Select);
    let value = match (index, text) {
        (Some(n), None) if i.method == InteractionMethod::Select => {
            let shown = options
                .get(n)
                .ok_or_else(|| format!("That question has {} options.", options.len()))?;
            if label != Some(shown.as_str()) {
                return Err(OPTIONS_CHANGED.into());
            }
            // The answer is the option's own text, not its cut form.
            i.options
                .iter()
                .flatten()
                .nth(n)
                .cloned()
                .unwrap_or_default()
        }
        (None, Some(t)) if typed_ok => {
            let t = t.trim();
            if t.is_empty() || t.len() > MAX_TYPED {
                return Err(format!("The answer must be 1-{MAX_TYPED} bytes."));
            }
            t.to_owned()
        }
        _ => {
            return Err(if typed_ok {
                "Answer this question with an option number or typed text, not both.".into()
            } else {
                "Answer this question by choosing one of its options, by number.".into()
            })
        }
    };
    Ok(Plan {
        args: json!({ "chatId": chat_id, "requestId": i.id, "value": value }),
        label: approve::cut(&value, 200),
        accepts: accepts_question,
    })
}

/// `app_answer_waiting`, after the gate has checked the argument shapes.
pub async fn answer(w: &Wiring, answered: &Answered, args: &Value) -> Result<Value, ToolFailure> {
    let profile = w
        .services
        .get::<Profile>()
        .ok_or_else(|| refused("Helm's profile is not available."))?;
    if !telegram::load_remote_answers(&telegram::policy_path(&profile.dir)) {
        return Err(refused(SWITCH_OFF));
    }
    let chat = args["chatId"].as_str().unwrap_or("");
    let request = args["requestId"].as_str().unwrap_or("");
    let index = args["index"].as_u64().map(|n| n as usize);
    let label = args["label"].as_str();
    let text = args["text"].as_str();
    let rt = w
        .services
        .get::<ChatRuntime>()
        .ok_or_else(|| refused("Helm's chats are not available."))?;
    let sink = w
        .services
        .get::<ChatSink>()
        .ok_or_else(|| refused("Helm is still starting; try again in a moment."))?;
    let Some(interaction) = rt
        .live_thread(chat)
        .and_then(|t| t.interactions.into_iter().find(|i| i.id == request))
    else {
        return Err(refused(GONE));
    };
    // The title the listing showed, so the "shown in full" test is the one the page saw.
    let title = rt
        .attention()
        .ok()
        .and_then(|s| {
            s.needs_you
                .into_iter()
                .find(|r| r.chat_id == chat)
                .map(|r| r.title)
        })
        .ok_or_else(|| refused(GONE))?;
    let p = plan(chat, &title, &interaction, index, label, text).map_err(refused)?;
    if !answered.claim(chat, request, Instant::now()) {
        return Err(refused(
            "That request was answered a moment ago, so nothing more was sent.",
        ));
    }
    let c = match sink.ctx() {
        Ok(c) => c,
        Err(e) => {
            answered.release(chat, request);
            return Err(refused(e));
        }
    };
    let row = (
        ContextKind::Notice,
        format!("Answered from the Remote page: {}", p.label),
    );
    match answer_request(&c, chat, request, p.args, p.accepts, row).await {
        Ok(()) => Ok(json!({
            "answered": true,
            "chatId": chat,
            "requestId": request,
            "label": p.label,
        })),
        Err(e) => {
            answered.release(chat, request);
            Err(refused(match e {
                AnswerError::Gone => GONE.to_owned(),
                AnswerError::Failed(why) => format!("Helm could not answer it: {why}"),
            }))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use helm_chats::types::InteractionContext;

    fn interaction(
        id: &str,
        attention: InteractionAttention,
        method: InteractionMethod,
        options: &[&str],
    ) -> Interaction {
        Interaction {
            chat_id: "c1".into(),
            id: id.into(),
            method,
            attention: Some(attention),
            title: Some("Allow Bash?".into()),
            header: None,
            message: None,
            context: Some(vec![InteractionContext {
                label: "Command".into(),
                text: "cargo test".into(),
                markdown: None,
            }]),
            options: (!options.is_empty()).then(|| options.iter().map(|o| o.to_string()).collect()),
            option_descriptions: None,
            allow_other: None,
            secret: None,
            step: None,
            steps: None,
            answer: None,
            prefill: None,
        }
    }

    fn waiting(i: Interaction) -> Waiting {
        Waiting {
            chat_id: "c1".into(),
            chat_title: "Docs".into(),
            interaction: i,
        }
    }

    #[test]
    fn what_the_page_lists_and_how_each_kind_is_answered() {
        use InteractionAttention::*;
        use InteractionMethod::*;
        let allow = interaction("r1", Approval, Select, &["Allow once", "Deny", "Stop turn"]);
        let plan_i = interaction("r2", Plan, Confirm, &[]);
        let mut long = allow.clone();
        long.id = "r3".into();
        long.context.as_mut().unwrap()[0].text = "x".repeat(401);
        let mut secret = interaction("r4", Question, Input, &[]);
        secret.secret = Some(true);
        let pick = interaction("r5", Question, Select, &["red", "blue"]);
        let typed = interaction("r6", Question, Input, &[]);
        let trust = interaction("r7", Trust, Select, &["Trust"]);

        // (interaction, kind listed, labels listed, answerable with the switch on)
        for (i, kind, labels, on) in [
            (
                &allow,
                Some("permission"),
                vec!["Allow once", "Deny", "Stop turn"],
                true,
            ),
            (
                &plan_i,
                Some("plan"),
                vec!["Approve plan", "Keep planning"],
                true,
            ),
            // Too long to read on a phone: only the options that allow nothing.
            (&long, Some("permission"), vec!["Deny", "Stop turn"], true),
            (&pick, Some("question"), vec!["red", "blue"], true),
            (&typed, Some("question"), vec![], true),
            // Never listed: a secret input, a folder-trust prompt.
            (&secret, None, vec![], false),
            (&trust, None, vec![], false),
        ] {
            for switch in [true, false] {
                let v = view(&waiting(i.clone()), switch);
                assert_eq!(
                    v.as_ref().and_then(|v| v["kind"].as_str()),
                    kind,
                    "{}",
                    i.id
                );
                let Some(v) = v else { continue };
                let shown: Vec<&str> = v["options"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|o| o["label"].as_str().unwrap())
                    .collect();
                assert_eq!(shown, labels, "{}", i.id);
                assert_eq!(v["answerable"], json!(on && switch), "{} {switch}", i.id);
            }
        }
        let v = view(&waiting(allow.clone()), true).unwrap();
        assert_eq!(v["options"][0]["allows"], json!(true));
        assert_eq!(v["options"][1]["allows"], json!(false));
        assert_eq!(v["tooLong"], json!(false));
        assert_eq!(
            view(&waiting(long.clone()), true).unwrap()["tooLong"],
            json!(true)
        );
        assert_eq!(
            view(&waiting(typed.clone()), true).unwrap()["typed"],
            json!(true)
        );

        // How an answer is read: (interaction, index, text, the value sent or the refusal).
        // The label the page sends with an option's number is the one `view` showed for it.
        let shown = |i: &Interaction, n: usize| -> Option<String> {
            view(&waiting(i.clone()), true)?["options"][n]["label"]
                .as_str()
                .map(str::to_owned)
        };
        let ok = |i: &Interaction, n: Option<usize>, t: Option<&str>| {
            let label = n.and_then(|n| shown(i, n));
            plan("c1", "Docs", i, n, label.as_deref(), t)
        };
        let sent = ok(&allow, Some(0), None).unwrap();
        assert_eq!(
            sent.args,
            json!({"chatId": "c1", "requestId": "r1", "value": "Allow once"})
        );
        assert_eq!(sent.label, "Allow once");
        // A plan's "Approve plan" confirms, as its card does; a refusing option never does.
        assert_eq!(
            ok(&plan_i, Some(0), None).unwrap().args["confirmed"],
            json!(true)
        );
        assert_eq!(
            ok(&plan_i, Some(1), None).unwrap().args["confirmed"],
            json!(false)
        );
        // Long details: the allowing options are not there to choose, so index 0 is a refusal.
        assert_eq!(ok(&long, Some(0), None).unwrap().label, "Deny");
        assert!(ok(&long, Some(2), None).is_err());
        // Permissions and plans are never answered by typed text, nor by both.
        for (i, n, t) in [
            (&allow, None, Some("Allow once")),
            (&allow, Some(0), Some("Allow once")),
            (&allow, None, None),
            (&allow, Some(9), None),
            (&plan_i, None, Some("yes")),
        ] {
            assert!(ok(i, n, t).is_err(), "{} {n:?} {t:?}", i.id);
        }
        // Questions: a number picks, typed text answers a free-text one.
        assert_eq!(
            ok(&pick, Some(1), None).unwrap().args,
            json!({"chatId": "c1", "requestId": "r5", "value": "blue"})
        );
        assert_eq!(
            ok(&typed, None, Some("  hello ")).unwrap().args["value"],
            json!("hello")
        );
        for (i, n, t) in [
            (&pick, Some(5), None),
            (&pick, None, Some("purple")),
            (&typed, Some(0), None),
            (&typed, None, Some("  ")),
            (&typed, None, Some(&"x".repeat(MAX_TYPED + 1)[..])),
            (&secret, None, Some("hunter2")),
            (&trust, Some(0), None),
        ] {
            assert!(ok(i, n, t).is_err(), "{} {n:?}", i.id);
        }
        let mut other = pick.clone();
        other.allow_other = Some(true);
        assert_eq!(
            ok(&other, None, Some("purple")).unwrap().args["value"],
            json!("purple")
        );
        // Only the kind of request the answer was worked out for may be answered.
        assert!((ok(&allow, Some(0), None).unwrap().accepts)(&allow));
        assert!(!(ok(&allow, Some(0), None).unwrap().accepts)(&pick));
        assert!((ok(&pick, Some(0), None).unwrap().accepts)(&pick));
        assert!(!(ok(&pick, Some(0), None).unwrap().accepts)(&secret));

        // The label must be the option's own: a number is never read against a list the page did
        // not see. (interaction, chat title, index, label, answered)
        let by = |i: &Interaction, title: &str, n: usize, label: Option<&str>| {
            plan("c1", title, i, Some(n), label, None).is_ok()
        };
        let huge = "T".repeat(3490);
        for (i, title, n, label, want) in [
            (&allow, "Docs", 0, Some("Allow once"), true),
            (&allow, "Docs", 1, Some("Deny"), true),
            // Another option's text, none at all, or a different case: refused.
            (&allow, "Docs", 0, Some("Deny"), false),
            (&allow, "Docs", 0, None, false),
            (&allow, "Docs", 0, Some("allow once"), false),
            (&pick, "Docs", 1, Some("blue"), true),
            (&pick, "Docs", 1, Some("red"), false),
            // A title so long that the details no longer fit leaves only the refusing options,
            // and the listing and the answer must use the same title to agree on the numbers.
            (&allow, &huge, 0, Some("Allow once"), false),
            (&allow, &huge, 0, Some("Deny"), true),
        ] {
            assert_eq!(
                by(i, title, n, label),
                want,
                "{} {title:.8} {n} {label:?}",
                i.id
            );
        }
        let mut cut_option = pick.clone();
        cut_option.options = Some(vec!["a".repeat(250), "b".into()]);
        let shown_cut = "a".repeat(200) + "…";
        let p = plan("c1", "Docs", &cut_option, Some(0), Some(&shown_cut), None).unwrap();
        assert_eq!(
            p.args["value"],
            json!("a".repeat(250)),
            "the whole option is the answer, the cut one is only what was shown"
        );
    }

    #[test]
    fn a_request_is_answered_once_and_comes_back_when_the_answer_fails() {
        let a = Answered::default();
        let t0 = Instant::now();
        assert!(a.claim("c", "r", t0));
        assert!(!a.claim("c", "r", t0), "the second answer is refused");
        assert!(a.claim("c", "other", t0), "per request");
        assert!(a.claim("d", "r", t0), "per chat");
        a.release("c", "r");
        assert!(
            a.claim("c", "r", t0),
            "an answer that failed may be tried again"
        );
        assert!(
            a.claim("c", "r", t0 + ANSWERED_FOR),
            "the mark is only until the chat has caught up"
        );
    }
}
