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
    /// Move the base branch to the PR's head through the API, refused unless the base is an
    /// ancestor of the head. GitHub then marks the PR merged and writes no commit of its own.
    /// Only a repository rule may pick it, never `landing.method`.
    FastForward,
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
    /// The dispatch gate on agent spawns during a task: user-only, default on.
    pub dispatch_gate: bool,
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
    /// Stage records for the task board that Helm has not been able to send yet (it retries every
    /// minute). Only in the UI's read; the orchestrator's `status` leaves it 0.
    #[serde(default)]
    pub board_queued: u32,
}

/// One landing-guard override or refusal, for the panel's Needs you.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct GuardNoteView {
    /// Let through by an override (else refused).
    pub overridden: bool,
    /// `bash` or `mcp`, or `pr` for a Merge anyway / Mark ready anyway from the PR view.
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

/// What the PR view's merge box needs from the host for one pull request (`pr_land_info`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct PrLandInfo {
    /// `github.com/owner/name`, lowercased.
    pub repo: String,
    /// Settings → Orchestrator's master switch. While it is off no task holds a PR back.
    pub workflow_on: bool,
    pub rule: PrRule,
    /// The task that tracks the PR, if any.
    pub tracked: Option<PrTracked>,
}

/// How a merge from the PR view lands this repository.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct PrRule {
    /// `draft` only when the repository has its own draft rule: then Helm never merges it and
    /// the merge box says it is merged by hand.
    pub mode: LandMode,
    /// The method a merge uses: the repository rule's, else `landing.method`.
    pub method: MergeMethod,
    /// The repository has a rule of its own. Without one the page confirms before merging.
    pub explicit: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct PrTracked {
    pub run_id: String,
    pub chat_id: String,
    /// The task's title (control characters stripped, at most 80 characters).
    pub title: String,
    /// The task's status (`active`, `parked`, `handed_off`, ...).
    pub status: String,
    pub repo: String,
    pub branch: String,
    /// The change's state, as in [`RunChangeView::state`].
    pub state: String,
    pub approved_tip: Option<String>,
    /// The change holds the PR back: Merge and Ready either go through the task or need
    /// "anyway".
    pub blocking: bool,
    /// The land step is working on it right now: no button applies until it finishes.
    pub busy: bool,
    /// `merge` when Merge goes through the task (it awaits the user's Merge), `ready` when Ready
    /// for review does (it was handed off as a draft).
    pub routes: Option<String>,
}

/// What `pr_merge`, `pr_ready` and `pr_draft` did.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub enum PrOutcome {
    /// Handed to the task: its land step merges it, or it marked the PR ready.
    Routed,
    /// GitHub reads the PR back as merged.
    Merged,
    /// GitHub took the merge but does not show it merged yet (a merge queue, or lag).
    Accepted,
    Ready,
    AlreadyReady,
    Converted,
    AlreadyDraft,
    /// Converted to draft while a task tracks it; its land step marks it ready again if it lands it.
    Tracked,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct PrWriteResult {
    pub outcome: PrOutcome,
    /// The method a merge used.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub method: Option<MergeMethod>,
    /// The commit the base now points at, when GitHub reports it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub merged_sha: Option<String>,
    /// The task's change, for `routed` and for an override.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub run_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub branch: Option<String>,
    /// The change's state afterwards.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub state: Option<String>,
    /// The write went past a task that held the PR ("anyway") and is recorded on it.
    #[serde(default)]
    pub overridden: bool,
    /// GitHub wrote, but recording it on the task failed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub record_error: Option<String>,
}
