//! The shapes the Orchestrator settings page and the composer switch share with the host.
//! Generated TypeScript lives in `ui/src/api/generated/workflow/` (see [`crate::bindings`]).
//!
//! Field meanings are documented on the settings below.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// `required` | `auto` | `off` for plan, planReview, cleanup and smoketest. Review takes only
/// `required` or `off` (validated in [`crate::settings`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum StageUse {
    Required,
    Auto,
    Off,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct PlanStage {
    #[serde(rename = "use")]
    pub use_: StageUse,
    pub agent: String,
}

/// A verdict-bearing stage: plan review, review, smoketest.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct RoundStage {
    #[serde(rename = "use")]
    pub use_: StageUse,
    pub agent: String,
    /// 1–20.
    pub max_rounds: u32,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct BuildStage {
    pub agent: String,
    /// Agents whose dispatch outside a task starts one (≤ 32).
    pub start_agents: Vec<String>,
    /// Agents whose dispatch during a task is not a build (≤ 32).
    pub build_exempt: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct Stages {
    pub plan: PlanStage,
    pub plan_review: RoundStage,
    pub build: BuildStage,
    pub review: RoundStage,
    pub cleanup: PlanStage,
    pub smoketest: RoundStage,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum EditsRule {
    Deny,
    Allow,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum SignOff {
    /// An approving plan review is enough.
    Review,
    /// The user also signs off every plan before any build.
    Me,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum LandMode {
    Draft,
    Merge,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum MergeMethod {
    Merge,
    Squash,
    Rebase,
}

/// One per-repository land rule. `method` only on a merge rule.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct RepoRule {
    /// `host/owner/name`, lowercased.
    pub repo: String,
    pub mode: LandMode,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub method: Option<MergeMethod>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct Landing {
    pub default: LandMode,
    pub method: MergeMethod,
    pub repos: Vec<RepoRule>,
    /// ≤ 8 labels of ≤ 50 characters, added at draft hand-off.
    pub draft_labels: Vec<String>,
    /// Removed when the user presses Ready for review.
    pub flip_label: Option<String>,
    /// The only branches the land step merges into, plus (for the guard) each repo's default
    /// branch. Empty means the land step merges nothing.
    pub protected_branches: Vec<String>,
}

/// "Orchestrator for new chats": the state of every chat with no state of its own.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum NewChats {
    Always,
    Never,
    Auto,
}

/// Settings → Orchestrator. A chat reads all of it through the settings tools and may change only
/// `newChats` and each stage's agent; every other field is the user's.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowSettings {
    /// The master switch. Off by default.
    pub enabled: bool,
    pub stages: Stages,
    pub orchestrator_edits: EditsRule,
    pub plan_sign_off: SignOff,
    /// ≤ 50 `host/owner/name` repositories whose being the working folder does not make them part
    /// of every task.
    pub home_repos: Vec<String>,
    pub landing: Landing,
    /// Move fast: user-only, default off.
    pub move_fast: bool,
    /// The landing guard on Bash and MCP calls in chats with a task: user-only, default on.
    pub landing_guard: bool,
    /// Defaults to `auto`.
    pub new_chats: NewChats,
}

/// A chat's own Orchestrator state.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum ChatMode {
    On,
    Off,
    Auto,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum SetBy {
    User,
    Agent,
    /// No row of its own yet, or pinned from `newChats` at first use.
    Default,
}

/// What the composer switch shows for one chat (`workflow_chat_mode_get` / `_set`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct ChatModeView {
    pub chat_id: String,
    pub mode: ChatMode,
    pub set_by: SetBy,
    /// The agent's reason when it turned the chat on (control and bidi characters stripped,
    /// ≤ 300 characters). Shown only in the tooltip.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub reason: Option<String>,
    /// The workflow runs for this chat now: master on, a Claude Code chat, not archived, mode on.
    pub in_effect: bool,
    /// Why the switch is unavailable (master off, not a Claude Code chat, archived); `None` when
    /// it can be clicked.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub unavailable: Option<String>,
    /// The chat's turn is running (turning off is refused until it ends).
    pub running: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ChatModeArgs {
    pub chat_id: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ChatModeSetArgs {
    pub chat_id: String,
    pub mode: ChatMode,
}

/// The answer of `workflow_chat_mode_set`: the new view, plus the transcript line the change wrote.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct ChatModeSetResult {
    pub view: ChatModeView,
    /// `None` when the request matched the current state and nothing was written.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub row: Option<String>,
}

/// One chat's workflow as the panel and `app_workflow status` read it: the current task with its
/// plan and changes, plus the chat's other open tasks (at most 8). Served by `workflow_run_get`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct RunView {
    /// `None` when the chat has no open task.
    pub task: Option<RunTaskView>,
    /// The current task's plan; `None` without a task.
    pub plan: Option<RunPlanView>,
    /// The current task's changes.
    pub changes: Vec<RunChangeView>,
    /// The chat's other open tasks, each with the changes that wait on the user.
    pub other_tasks: Vec<OtherTaskView>,
    /// The landing guard's overrides and refusals since the user last typed in the chat (only in
    /// the UI's read; the orchestrator's `status` leaves it empty).
    #[serde(default)]
    pub guard: Vec<GuardNoteView>,
}

/// One landing-guard override or refusal, for the panel's Needs you.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct GuardNoteView {
    /// Let through by an override (else refused).
    pub overridden: bool,
    /// `bash` or `mcp`.
    pub gate: String,
    /// The guard's rule: `task_repo`, `containment`, `untracked`, `draft_rule`, `fail_closed`.
    pub rule: String,
    /// The command or call, cut to 120 characters.
    pub command: String,
    /// For an override: the user's quoted words were found.
    pub verified: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct RunTaskView {
    pub run_id: String,
    pub title: String,
    /// `active`, `parked`, `handed_off` or another task status.
    pub status: String,
    #[ts(type = "number")]
    pub attempt: i64,
    pub visual: bool,
    pub item_id: Option<String>,
    /// Stages the user skipped (`plan`, `planReview`, `review`, `cleanup`, `smoketest`).
    pub skipped: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct RunPlanView {
    pub id: Option<String>,
    pub approved: bool,
    pub signed_off: bool,
    #[ts(type = "number")]
    pub rounds: i64,
    #[ts(type = "number")]
    pub cap: i64,
    /// Why a build would not count yet, in the host's words; `None` when nothing is missing.
    pub missing: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct RunChangeView {
    pub repo: String,
    pub branch: String,
    pub pr: Option<String>,
    /// `open`, `landing_draft`, `pending_land`, `landing`, `awaiting_merge`, `land_failed`,
    /// `handed_off`, `landed`, `flipped`, `dropped` or `declined`.
    pub state: String,
    pub failed_mode: Option<String>,
    pub tip: Option<String>,
    /// The review's approval holds at the change's latest commit.
    pub approved: bool,
    #[ts(type = "number")]
    pub review_rounds: i64,
    #[ts(type = "number")]
    pub review_cap: i64,
    #[ts(type = "number")]
    pub smoketest_rounds: i64,
    #[ts(type = "number")]
    pub smoketest_cap: i64,
    pub error: Option<String>,
    /// What the change still needs before it can be handed off, in the host's words.
    pub missing: Vec<String>,
}

/// A task that is not the chat's current one.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct OtherTaskView {
    pub run_id: String,
    pub title: String,
    pub status: String,
    pub park_reason: Option<String>,
    /// Its changes that wait on the user (awaiting a Merge, land failed, or a draft for review),
    /// so the panel can offer the same buttons as for the current task.
    pub waiting: Vec<RunChangeView>,
}
