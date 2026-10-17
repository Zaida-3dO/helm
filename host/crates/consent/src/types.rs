//! Wire shapes for the consent ops. Generated into
//! `ui/src/api/generated/consent/` by [`crate::bindings`].

use serde::{Deserialize, Serialize};
use ts_rs::TS;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum RequestState {
    Pending,
    Allowed,
    Denied,
    Expired,
    Used,
}

impl RequestState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Allowed => "allowed",
            Self::Denied => "denied",
            Self::Expired => "expired",
            Self::Used => "used",
        }
    }

    pub fn parse(s: &str) -> Self {
        match s {
            "pending" => Self::Pending,
            "allowed" => Self::Allowed,
            "denied" => Self::Denied,
            "used" => Self::Used,
            _ => Self::Expired,
        }
    }
}

/// One Allow request as a card or a transcript chip shows it. `reason` and `evidence` are the
/// agent's words: the renderer shows them as inert, marked text.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct ConsentRequest {
    pub id: String,
    pub chat_id: String,
    pub kind: String,
    /// The kind's card label.
    pub label: String,
    /// The host's one-line summary of the canonical target.
    pub summary: String,
    pub reason: String,
    pub evidence: String,
    pub state: RequestState,
    /// The grant length the agent asked for (5–120; default 30).
    #[ts(type = "number")]
    pub grant_minutes: i64,
    #[ts(type = "number")]
    pub created_at: i64,
    /// When a pending request expires unanswered.
    #[ts(type = "number")]
    pub expires_at: i64,
    #[ts(type = "number | null")]
    pub decided_at: Option<i64>,
    /// When an Allow grant lapses.
    #[ts(type = "number | null")]
    pub grant_expires_at: Option<i64>,
    /// The user's note, sent with Deny.
    pub note: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct ConsentList {
    /// At most 50, oldest first.
    pub pending: Vec<ConsentRequest>,
    /// At most 20, newest first.
    pub decided: Vec<ConsentRequest>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ConsentListArgs {
    pub chat_id: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum Decision {
    Allow,
    Deny,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ConsentDecideArgs {
    pub request_id: String,
    pub decision: Decision,
    /// Allow only: 5–240 minutes; default the agent's ask.
    #[serde(default)]
    #[ts(type = "number | null")]
    pub grant_minutes: Option<i64>,
    /// ≤ 500 characters, sent to the agent with the decision.
    #[serde(default)]
    pub note: Option<String>,
}
