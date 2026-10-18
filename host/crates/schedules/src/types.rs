//! The shapes shared with the UI (generated into `ui/src/api/generated/schedules/`).

use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// When a schedule fires. Times are Unix milliseconds; weekdays count from Monday = 0.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum Timing {
    /// Once, at an instant.
    Once {
        #[ts(type = "number")]
        at: i64,
    },
    /// Every `seconds` (60 to a year), starting at `startAt` or one period from creation.
    #[serde(rename_all = "camelCase")]
    Interval {
        #[ts(type = "number")]
        seconds: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(type = "number | null", optional)]
        start_at: Option<i64>,
    },
    /// At a local time of day on chosen weekdays.
    Weekly { hour: u8, minute: u8, days: Vec<u8> },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub enum ScheduleState {
    Active,
    Paused,
    Completed,
    Cancelled,
}

impl ScheduleState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Paused => "paused",
            Self::Completed => "completed",
            Self::Cancelled => "cancelled",
        }
    }
    pub fn parse(s: &str) -> Self {
        match s {
            "paused" => Self::Paused,
            "completed" => Self::Completed,
            "cancelled" => Self::Cancelled,
            _ => Self::Active,
        }
    }
}

/// What a schedule says; stored as one JSON document.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct Definition {
    pub title: String,
    pub message: String,
    pub timing: Timing,
    /// The existing chat the prompt is sent into.
    pub chat_id: String,
    /// The ready-made template this was enabled from, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub template: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct Schedule {
    pub id: String,
    pub definition: Definition,
    pub state: ScheduleState,
    /// The next run (Unix ms); null when paused with nothing pending, completed or cancelled.
    #[ts(type = "number | null")]
    pub next_run_at: Option<i64>,
    /// Bumps on every write; an update must quote the one it read.
    #[ts(type = "number")]
    pub revision: i64,
    #[ts(type = "number")]
    pub created_at: i64,
    /// The chat's title, for the list (empty when the chat is gone).
    pub chat_title: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub enum RunStatus {
    /// Claimed; the send is being made.
    Dispatching,
    /// Handed to the chat.
    Started,
    /// The send was refused or could not be made. Never retried.
    Failed,
    /// The chat was gone or archived; the schedule paused itself.
    Blocked,
    /// Helm closed during the send and cannot tell whether it landed. Never replayed.
    Uncertain,
}

impl RunStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Dispatching => "dispatching",
            Self::Started => "started",
            Self::Failed => "failed",
            Self::Blocked => "blocked",
            Self::Uncertain => "uncertain",
        }
    }
    pub fn parse(s: &str) -> Self {
        match s {
            "started" => Self::Started,
            "failed" => Self::Failed,
            "blocked" => Self::Blocked,
            "uncertain" => Self::Uncertain,
            _ => Self::Dispatching,
        }
    }
}

/// One claimed occurrence.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct Run {
    pub schedule_id: String,
    /// The slot that was due (Unix ms); unique per schedule.
    #[ts(type = "number")]
    pub scheduled_at: i64,
    #[ts(type = "number")]
    pub started_at: i64,
    pub status: RunStatus,
    pub chat_id: String,
    pub error: Option<String>,
}

/// A ready-made schedule, off until the user enables it for a chat.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct Template {
    pub id: String,
    pub title: String,
    pub summary: String,
    pub message: String,
    pub timing: Timing,
}

/// `schedule_list` answer.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct ScheduleList {
    pub schedules: Vec<Schedule>,
    pub templates: Vec<Template>,
    /// The host's clock, so the pane counts down against the same time the scheduler uses.
    #[ts(type = "number")]
    pub now: i64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct RunList {
    pub runs: Vec<Run>,
}
