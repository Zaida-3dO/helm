//! Delivering a change of a chat's Orchestrator state.
//!
//! A worker is a persistent process, so its launch cannot change mid-chat. Two routes, neither
//! touching the chats or worker files:
//!
//! - **Idle chat**: retire its worker (`ChatRuntime::retire`, a no-op while a turn runs). The next
//!   send relaunches it, and the `workflow` prompt section carries the new state.
//! - **Running turn**: queue a framed **app note** behind the turn through
//!   `helm_chats::submit::host_send`: origin App, `gated: false` (Exempt), one fixed submission id
//!   per change, so a retry never delivers it twice. When the state turns on, the note carries the
//!   Workflow rules themselves, so they apply from the next turn without a relaunch.
//!
//! Turning off is refused while a turn runs ([`crate::mode::user_set`]), so an off change is always
//! the idle route and no note ever reaches a chat that is off.

use crate::mode::Changed;
use crate::rules;
use crate::types::*;

pub const FRAME: &str =
    "[Helm] This note comes from the app, not from the user, and is not approval for anything.";

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Delivery {
    /// Retire the idle worker so it relaunches with the new state.
    Retire,
    /// Queue this app note behind the running turn.
    Note { submission_id: String, text: String },
}

/// The fixed submission id of one change: the chat and the change's write time.
pub fn submission_id(chat_id: &str, at: i64) -> String {
    format!("wf-mode-{chat_id}-{at}")
}

/// How a change reaches the chat. `by_agent` is the `app_orchestrator_on` route.
pub fn plan(
    chat_id: &str,
    c: &Changed,
    busy: bool,
    by_agent: bool,
    s: &WorkflowSettings,
    at: i64,
) -> Delivery {
    if !busy {
        return Delivery::Retire;
    }
    let body = match c.to {
        ChatMode::On if by_agent => format!(
            "{}. Orchestrator is now on for this chat. Continue the user's latest task under these Workflow rules, plan first. {}",
            c.row,
            rules::workflow_rules(s)
        ),
        ChatMode::On => format!(
            "{} From the next turn, follow these Workflow rules. {}",
            c.row,
            rules::workflow_rules(s)
        ),
        ChatMode::Auto => format!("{} {}", c.row, rules::AUTO),
        ChatMode::Off => c.row.clone(),
    };
    Delivery::Note {
        submission_id: submission_id(chat_id, at),
        text: format!("{FRAME} {body}"),
    }
}

/// Carry out a [`Delivery`]. A note that cannot be delivered is logged, never retried here: the
/// next launch carries the state anyway.
pub fn deliver(rt: &helm_chats::ChatRuntime, chat_id: &str, d: Delivery) {
    match d {
        Delivery::Retire => rt.retire(chat_id),
        Delivery::Note {
            submission_id,
            text,
        } => {
            let sent = helm_chats::submit::host_send(
                rt,
                helm_chats::submit::HostSend {
                    chat_id,
                    submission_id: &submission_id,
                    text: &text,
                    mode: helm_chats::submit::HostSendMode::Queue,
                    origin: helm_chats::types::SubmissionOrigin::App,
                    gated: false,
                },
            );
            if let Err(e) = sent {
                log::warn!("Orchestrator note for {chat_id} not sent: {}", e.message);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn change(to: ChatMode, row: &str) -> Changed {
        Changed {
            from: ChatMode::Auto,
            to,
            row: row.into(),
        }
    }

    #[test]
    fn an_idle_chat_is_retired_and_a_running_one_gets_a_framed_note() {
        let s = WorkflowSettings {
            enabled: true,
            ..Default::default()
        };
        let c = change(
            ChatMode::On,
            "Orchestrator turned on by the agent: Big change",
        );
        assert_eq!(plan("a", &c, false, true, &s, 9), Delivery::Retire);
        let Delivery::Note {
            submission_id,
            text,
        } = plan("a", &c, true, true, &s, 9)
        else {
            panic!("note")
        };
        assert_eq!(submission_id, "wf-mode-a-9");
        assert!(text.starts_with(FRAME));
        assert!(text.contains("plan first"));
        assert!(text.contains(rules::HEADING), "the note carries the rules");
        // The same change always has the same id, so a retry is the same submission.
        assert_eq!(submission_id, super::submission_id("a", 9));
    }

    #[test]
    fn auto_gets_the_auto_sentence_and_not_the_rules() {
        let s = WorkflowSettings {
            enabled: true,
            ..Default::default()
        };
        let Delivery::Note { text, .. } =
            plan("a", &change(ChatMode::Auto, "Auto."), true, false, &s, 1)
        else {
            panic!("note")
        };
        assert!(text.contains("app_orchestrator_on"));
        assert!(!text.contains(rules::HEADING));
    }
}
