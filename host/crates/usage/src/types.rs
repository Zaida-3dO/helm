//! The shapes the meter shares with the UI. Generated TypeScript lives in
//! `ui/src/api/generated/usage/` (see [`crate::bindings`]).

use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// Where a number came from. Both are the claude binary's: the `get_usage` control request, or a
/// `rate_limit_event` line in a live chat.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum UsageSource {
    Control,
    RateLimitEvent,
}

/// One plan window as last read.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct UsageWindow {
    /// Percent used, 0 to 100; `None` when the binary gave a reset time but no number.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional, type = "number")]
    pub pct: Option<f64>,
    /// Epoch milliseconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional, type = "number")]
    pub resets_at: Option<i64>,
    /// When this window was read (epoch milliseconds).
    #[ts(type = "number")]
    pub at: i64,
    pub source: UsageSource,
}

/// A per-model weekly window (for example the plan's Opus or Sonnet allowance).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct ModelUsage {
    pub model: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional, type = "number")]
    pub pct: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional, type = "number")]
    pub resets_at: Option<i64>,
    #[ts(type = "number")]
    pub at: i64,
    pub source: UsageSource,
}

/// What the host holds: the newest data per window (newer wins, per window).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StoredUsage {
    #[serde(default)]
    pub plan: Option<String>,
    #[serde(default)]
    pub five_hour: Option<UsageWindow>,
    #[serde(default)]
    pub weekly: Option<UsageWindow>,
    #[serde(default)]
    pub per_model: Vec<ModelUsage>,
}

/// The reading the meter and Settings → Usage show.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct PlanUsageReading {
    /// The plan name the binary reported (for example "max"), when it did.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub plan: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub five_hour: Option<UsageWindow>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub weekly: Option<UsageWindow>,
    pub per_model: Vec<ModelUsage>,
    /// The source of the newest window.
    pub source: UsageSource,
    /// The newest window's read time (epoch milliseconds).
    #[ts(type = "number")]
    pub fetched_at: i64,
    /// Older than 15 minutes: shown greyed with "as of".
    pub stale: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum ReadinessState {
    /// A reading is available (possibly stale).
    Ready,
    /// There is no reading, and `reason` says why and where to act.
    Inactive,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct UsageReadiness {
    pub state: ReadinessState,
    /// Plain words, always present.
    pub reason: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub action_label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub action_target: Option<String>,
    /// The Claude Code version the last read saw, when it told us.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub binary_version: Option<String>,
}

/// The answer of `usage_get` and `usage_refresh`, and the payload of the `plan_usage` event.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct UsageSnapshot {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub reading: Option<PlanUsageReading>,
    pub readiness: UsageReadiness,
    /// A read is running now.
    pub refreshing: bool,
    /// The earliest time another read may start (epoch milliseconds), when one ran lately.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional, type = "number")]
    pub next_refresh_at: Option<i64>,
}

/// Token counts and the cost the binary reported, summed over some messages.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct CostTotals {
    #[ts(type = "number")]
    pub input_tokens: i64,
    #[ts(type = "number")]
    pub output_tokens: i64,
    #[ts(type = "number")]
    pub cache_read_tokens: i64,
    #[ts(type = "number")]
    pub cache_write_tokens: i64,
    /// The four counts added up.
    #[ts(type = "number")]
    pub total_tokens: i64,
    /// Claude Code's API-equivalent estimate in US dollars, not what a plan bills.
    pub cost_usd: f64,
}

/// One chat's line in a cost report.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct ChatCost {
    pub chat_id: String,
    pub title: String,
    pub archived: bool,
    pub totals: CostTotals,
    /// This chat's cost since the current weekly window began.
    pub week_cost_usd: f64,
    /// The estimated share of the weekly plan limit this chat used, in percent of the limit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional, type = "number")]
    pub weekly_pct: Option<f64>,
}

/// How a report's weekly share was worked out.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct WeeklyShare {
    /// The plan's weekly window, percent used, as the meter last read it.
    pub week_pct: f64,
    /// When the current weekly window began and when it resets (epoch milliseconds).
    #[ts(type = "number")]
    pub window_start: i64,
    #[ts(type = "number")]
    pub resets_at: i64,
    /// What every Helm chat cost in the window, and what this report's chats cost in it.
    pub helm_cost_usd: f64,
    pub cost_usd: f64,
    /// The estimated share of the weekly limit this report's chats used, in percent of the limit.
    pub share_pct: f64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum CostScope {
    Chat,
    Project,
}

/// The answer of `usage_cost`: what one chat, or every chat in a project and its sub-projects, used.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct CostReport {
    pub scope: CostScope,
    /// The chat's title or the project's name.
    pub name: String,
    pub totals: CostTotals,
    /// `None` when the meter has no current weekly reading.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub weekly: Option<WeeklyShare>,
    /// Every chat that used anything, the costliest first (one line for a chat report).
    pub chats: Vec<ChatCost>,
    /// How many of those chats are archived (they are counted).
    pub archived_chats: u32,
}
