//! What AFK does with the asks a chat's agent raises while the user is away, and the line every
//! turn carries. The chat's actor asks; this decides and keeps what it decided for the digest.
//!
//! - A question gets the automatic answer (the user's text, or the built-in one, then the safety
//!   clause).
//! - A plan is approved when the saved choice is to approve and it was not already waiting when
//!   AFK started; otherwise it waits for the user.
//! - A permission card (Allow once / Deny) is never answered: it waits for the user and is listed
//!   in the digest. Allow requests filed through the consent tool are not asks at all: only the
//!   user's click decides them, and the digest lists the ones still waiting.

use crate::service::AfkService;
use crate::settings::PlanApproval;
use crate::texts;
use helm_chats::seams::{AfkAction, AfkPolicy};
use helm_chats::types::{Interaction, InteractionAttention};
use serde_json::{json, Map};
use std::sync::Arc;

pub struct ServicePolicy(pub Arc<AfkService>);

/// The first line of the first context row, for a card title (the command or file it is about).
fn detail(i: &Interaction) -> String {
    let first = i
        .context
        .as_ref()
        .and_then(|c| c.first())
        .map(|r| r.text.lines().next().unwrap_or("").trim().to_owned())
        .unwrap_or_default();
    first.chars().take(160).collect()
}

fn title(i: &Interaction) -> String {
    i.title.clone().unwrap_or_else(|| "A request".into())
}

/// The decision for one ask while AFK is `a`. Pure apart from `keep`, which records it.
pub fn decide_with(
    a: &crate::service::Active,
    i: &Interaction,
    at_flip: bool,
    keep: &dyn Fn(&str, &str),
) -> AfkAction {
    let left = |kind: &str, what: String| {
        keep(kind, &what);
        AfkAction::Defer {
            marker: format!("AFK left this for you: {what}"),
        }
    };
    match i.attention {
        Some(InteractionAttention::Question) if i.secret != Some(true) => {
            let mut f = Map::new();
            f.insert(
                "value".into(),
                json!(texts::answer(a.settings.answer.as_deref())),
            );
            let q = title(i);
            keep("question", &q);
            AfkAction::Answer {
                fields: f,
                marker: format!("AFK answered a question for you: {q}"),
            }
        }
        Some(InteractionAttention::Plan) => {
            let offered = i
                .options
                .as_ref()
                .is_some_and(|o| o.iter().any(|x| x == "Approve plan"));
            if a.settings.plan_approval == PlanApproval::Approve && offered && !at_flip {
                let mut f = Map::new();
                f.insert("confirmed".into(), json!(true));
                f.insert("value".into(), json!("Approve plan"));
                keep("plan_approved", "");
                AfkAction::Answer {
                    fields: f,
                    marker: "AFK approved the plan for you.".into(),
                }
            } else {
                left("plan_deferred", "approval of the plan".into())
            }
        }
        Some(InteractionAttention::Approval) | Some(InteractionAttention::Trust) => {
            let d = detail(i);
            let what = if d.is_empty() {
                title(i)
            } else {
                format!("{} {d}", title(i))
            };
            left("approval_deferred", what)
        }
        // A secret question, or something AFK does not know: the user answers it.
        _ => AfkAction::Leave,
    }
}

impl AfkPolicy for ServicePolicy {
    fn turn_text(&self, _chat_id: &str) -> Option<String> {
        let a = self.0.active()?;
        Some(texts::turn_text(
            a.started_at,
            a.settings.plan_approval == PlanApproval::Approve,
        ))
    }

    fn decide(&self, chat_id: &str, i: &Interaction, at_flip: bool) -> AfkAction {
        let Some(a) = self.0.active() else {
            return AfkAction::Leave;
        };
        let now = helm_core::now_ms();
        decide_with(&a, i, at_flip, &|kind, what| {
            self.0.record(&a.span_id, chat_id, kind, what, now)
        })
    }
}
