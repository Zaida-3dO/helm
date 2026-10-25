//! Approving from Telegram.
//!
//! Off unless the user turns it on in Settings → Hub. Then a permission prompt or a plan approval
//! that waits in a chat (while the user is away, or in the Telegram chat itself) goes to Telegram
//! as a message with one button per option, and a tap answers it as the card in Helm would.
//!
//! - **Only a tap counts.** Telegram tells the hub which account tapped; the hub passes on a tap
//!   only from the allowed user in a private chat; Helm accepts it only for an ask id it issued,
//!   once, and only while that request still waits. Text from Telegram never approves anything.
//! - **What is sent.** Permission prompts and plan approvals. Folder-trust prompts and secret
//!   inputs never are; questions are answered by replying in the Telegram chat.
//! - **Kept in step.** A request answered in Helm, withdrawn or ended has its buttons taken down.
//!   The asks live in memory: after a restart a tap on an old one says it is no longer waiting.

use helm_chats::types::{Interaction, InteractionAttention, InteractionMethod};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Mutex;

/// Telegram shows at most this many buttons for one ask.
pub const MAX_OPTIONS: usize = 4;

/// One ask on Telegram: the request it answers and the buttons it offered.
#[derive(Clone, Debug, PartialEq)]
pub struct Ask {
    pub chat_id: String,
    pub request_id: String,
    pub method: InteractionMethod,
    pub plan: bool,
    pub options: Vec<String>,
}

/// A request waiting in a chat, as the watcher found it.
#[derive(Clone, Debug)]
pub struct Waiting {
    pub chat_id: String,
    pub chat_title: String,
    pub interaction: Interaction,
}

/// What one look decided: asks to send (`id`, text, options) and asks to take down (`id`, note).
#[derive(Debug, Default, PartialEq)]
pub struct Step {
    pub ask: Vec<(String, String, Vec<String>)>,
    pub done: Vec<(String, String)>,
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

fn cut(s: &str, max: usize) -> String {
    let mut it = s.chars();
    let out: String = it.by_ref().take(max).collect();
    if it.next().is_some() {
        format!("{out}…")
    } else {
        out
    }
}

/// The ask's text: which chat, what it wants, and a short look at the details.
pub fn ask_text(chat_title: &str, i: &Interaction) -> String {
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
            s.push('\n');
            s.push_str(&cut(p, 600));
        }
    }
    for c in i.context.iter().flatten().take(3) {
        let t = c.text.trim();
        if !t.is_empty() {
            s.push_str(&format!("\n{}: {}", c.label.trim(), cut(t, 400)));
        }
    }
    cut(&s, 3000)
}

/// What the tapped option sends, as the card in Helm would: a confirmation answers yes for the
/// first button; a plan's option also confirms when it is "Approve plan".
pub fn answer_args(ask: &Ask, index: usize) -> Value {
    let label = &ask.options[index];
    let mut a = json!({ "chatId": ask.chat_id, "requestId": ask.request_id });
    if ask.method == InteractionMethod::Confirm {
        a["confirmed"] = json!(index == 0);
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
        let gone: Vec<String> = m
            .iter()
            .filter(|(_, a)| !live.contains(&(a.chat_id.as_str(), a.request_id.as_str())))
            .map(|(id, _)| id.clone())
            .collect();
        for id in gone {
            m.remove(&id);
            step.done.push((
                id,
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
            let ask = Ask {
                chat_id: w.chat_id.clone(),
                request_id: w.interaction.id.clone(),
                method: w.interaction.method,
                plan: w.interaction.attention == Some(InteractionAttention::Plan),
                options: options_of(&w.interaction),
            };
            let id = new_id();
            step.ask.push((
                id.clone(),
                ask_text(&w.chat_title, &w.interaction),
                ask.options.clone(),
            ));
            m.insert(id, ask);
        }
        step
    }

    /// An ask the hub could not send: forget it, so the next look asks again.
    pub fn unsent(&self, id: &str) {
        self.map().remove(id);
    }

    /// Take every open ask down (approvals were turned off, or the line went away).
    pub fn clear(&self) -> Vec<String> {
        self.map().drain().map(|(id, _)| id).collect()
    }

    /// The user's tap. Single use: the ask is gone afterwards, whatever the outcome.
    pub fn take(&self, id: &str, index: usize) -> Option<Ask> {
        let ask = self.map().remove(id)?;
        (index < ask.options.len()).then_some(ask)
    }
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
            "Docs needs your approval:\nAllow Bash?\nCommand: cargo test"
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
        assert_eq!(s.ask[1].2, ["Approve plan", "Keep planning"]);
        assert!(asks.step(&both, &mut id).ask.is_empty(), "asked once");
        // The plan was answered in Helm: its buttons come down.
        let s = asks.step(&[waiting(approval.clone())], &mut id);
        assert_eq!(s.done.len(), 1);
        assert_eq!(s.done[0].0, "ask2");
        // A tap: an index past the buttons answers nothing; a good one answers once.
        assert!(asks.take("nope", 0).is_none());
        let ask = asks.take("ask1", 0).unwrap();
        assert_eq!(
            answer_args(&ask, 0),
            json!({"chatId": "c1", "requestId": "r1", "value": "Allow once"})
        );
        assert!(asks.take("ask1", 0).is_none(), "single use");
        // Confirmations and plans answer as the card does.
        let confirm = Ask {
            method: Confirm,
            plan: true,
            options: vec!["Approve plan".into(), "Keep planning".into()],
            ..ask.clone()
        };
        assert_eq!(answer_args(&confirm, 1)["confirmed"], false);
        assert_eq!(answer_args(&confirm, 0)["confirmed"], true);
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
    }
}
