//! Approving from Telegram.
//!
//! Off unless the user turns it on in Settings → Hub. Then a permission prompt or a plan approval
//! that waits in a chat (while the user is away, or in the Telegram chat itself) goes to Telegram
//! as a message with one button per option, and a tap answers it as the card in Helm would.
//!
//! - **Only a tap counts.** Telegram tells the hub which account tapped; the hub passes on a tap
//!   only from the allowed user in a private chat; Helm accepts it only for an ask id it issued,
//!   once, and only while that request still waits. Text from Telegram never approves anything.
//! - **What is sent.** Permission prompts, plan approvals and Allow cards. Folder-trust prompts
//!   and secret inputs never are; questions are answered by replying in the Telegram chat.
//! - **Nothing allowed unseen.** When the details are too long to show in full on the phone, the
//!   ask offers only the options that allow nothing (Deny, Stop turn, Keep planning) and says to
//!   open Helm to allow it.
//! - **Kept in step.** A request answered in Helm, withdrawn or ended has its buttons taken down.
//!   The asks live in memory: after a restart a tap on an old one says it is no longer waiting.
//! - **Every Allow card.** A card that only grants a chat permission (it changes nothing itself)
//!   is asked the same way, with Allow and Deny. Allow grants for the minutes the agent asked
//!   for, as the card in Helm does by default.
//! - **Staged settings changes too.** A request a chat staged for Helm to apply (the card that
//!   says Approve and lists every change) is asked with Approve and Deny when approving from
//!   Telegram is on and the user is away or talking to Helm over Telegram or voice. The tap goes
//!   through the same one decision as the card in Helm, so whichever answers first wins and the
//!   other is closed. The ask is Helm-issued, single use, expires with the request, and is
//!   accepted only from the account of the person who owns the chat. When the lines do not fit in
//!   one message only Deny is offered, with a note to open Helm.
//! - **A chat may ask too.** `app_telegram_ask` puts a chat's own question to the user with
//!   buttons ("May I merge and redeploy?"); the tapped option comes back to that chat as a note
//!   from Helm saying it was tapped on the user's own Telegram account.

use helm_chats::types::{Interaction, InteractionAttention, InteractionMethod};
use helm_consent::{ConsentRequest, RequestState};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Mutex;

/// Telegram shows at most this many buttons for one ask.
pub const MAX_OPTIONS: usize = 4;

/// One ask on Telegram: the request it answers and the buttons it offered.
#[derive(Clone, Debug, PartialEq)]
pub struct Ask {
    pub chat_id: String,
    /// The hub person whose Telegram this was asked on (`""`: none named). Taken down there.
    pub person: String,
    pub request_id: String,
    pub method: InteractionMethod,
    pub plan: bool,
    pub options: Vec<String>,
    /// For a yes/no confirmation: the label that answers yes (it may not be among `options`).
    pub yes: Option<String>,
    /// A chat's own question (`app_telegram_ask`); `None` for a waiting request.
    pub question: Option<String>,
    /// A staged settings change awaiting Approve: `request_id` is the consent request's.
    pub consent: bool,
    /// When the ask stops being usable (ms since the epoch); `None` while its request waits.
    pub expires_at: Option<i64>,
}

/// The buttons of a card: Approve for a change Helm applies, Allow for a permission only.
pub const APPROVE: &str = "Approve";
pub const ALLOW: &str = "Allow";
pub const DENY: &str = "Deny";
/// The most characters of a staged change shown in full; a longer one offers only Deny.
const CONSENT_FIT: usize = 3000;

/// An Allow card waiting for an answer, as the watcher found it.
#[derive(Clone, Debug)]
pub struct ConsentWaiting {
    pub chat_id: String,
    pub chat_title: String,
    /// The hub person of the profile that owns the chat (`""`: none linked).
    pub person: String,
    pub request: ConsentRequest,
}

/// A request waiting in a chat, as the watcher found it.
#[derive(Clone, Debug)]
pub struct Waiting {
    pub chat_id: String,
    pub chat_title: String,
    /// The hub person of the profile that owns the chat (`""`: none linked).
    pub person: String,
    pub interaction: Interaction,
}

/// What one look decided: asks to send (`id`, person, text, options) and asks to take down
/// (`id`, person, note).
#[derive(Debug, Default, PartialEq)]
pub struct Step {
    pub ask: Vec<(String, String, String, Vec<String>)>,
    pub done: Vec<(String, String, String)>,
}

/// Whether a waiting request may be offered on Telegram.
pub fn askable(i: &Interaction) -> bool {
    matches!(
        i.attention,
        Some(InteractionAttention::Approval) | Some(InteractionAttention::Plan)
    ) && matches!(
        i.method,
        InteractionMethod::Select | InteractionMethod::Confirm
    ) && i.secret != Some(true)
        && !options_of(i).is_empty()
}

/// The buttons: the request's own options, or for a yes/no confirmation its two answers.
fn options_of(i: &Interaction) -> Vec<String> {
    let own: Vec<String> = i.options.clone().unwrap_or_default();
    let mut o = if !own.is_empty() {
        own
    } else if i.method == InteractionMethod::Confirm {
        if i.attention == Some(InteractionAttention::Plan) {
            vec!["Approve plan".to_owned(), "Keep planning".to_owned()]
        } else {
            vec!["Allow".to_owned(), "Deny".to_owned()]
        }
    } else {
        vec![]
    };
    o.truncate(MAX_OPTIONS);
    o
}

pub(crate) fn cut(s: &str, max: usize) -> String {
    let mut it = s.chars();
    let out: String = it.by_ref().take(max).collect();
    if it.next().is_some() {
        format!("{out}…")
    } else {
        out
    }
}

/// The options that allow nothing, offered when the details could not all be shown.
fn refusing(options: &[String]) -> Vec<String> {
    options
        .iter()
        .filter(|o| {
            let l = o.to_lowercase();
            ["deny", "stop", "keep planning", "reject", "cancel"]
                .iter()
                .any(|w| l.starts_with(w))
        })
        .cloned()
        .collect()
}

/// The ask's text: which chat, what it wants, and a short look at the details, and whether all
/// of the details fit.
pub fn ask_text(chat_title: &str, i: &Interaction) -> (String, bool) {
    let mut full = true;
    // A plan is its message, so it gets most of a Telegram message (the hub's limit is 3900).
    let max = if i.attention == Some(InteractionAttention::Plan) {
        2800
    } else {
        600
    };
    let what = if i.attention == Some(InteractionAttention::Plan) {
        "wants its plan approved"
    } else {
        "needs your approval"
    };
    let title = chat_title.trim();
    let mut s = format!(
        "{} {what}:",
        if title.is_empty() {
            "A Helm chat"
        } else {
            title
        }
    );
    for part in [i.title.as_deref(), i.message.as_deref()]
        .into_iter()
        .flatten()
    {
        let p = part.trim();
        if !p.is_empty() && !s.contains(p) {
            full &= p.chars().count() <= max;
            s.push('\n');
            s.push_str(&cut(p, max));
        }
    }
    let rows: Vec<_> = i.context.iter().flatten().collect();
    full &= rows.len() <= 3;
    for c in rows.iter().take(3) {
        let t = c.text.trim();
        if !t.is_empty() {
            full &= t.chars().count() <= 400;
            s.push_str(&format!("\n{}: {}", c.label.trim(), cut(t, 400)));
        }
    }
    full &= s.chars().count() <= 3500;
    (cut(&s, 3500), full)
}

/// What one waiting request shows on a small screen: its text, and the ask (the options it may
/// offer). Shared by Telegram's buttons and the Remote page, so both show, and refuse, alike.
#[derive(Clone, Debug, PartialEq)]
pub struct Offer {
    pub ask: Ask,
    pub text: String,
    /// Whether the details fit in full. When they do not, only options that allow nothing remain.
    pub full: bool,
}

/// The offer for a waiting request, or `None` when it is not askable or nothing could be offered.
pub fn offer(w: &Waiting) -> Option<Offer> {
    if !askable(&w.interaction) {
        return None;
    }
    let all = options_of(&w.interaction);
    let (mut text, full) = ask_text(&w.chat_title, &w.interaction);
    let options = if full {
        all.clone()
    } else {
        text.push_str("\n\nToo long to show in full here: open Helm to allow it.");
        refusing(&all)
    };
    if options.is_empty() {
        return None;
    }
    let ask = Ask {
        chat_id: w.chat_id.clone(),
        person: w.person.clone(),
        request_id: w.interaction.id.clone(),
        method: w.interaction.method,
        plan: w.interaction.attention == Some(InteractionAttention::Plan),
        options,
        // The label that answers yes, found by name: a refusing button left on its own never
        // becomes it.
        yes: (w.interaction.method == InteractionMethod::Confirm)
            .then(|| {
                all.iter()
                    .find(|o| matches!(o.as_str(), "Approve plan" | "Allow"))
                    .cloned()
            })
            .flatten(),
        question: None,
        consent: false,
        expires_at: None,
    };
    Some(Offer { ask, text, full })
}

/// The offer for an Allow card. A change Helm applies on Approve lists every line of what
/// changes, then offers Approve and Deny; a plain permission offers Allow and Deny. Nothing is
/// allowed unseen: when the lines do not fit in one message only Deny is offered.
pub fn consent_offer(w: &ConsentWaiting) -> Option<Offer> {
    let r = &w.request;
    if r.state != RequestState::Pending {
        return None;
    }
    let title = cut(w.chat_title.trim(), 60);
    // A chat's title is its own to rename: it is quoted, never the start of the sentence.
    let who = if title.is_empty() {
        "A Helm chat".to_owned()
    } else {
        format!("The chat \"{}\"", title.replace('"', "'"))
    };
    let (mut text, yes, open) = if r.applies_on_allow {
        (
            format!(
                "{who} asks for this change and needs your approval:\n{}",
                r.summary.trim()
            ),
            APPROVE,
            "approve",
        )
    } else {
        (
            format!(
                "{who} asks for your Allow: {}\n{}",
                r.label.trim(),
                r.summary.trim()
            ),
            ALLOW,
            "allow",
        )
    };
    let full = text.chars().count() <= CONSENT_FIT;
    let options = if full {
        vec![yes.to_owned(), DENY.to_owned()]
    } else {
        text = cut(&text, CONSENT_FIT);
        text.push_str(&format!(
            "\n\nToo long to show in full here: open Helm to {open} it."
        ));
        vec![DENY.to_owned()]
    };
    let ask = Ask {
        chat_id: w.chat_id.clone(),
        person: w.person.clone(),
        request_id: r.id.clone(),
        method: InteractionMethod::Select,
        plan: false,
        options,
        yes: full.then(|| yes.to_owned()),
        question: None,
        consent: true,
        expires_at: Some(r.expires_at),
    };
    Some(Offer { ask, text, full })
}

/// What the buttons of a staged change are told when its request left the waiting list: where it
/// was answered, or that it expired.
pub fn settled_note(r: Option<&ConsentRequest>, now: i64) -> String {
    let Some(r) = r else {
        return "No longer waiting: withdrawn.".into();
    };
    match (r.state, r.decided_via.as_deref()) {
        (RequestState::Pending, _) if r.expires_at <= now => {
            "Expired: it was not answered in time.".into()
        }
        (RequestState::Pending, _) => "Still waiting in Helm.".into(),
        (_, Some("telegram")) => "Answered on Telegram.".into(),
        (_, Some("app")) => "Answered in Helm.".into(),
        (_, Some(v)) if v.starts_with("device:") => "Answered on another device.".into(),
        (RequestState::Expired, _) => "Expired: it was not answered in time.".into(),
        _ => "No longer waiting.".into(),
    }
}

/// Whether choosing `label` lets something happen (as opposed to refusing it).
pub fn allows(label: &str) -> bool {
    refusing(&[label.to_owned()]).is_empty()
}

/// What the tapped option sends, as the card in Helm would: a confirmation answers yes for the
/// first button; a plan's option also confirms when it is "Approve plan".
pub fn answer_args(ask: &Ask, index: usize) -> Value {
    let label = &ask.options[index];
    let mut a = json!({ "chatId": ask.chat_id, "requestId": ask.request_id });
    if ask.method == InteractionMethod::Confirm {
        a["confirmed"] = json!(ask.yes.as_deref() == Some(label.as_str()));
    } else {
        a["value"] = json!(label);
        if ask.plan {
            a["confirmed"] = json!(label == "Approve plan");
        }
    }
    a
}

/// The open asks, by id.
#[derive(Default)]
pub struct Asks(Mutex<HashMap<String, Ask>>);

impl Asks {
    fn map(&self) -> std::sync::MutexGuard<'_, HashMap<String, Ask>> {
        self.0.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// One look at what waits now: ask what is new, take down asks whose request no longer
    /// waits. `new_id` makes a fresh, unguessable id.
    pub fn step(&self, waiting: &[Waiting], mut new_id: impl FnMut() -> String) -> Step {
        let mut m = self.map();
        let mut step = Step::default();
        let live: Vec<(&str, &str)> = waiting
            .iter()
            .map(|w| (w.chat_id.as_str(), w.interaction.id.as_str()))
            .collect();
        let gone: Vec<(String, String)> = m
            .iter()
            .filter(|(_, a)| {
                a.question.is_none()
                    && !a.consent
                    && !live.contains(&(a.chat_id.as_str(), a.request_id.as_str()))
            })
            .map(|(id, a)| (id.clone(), a.person.clone()))
            .collect();
        for (id, person) in gone {
            m.remove(&id);
            step.done.push((
                id,
                person,
                "No longer waiting: answered in Helm, or withdrawn.".into(),
            ));
        }
        for w in waiting.iter().filter(|w| askable(&w.interaction)) {
            let asked = m
                .values()
                .any(|a| a.chat_id == w.chat_id && a.request_id == w.interaction.id);
            if asked {
                continue;
            }
            let Some(o) = offer(w) else {
                continue;
            };
            let id = new_id();
            let person = o.ask.person.clone();
            step.ask
                .push((id.clone(), person, o.text, o.ask.options.clone()));
            m.insert(id, o.ask);
        }
        step
    }

    /// One look at the staged settings changes that wait: ask what is new, and take down the asks
    /// of requests that are no longer pending, each with its note from `settled` (by request id).
    pub fn step_consent(
        &self,
        waiting: &[ConsentWaiting],
        settled: &HashMap<String, String>,
        mut new_id: impl FnMut() -> String,
    ) -> Step {
        let mut m = self.map();
        let mut step = Step::default();
        let gone: Vec<(String, String, String)> = m
            .iter()
            .filter(|(_, a)| a.consent && !waiting.iter().any(|w| w.request.id == a.request_id))
            .map(|(id, a)| {
                let note = settled
                    .get(&a.request_id)
                    .cloned()
                    .unwrap_or_else(|| "No longer waiting.".into());
                (id.clone(), a.person.clone(), note)
            })
            .collect();
        for (id, person, note) in gone {
            m.remove(&id);
            step.done.push((id, person, note));
        }
        for w in waiting {
            if m.values()
                .any(|a| a.consent && a.request_id == w.request.id)
            {
                continue;
            }
            let Some(o) = consent_offer(w) else {
                continue;
            };
            let id = new_id();
            step.ask.push((
                id.clone(),
                o.ask.person.clone(),
                o.text,
                o.ask.options.clone(),
            ));
            m.insert(id, o.ask);
        }
        step
    }

    /// The consent request ids that have an open ask.
    pub fn consent_ids(&self) -> Vec<String> {
        self.map()
            .values()
            .filter(|a| a.consent)
            .map(|a| a.request_id.clone())
            .collect()
    }

    /// A chat's own question, put to the user with buttons. Kept until a tap or a restart.
    pub fn add_question(
        &self,
        id: &str,
        chat_id: &str,
        person: &str,
        question: &str,
        options: Vec<String>,
    ) {
        self.map().insert(
            id.to_owned(),
            Ask {
                chat_id: chat_id.to_owned(),
                person: person.to_owned(),
                request_id: String::new(),
                method: InteractionMethod::Select,
                plan: false,
                options,
                yes: None,
                question: Some(question.to_owned()),
                consent: false,
                expires_at: None,
            },
        );
    }

    /// An ask the hub could not send: forget it, so the next look asks again.
    pub fn unsent(&self, id: &str) {
        self.map().remove(id);
    }

    /// Take every open ask down (approvals were turned off, or the line went away), with the
    /// person each was asked on. With `questions` false, chats' own questions stay: they are
    /// not asked again.
    pub fn clear(&self, questions: bool) -> Vec<(String, String)> {
        let mut m = self.map();
        let ids: Vec<(String, String)> = m
            .iter()
            .filter(|(_, a)| questions || a.question.is_none())
            .map(|(id, a)| (id.clone(), a.person.clone()))
            .collect();
        for (id, _) in &ids {
            m.remove(id);
        }
        ids
    }

    /// The user's tap, from the account of `who` (the hub person the hub vouches for). Single
    /// use: once the ask is accepted, refused as expired or found to have no such button, it is
    /// gone. A tap from another person's account leaves it alone, so it cannot burn the ask.
    pub fn tap(&self, id: &str, index: usize, who: &str, now: i64) -> Tap {
        let mut m = self.map();
        match m.get(id) {
            None => return Tap::Gone,
            Some(a) if a.person != who => return Tap::Foreign,
            Some(_) => {}
        }
        let Some(ask) = m.remove(id) else {
            return Tap::Gone;
        };
        if ask.expires_at.is_some_and(|e| e <= now) {
            return Tap::Expired;
        }
        if index >= ask.options.len() {
            return Tap::Gone;
        }
        Tap::Taken(ask)
    }
}

/// What a tap found.
#[derive(Debug, PartialEq)]
pub enum Tap {
    /// The ask is accepted (and spent).
    Taken(Ask),
    /// No such open ask: never issued, already used, taken down, or lost with a restart.
    Gone,
    /// The ask belongs to another person.
    Foreign,
    /// The ask's time was up.
    Expired,
}

/// Answer a staged settings change from the button at `index` through the one decision path the
/// card in Helm uses. The first answer wins; a later one is a `conflict`.
pub fn decide_consent(
    services: &std::sync::Arc<helm_ipc::Services>,
    events: &std::sync::Arc<dyn helm_ipc::EventSink>,
    ask: &Ask,
    index: usize,
) -> Result<helm_consent::apply::Decided, helm_ipc::ApiError> {
    // A change to this PC is the PC owner's to approve, never a household member's, whatever
    // account asked: only their own chats' changes are offered, and this checks it again.
    if !services.get::<helm_core::db::Database>().is_some_and(|db| {
        db.snapshot(|tx| {
            Ok::<_, rusqlite::Error>(!helm_profiles::store::is_household_member(tx, &ask.chat_id))
        })
        .unwrap_or(false)
    }) {
        return Err(helm_ipc::ApiError::new(
            "forbidden",
            "Only this PC's owner can approve a change to it.",
        ));
    }
    let decision = if ask.options.get(index).map(String::as_str) == Some(APPROVE)
        || ask.options.get(index).map(String::as_str) == Some(ALLOW)
    {
        helm_consent::Decision::Allow
    } else {
        helm_consent::Decision::Deny
    };
    helm_consent::decide_and_apply(
        services,
        events,
        &helm_consent::ConsentDecideArgs {
            request_id: ask.request_id.clone(),
            decision,
            grant_minutes: None,
            note: None,
        },
        "telegram",
    )
}

/// The note a chat gets when the user taps an option of its question.
pub fn answer_note(id: &str, question: &str, label: &str) -> String {
    format!(
        "[Answer from Telegram] The user tapped \"{label}\" on their own Telegram account, \
         answering your question \"{question}\" (ask {id}). Helm checked that the tap came from \
         the user's account, for this question, once."
    )
}

/// A fresh ask id: 32 hex digits from a random UUID.
pub fn new_id() -> String {
    uuid::Uuid::new_v4().simple().to_string()
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
            gate: None,
        }
    }

    /// A tap from the person the asks are for, long before anything expires.
    fn take(asks: &Asks, id: &str, index: usize) -> Option<Ask> {
        match asks.tap(id, index, "p7", 0) {
            Tap::Taken(a) => Some(a),
            _ => None,
        }
    }

    fn waiting(i: Interaction) -> Waiting {
        Waiting {
            chat_id: "c1".into(),
            chat_title: "Docs".into(),
            person: "p7".into(),
            interaction: i,
        }
    }

    #[test]
    fn only_approvals_and_plans_are_asked_and_a_tap_answers_once() {
        use InteractionAttention::*;
        use InteractionMethod::*;
        let approval = interaction(
            "r1",
            Approval,
            Select,
            &["Allow once", "Allow for this session", "Deny", "Stop turn"],
        );
        // (interaction, askable)
        for (i, want) in [
            (approval.clone(), true),
            (interaction("r2", Plan, Confirm, &[]), true),
            (interaction("r3", Trust, Select, &["Trust"]), false),
            (interaction("r4", Question, Select, &["A", "B"]), false),
            (interaction("r5", Approval, Input, &[]), false),
        ] {
            assert_eq!(askable(&i), want, "{}", i.id);
        }
        assert_eq!(
            ask_text("Docs", &approval),
            (
                "Docs needs your approval:\nAllow Bash?\nCommand: cargo test".to_owned(),
                true
            )
        );
        // Details too long for the phone: only the options that allow nothing are offered.
        let mut long = approval.clone();
        long.id = "r9".into();
        long.context.as_mut().unwrap()[0].text = "x".repeat(401);
        let s = Asks::default().step(&[waiting(long)], new_id);
        assert_eq!(s.ask[0].1, "p7", "the ask carries the chat owner's person");
        assert_eq!(s.ask[0].3, ["Deny", "Stop turn"]);
        assert!(
            s.ask[0].2.ends_with("open Helm to allow it."),
            "{}",
            s.ask[0].2
        );

        let asks = Asks::default();
        let mut n = 0;
        let mut id = || {
            n += 1;
            format!("ask{n}")
        };
        let plan = interaction("r2", Plan, Confirm, &[]);
        let both = [waiting(approval.clone()), waiting(plan.clone())];
        let s = asks.step(&both, &mut id);
        assert_eq!(s.ask.len(), 2);
        assert_eq!(s.ask[1].3, ["Approve plan", "Keep planning"]);
        assert!(asks.step(&both, &mut id).ask.is_empty(), "asked once");
        // The plan was answered in Helm: its buttons come down.
        let s = asks.step(&[waiting(approval.clone())], &mut id);
        assert_eq!(s.done.len(), 1);
        assert_eq!(s.done[0].0, "ask2");
        // A tap: an index past the buttons answers nothing; a good one answers once.
        assert!(take(&asks, "nope", 0).is_none());
        let ask = take(&asks, "ask1", 0).unwrap();
        assert_eq!(
            answer_args(&ask, 0),
            json!({"chatId": "c1", "requestId": "r1", "value": "Allow once"})
        );
        assert!(take(&asks, "ask1", 0).is_none(), "single use");
        // Confirmations and plans answer as the card does.
        let confirm = Ask {
            method: Confirm,
            plan: true,
            options: vec!["Approve plan".into(), "Keep planning".into()],
            yes: Some("Approve plan".into()),
            ..ask.clone()
        };
        assert_eq!(answer_args(&confirm, 1)["confirmed"], false);
        assert_eq!(answer_args(&confirm, 0)["confirmed"], true);
        // With only the refusing option left, its index 0 still answers no.
        let refused = Ask {
            options: vec!["Keep planning".into()],
            ..confirm.clone()
        };
        assert_eq!(answer_args(&refused, 0)["confirmed"], false);
        let plan_select = Ask {
            method: Select,
            plan: true,
            options: vec!["Approve plan".into(), "Keep planning".into()],
            ..ask
        };
        assert_eq!(
            answer_args(&plan_select, 0),
            json!({"chatId": "c1", "requestId": "r1", "value": "Approve plan", "confirmed": true})
        );
        assert_eq!(new_id().len(), 32);
        // A chat's own question is not taken down by looks or by clearing waiting asks.
        let asks = Asks::default();
        asks.add_question(
            "q1",
            "c9",
            "p7",
            "Merge and redeploy?",
            vec!["Yes".into(), "No".into()],
        );
        assert!(asks.step(&[], new_id).done.is_empty());
        assert!(asks.clear(false).is_empty());
        let q = take(&asks, "q1", 0).unwrap();
        assert_eq!(q.question.as_deref(), Some("Merge and redeploy?"));
        assert!(answer_note("q1", "Merge and redeploy?", "Yes").contains("tapped \"Yes\""));
    }

    struct Recorder(Mutex<Vec<String>>);

    impl helm_consent::Applier for Recorder {
        fn apply(&self, job: &helm_consent::ApplyJob<'_>) -> String {
            self.0.lock().unwrap().push(job.request_id.to_owned());
            "Done: changed.".into()
        }
        fn staged_with(&self) -> &'static str {
            "app_stage_test"
        }
    }

    fn staged_kind() -> helm_consent::AllowKind {
        fn canonical(
            _: &rusqlite::Transaction<'_>,
            _: &str,
            t: &Value,
        ) -> Result<helm_consent::Canonical, String> {
            let name = t["name"].as_str().ok_or("name")?;
            Ok(helm_consent::Canonical {
                target: json!({ "name": name }),
                summary: format!("Change 1 setting\nName: none → {name}"),
                fixed_form: String::new(),
                details: vec![],
            })
        }
        helm_consent::AllowKind {
            id: "test_apply",
            label: "Change settings",
            accepts_words: false,
            canonical,
        }
    }

    fn kind_of(t: &Tap) -> &'static str {
        match t {
            Tap::Taken(_) => "taken",
            Tap::Gone => "gone",
            Tap::Foreign => "foreign",
            Tap::Expired => "expired",
        }
    }

    /// The right account of the person who owns the chat taps Approve or Deny on the ask for a
    /// staged settings change; the first answer, there or in Helm, wins, and the loser is told.
    #[test]
    fn a_staged_change_is_approved_once_by_the_owner_and_the_first_answer_wins() {
        struct Row {
            case: &'static str,
            /// The hub person whose account taps.
            who: &'static str,
            /// How late the tap comes after the ask's expiry (`None`: in time).
            late: Option<i64>,
            /// The button: 0 is Approve (or Deny when only Deny is offered).
            index: usize,
            /// Whether the details do not fit on the phone.
            long: bool,
            /// Answered in Helm before the tap.
            app_first: bool,
            /// The chat belongs to a household member's profile.
            member: bool,
            tap: &'static str,
            /// What deciding on the tap says: `ok`, `conflict`, `forbidden` or `-` (no decision attempted).
            decided: &'static str,
            applied: usize,
            state: RequestState,
            note: &'static str,
        }
        let rows = [
            Row {
                case: "the owner approves",
                who: "p7",
                late: None,
                index: 0,
                long: false,
                app_first: false,
                member: false,
                tap: "taken",
                decided: "ok",
                applied: 1,
                state: RequestState::Used,
                note: "Answered on Telegram.",
            },
            Row {
                case: "the owner denies",
                who: "p7",
                late: None,
                index: 1,
                long: false,
                app_first: false,
                member: false,
                tap: "taken",
                decided: "ok",
                applied: 0,
                state: RequestState::Denied,
                note: "Answered on Telegram.",
            },
            Row {
                case: "a household member's chat, from their own account",
                who: "p7",
                late: None,
                index: 0,
                long: false,
                app_first: false,
                member: true,
                tap: "taken",
                decided: "forbidden",
                applied: 0,
                state: RequestState::Pending,
                note: "Still waiting in Helm.",
            },
            Row {
                case: "another person's account",
                who: "p8",
                late: None,
                index: 0,
                long: false,
                app_first: false,
                member: false,
                tap: "foreign",
                decided: "-",
                applied: 0,
                state: RequestState::Pending,
                note: "Still waiting in Helm.",
            },
            Row {
                case: "after the ask expired",
                who: "p7",
                late: Some(1),
                index: 0,
                long: false,
                app_first: false,
                member: false,
                tap: "expired",
                decided: "-",
                applied: 0,
                state: RequestState::Pending,
                note: "Still waiting in Helm.",
            },
            Row {
                case: "answered in Helm first",
                who: "p7",
                late: None,
                index: 0,
                long: false,
                app_first: true,
                member: false,
                tap: "taken",
                decided: "conflict",
                applied: 1,
                state: RequestState::Used,
                note: "Answered in Helm.",
            },
            Row {
                case: "too long to show: only Deny",
                who: "p7",
                late: None,
                index: 0,
                long: true,
                app_first: false,
                member: false,
                tap: "taken",
                decided: "ok",
                applied: 0,
                state: RequestState::Denied,
                note: "Answered on Telegram.",
            },
        ];
        for row in rows {
            let dir = tempfile::tempdir().unwrap();
            let mut reg = helm_core::db::MigrationRegistry::new();
            helm_chats::register_migrations(&mut reg);
            helm_consent::register_migrations(&mut reg);
            helm_profiles::register_migrations(&mut reg);
            let db = std::sync::Arc::new(
                helm_core::db::Database::open(&dir.path().join("t.db"), &reg).unwrap(),
            );
            db.write::<_, helm_profiles::store::ProfileError>(|tx| {
                tx.execute(
                    "INSERT INTO projects(id, name, created_at) VALUES ('p', 'P', 1)",
                    [],
                )?;
                tx.execute(
                    "INSERT INTO chats(id, project_id, created_at, updated_at) VALUES ('c1', 'p', 1, 1)",
                    [],
                )?;
                helm_profiles::store::ensure_home(tx, 1)?;
                if row.member {
                    let alex = helm_profiles::store::create(tx, "Alex", None, Some("9"), 2)?;
                    helm_profiles::store::assign(tx, &["c1".into()], &alex.id)?;
                }
                Ok(())
            })
            .unwrap();
            let applied = std::sync::Arc::new(Recorder(Mutex::new(vec![])));
            let mut kinds = helm_consent::AllowKinds::new();
            kinds.register_applying(staged_kind(), applied.clone());
            let kinds = std::sync::Arc::new(kinds);
            let mut services = helm_ipc::Services::new();
            services.insert(db.clone());
            services.insert(kinds.clone());
            services.insert(std::sync::Arc::new(helm_consent::MaintenanceLock::default()));
            let services = std::sync::Arc::new(services);
            let events: std::sync::Arc<dyn helm_ipc::EventSink> =
                std::sync::Arc::new(helm_ipc::RecordingSink::default());

            let now = helm_core::now_ms();
            let target = json!({ "name": "alpha" });
            let request = helm_consent::store::request(
                &db,
                &kinds,
                helm_consent::NewRequest {
                    chat_id: "c1",
                    kind: "test_apply",
                    target: &target,
                    reason: "The user asked.",
                    evidence: "",
                    grant_minutes: None,
                    tool_use_id: None,
                },
                now,
            )
            .unwrap();
            let mut shown = request.clone();
            if row.long {
                shown.summary = format!("Change 1 setting\n{}", "x".repeat(4000));
            }
            let waiting = [ConsentWaiting {
                chat_id: "c1".into(),
                chat_title: "Docs".into(),
                person: "p7".into(),
                request: shown,
            }];
            let asks = Asks::default();
            let s = asks.step_consent(&waiting, &HashMap::new(), || "ask1".into());
            let c = row.case;
            assert_eq!(s.ask.len(), 1, "{c}");
            assert_eq!(
                s.ask[0].3,
                if row.long {
                    vec!["Deny"]
                } else {
                    vec!["Approve", "Deny"]
                },
                "{c}"
            );
            assert_eq!(
                s.ask[0].2.ends_with("open Helm to approve it."),
                row.long,
                "{c}"
            );
            assert!(s.ask[0].2.contains("Name: none → alpha") || row.long, "{c}");
            assert!(
                asks.step_consent(&waiting, &HashMap::new(), || "ask2".into())
                    .ask
                    .is_empty(),
                "{c}: asked once"
            );

            let decide_in_helm = || {
                helm_consent::decide_and_apply(
                    &services,
                    &events,
                    &helm_consent::ConsentDecideArgs {
                        request_id: request.id.clone(),
                        decision: helm_consent::Decision::Allow,
                        grant_minutes: None,
                        note: None,
                    },
                    "app",
                )
            };
            if row.app_first {
                decide_in_helm().unwrap().applying.unwrap().join().unwrap();
            }
            let at = request.expires_at + row.late.unwrap_or(-1_000);
            let tapped = asks.tap("ask1", row.index, row.who, at);
            assert_eq!(kind_of(&tapped), row.tap, "{c}");
            if let Tap::Taken(ask) = tapped {
                let r = decide_consent(&services, &events, &ask, row.index);
                match (&r, row.decided) {
                    (Ok(_), "ok") => {}
                    (Err(e), "conflict") => assert_eq!(e.code, "conflict", "{c}"),
                    (Err(e), "forbidden") => assert_eq!(e.code, "forbidden", "{c}"),
                    _ => panic!("{c}: unexpected decision {:?}", r.as_ref().err()),
                }
                if let Ok(d) = r {
                    if let Some(h) = d.applying {
                        h.join().unwrap();
                    }
                }
                // Single use: the same ask answers nothing again.
                assert_eq!(
                    kind_of(&asks.tap("ask1", row.index, row.who, at)),
                    "gone",
                    "{c}"
                );
            } else {
                assert_eq!(row.decided, "-", "{c}");
                // A tap that did not count may leave the ask for its owner, but never spends the
                // request.
                assert_eq!(
                    asks.consent_ids().len(),
                    usize::from(row.tap == "foreign"),
                    "{c}"
                );
            }

            assert_eq!(applied.0.lock().unwrap().len(), row.applied, "{c}");
            let after = helm_consent::store::get(&db, &kinds, &request.id)
                .unwrap()
                .unwrap();
            assert_eq!(after.state, row.state, "{c}");
            // The buttons that remain come down with a note saying where it was answered.
            let note = settled_note(Some(&after), request.expires_at - 1_000);
            assert_eq!(note, row.note, "{c}");
            if row.tap == "foreign" {
                let s = asks.step_consent(
                    &[],
                    &HashMap::from([(request.id.clone(), note)]),
                    String::new,
                );
                assert_eq!(s.done.len(), 1, "{c}");
                assert_eq!(s.done[0].2, row.note, "{c}");
            }
        }
    }

    /// A plain permission card (Helm applies nothing) goes to Telegram too: Allow and Deny with
    /// the whole text, a tap on Allow answers as the request's own grant, a text past the limit
    /// offers Deny only, and a household member's chat is refused at the decision.
    #[test]
    fn a_plain_allow_card_is_offered_whole_and_allowed_once_for_its_own_grant() {
        struct Row {
            case: &'static str,
            long: bool,
            member: bool,
        }
        let rows = [
            Row {
                case: "the owner's chat",
                long: false,
                member: false,
            },
            Row {
                case: "too long to show: only Deny",
                long: true,
                member: false,
            },
            Row {
                case: "a household member's chat",
                long: false,
                member: true,
            },
        ];
        for row in rows {
            let c = row.case;
            let dir = tempfile::tempdir().unwrap();
            let mut reg = helm_core::db::MigrationRegistry::new();
            helm_chats::register_migrations(&mut reg);
            helm_consent::register_migrations(&mut reg);
            helm_profiles::register_migrations(&mut reg);
            let db = std::sync::Arc::new(
                helm_core::db::Database::open(&dir.path().join("t.db"), &reg).unwrap(),
            );
            db.write::<_, helm_profiles::store::ProfileError>(|tx| {
                tx.execute(
                    "INSERT INTO projects(id, name, created_at) VALUES ('p', 'P', 1)",
                    [],
                )?;
                tx.execute(
                    "INSERT INTO chats(id, project_id, created_at, updated_at) VALUES ('c1', 'p', 1, 1)",
                    [],
                )?;
                helm_profiles::store::ensure_home(tx, 1)?;
                if row.member {
                    let alex = helm_profiles::store::create(tx, "Alex", None, Some("9"), 2)?;
                    helm_profiles::store::assign(tx, &["c1".into()], &alex.id)?;
                }
                Ok(())
            })
            .unwrap();
            let mut kinds = helm_consent::AllowKinds::new();
            kinds.register(helm_consent::kinds::test_kind());
            let kinds = std::sync::Arc::new(kinds);
            let mut services = helm_ipc::Services::new();
            services.insert(db.clone());
            services.insert(kinds.clone());
            let services = std::sync::Arc::new(services);
            let events: std::sync::Arc<dyn helm_ipc::EventSink> =
                std::sync::Arc::new(helm_ipc::RecordingSink::default());
            let target = json!({ "name": "alpha" });
            let request = helm_consent::store::request(
                &db,
                &kinds,
                helm_consent::NewRequest {
                    chat_id: "c1",
                    kind: "test_action",
                    target: &target,
                    reason: "The user asked.",
                    evidence: "",
                    grant_minutes: Some(45),
                    tool_use_id: None,
                },
                helm_core::now_ms(),
            )
            .unwrap();
            assert!(!request.applies_on_allow, "{c}");
            let mut shown = request.clone();
            if row.long {
                shown.summary = "x".repeat(4000);
            }
            let waiting = ConsentWaiting {
                chat_id: "c1".into(),
                chat_title: "Docs".into(),
                person: "p7".into(),
                request: shown,
            };
            let offer = consent_offer(&waiting).unwrap();
            assert_eq!(offer.full, !row.long, "{c}");
            if row.long {
                assert_eq!(offer.ask.options, vec!["Deny"], "{c}");
                assert!(offer.text.ends_with("open Helm to allow it."), "{c}");
                continue;
            }
            assert_eq!(offer.ask.options, vec!["Allow", "Deny"], "{c}");
            assert_eq!(
                offer.text,
                "The chat \"Docs\" asks for your Allow: Run a test action\nRun the test action alpha",
                "{c}"
            );
            let r = decide_consent(&services, &events, &offer.ask, 0);
            let after = helm_consent::store::get(&db, &kinds, &request.id)
                .unwrap()
                .unwrap();
            if row.member {
                assert_eq!(r.err().map(|e| e.code), Some("forbidden".into()), "{c}");
                assert_eq!(after.state, RequestState::Pending, "{c}");
                continue;
            }
            assert!(r.unwrap().applying.is_none(), "{c}: nothing to apply");
            assert_eq!(after.state, RequestState::Allowed, "{c}");
            assert_eq!(after.decided_via.as_deref(), Some("telegram"), "{c}");
            assert_eq!(after.grant_minutes, 45, "{c}");
        }
    }
}
