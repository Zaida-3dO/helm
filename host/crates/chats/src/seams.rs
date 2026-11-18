//! Seams other crates implement and register as services. The chats crate
//! defines the traits and calls them; it never depends on the crates that implement them.
//!
//! | Seam | Implemented by | Called from |
//! |---|---|---|
//! | [`SendContextBuilder`] (service [`SendContext`]) | `helm_panel` (panel context) | `chat_send`, for a composer send that carries `context` |
//! | [`ProjectGate`] (service [`ProjectGates`]) | `helm_projects` | `chat_restore` and `chat_create`: an archived project refuses both. It also answers which board a chat is on and puts new chats on one ([`crate::store::Boards`]) |
//! | [`StartupSeam`] (installed with [`crate::ChatRuntime::set_startup_seam`]) | `helm_projects` | a chat's first message: the project's startup instructions ride it as hidden context |
//! | [`AfkPolicy`] (installed with [`crate::ChatRuntime::set_afk_policy`]) | `helm_afk` | each new interaction a worker raises, and each turn's context block |
//! | [`QuestionGate`] (installed with [`crate::ChatRuntime::set_question_gate`]) | `helm_gatekeeper` | each new interaction, before AFK; its verdict arrives through [`crate::ChatRuntime::gate_verdict`] |
//! | [`StopJudge`] (installed with [`crate::ChatRuntime::set_stop_judge`]) | `helm_gatekeeper` | each clean turn end, before the finish is announced, and each turn's context block; its verdict arrives through [`crate::ChatRuntime::stop_verdict`] |
//!
//! A seam with nothing registered is skipped: a context payload is dropped (logged once per send)
//! and every project counts as open.

use crate::archive::Refusal;
use rusqlite::Transaction;
use serde_json::Value;
use std::sync::Arc;

/// The per-chat context generation: bumped on every worker launch and compaction, so a builder
/// that dedupes ("unchanged") knows when it must send the full block again.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub struct ContextGen(pub u64);

/// The block attached to the user's message. The worker sends it to Claude Code as a separate
/// text content block after the message text; the transcript row shows only the message.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ContextBlock {
    pub text: String,
}

pub trait SendContextBuilder: Send + Sync {
    /// Build the block for `payload` (the renderer's strictly shaped panel-context value). An
    /// `Err` refuses the whole send, with the draft kept.
    fn build(
        &self,
        tx: &Transaction<'_>,
        chat_id: &str,
        payload: &Value,
        gen: ContextGen,
    ) -> Result<ContextBlock, Refusal>;
}

/// The service wrapper: `services.insert(Arc::new(SendContext(Arc::new(MyBuilder))))`.
#[derive(Clone)]
pub struct SendContext(pub Arc<dyn SendContextBuilder>);

/// What a refused write into a closed project says.
pub const CLOSED_PROJECT: &str = "This project is closed. Reopen it first.";

/// A board project as the chats crate sees it: where its new chats start and what to call it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BoardSummary {
    pub id: String,
    /// The name the sidebar shows (a local rename, else the board's own title).
    pub name: String,
    /// The repository the board names, if any.
    pub repo: Option<String>,
    /// The folder project the board's new chats start in, and its folder.
    pub folder_project_id: Option<String>,
    pub folder_path: Option<String>,
    pub archived: bool,
    /// Closed by the user: hidden, and no chat is put on it until it is reopened.
    pub closed: bool,
    /// The board no longer lists it: no new chat is put on it.
    pub stale: bool,
}

/// Every board answer defaults to "no boards", so a gate that only knows about archiving (and a
/// test that registers none) keeps chats exactly where they were made.
pub trait ProjectGate: Send + Sync {
    /// True when the project is archived: new chats and single-chat restores are refused.
    fn project_archived(&self, tx: &Transaction<'_>, project_id: &str) -> rusqlite::Result<bool>;

    /// True when the project is closed: nothing new is created in it or moved into it. Chats
    /// already there keep working. Open when the gate does not know.
    fn project_closed(&self, _tx: &Transaction<'_>, _project_id: &str) -> rusqlite::Result<bool> {
        Ok(false)
    }

    /// The board with this project id; `None` when the id is a folder project or unknown.
    fn board(
        &self,
        _tx: &Transaction<'_>,
        _project_id: &str,
    ) -> rusqlite::Result<Option<BoardSummary>> {
        Ok(None)
    }

    /// The board a chat is shown under; `None` when it is not on a board.
    fn chat_board(
        &self,
        _tx: &Transaction<'_>,
        _chat_id: &str,
    ) -> rusqlite::Result<Option<String>> {
        Ok(None)
    }

    /// Show the chat under the board, replacing any group it had. False (and nothing written)
    /// when the id is not an open board or the chat is a side chat.
    fn put_on_board(
        &self,
        _tx: &Transaction<'_>,
        _chat_id: &str,
        _board_id: &str,
    ) -> rusqlite::Result<bool> {
        Ok(false)
    }

    /// Where chats with no other home go (the default-board setting).
    fn default_board(&self, _tx: &Transaction<'_>) -> rusqlite::Result<Option<String>> {
        Ok(None)
    }

    /// The board a new chat goes to when nobody named one: the one open board whose folder is
    /// `folder_project_id`, else the default board.
    fn home_board(
        &self,
        _tx: &Transaction<'_>,
        _folder_project_id: Option<&str>,
    ) -> rusqlite::Result<Option<String>> {
        Ok(None)
    }
}

/// The service wrapper for [`ProjectGate`].
#[derive(Clone)]
pub struct ProjectGates(pub Arc<dyn ProjectGate>);

/// What the project adds to a chat's first message.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StartupInjection {
    /// The hidden context that rides the message, ahead of anything else it carries.
    pub context: String,
    /// The one visible transcript line saying so.
    pub row: String,
}

/// A project's startup instructions, offered once to a new chat's first message.
pub trait StartupSeam: Send + Sync {
    /// Called inside the send's transaction for every message to a chat that has not started
    /// (no run, no fork). `predecessor` is the chat this one continues after a roll over. Answers
    /// the injection the first time only, and again if the message that carried it never went
    /// out; `None` when the chat's project has no startup instructions.
    fn first_send(
        &self,
        tx: &Transaction<'_>,
        chat_id: &str,
        submission_id: &str,
        predecessor: Option<&str>,
    ) -> rusqlite::Result<Option<StartupInjection>>;

    /// The names of the context modes the chat's project starts its chats with, in the order the
    /// project lists them (the chat's board when it names some, else its folder project). Names
    /// that match no mode file are the caller's to skip.
    fn default_modes(
        &self,
        _tx: &Transaction<'_>,
        _chat_id: &str,
    ) -> rusqlite::Result<Vec<String>> {
        Ok(Vec::new())
    }
}

/// What AFK does with one pending interaction.
#[derive(Clone, Debug, PartialEq)]
pub enum AfkAction {
    /// Not AFK's to handle: the card waits for the user as usual.
    Leave,
    /// Answer it now. `fields` are the answer's fields (`value`, `confirmed`); `marker` is the
    /// transcript line recording what AFK did.
    Answer {
        fields: serde_json::Map<String, Value>,
        marker: String,
    },
    /// Leave it waiting for the user, and say so in the transcript.
    Defer { marker: String },
}

/// What the question gate makes of one new card.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GateOffer {
    /// Not the gate's business (the gate is off, this is not a question it may judge, or it is
    /// over its limits): the card follows the normal path — AFK, then the user.
    Pass,
    /// The gate takes it: the card waits, marked as being checked, until the gate answers with
    /// [`crate::ChatRuntime::gate_verdict`].
    Hold,
}

/// A question arriving through a tool channel rather than as a card: what the gate judges.
#[derive(Clone, Debug, PartialEq)]
pub struct AskSpec {
    /// Which channel raised it (`flag`, `telegram_ask`, `board_pick`, …).
    pub channel: String,
    pub question: String,
    pub options: Vec<String>,
    pub recommended: Option<String>,
}

/// The gate's say over a card, answered later through [`crate::ChatRuntime::gate_verdict`].
#[derive(Clone, Debug, PartialEq)]
pub enum GateOutcome {
    /// The gate answered for the user. `fields` are the answer's fields (`value`, or
    /// `confirmed` and `value` for a plan); `marker` is the transcript line recording it.
    Decide {
        fields: serde_json::Map<String, Value>,
        marker: String,
    },
    /// This one needs the user: the card stays, shown with the gate's brief on top.
    Escalate { brief: crate::types::GateBrief },
    /// The gate could not judge this card: it becomes a normal card with a quiet note.
    Failed { note: String },
}

/// The question gate: it decides whether an ask really needs the user, or whether a reasonable
/// default answers it. Implemented by the gatekeeper crate; with nothing installed every card
/// follows the normal path. The actor never waits on it: after a [`GateOffer::Hold`] the gate
/// calls `ChatRuntime::gate_verdict` for that card, and a deadline in the actor ends the hold
/// whatever the gate is doing.
pub trait QuestionGate: Send + Sync {
    /// Called on the actor for each new question or plan card. Returns at once; after a Hold the
    /// gate later calls [`crate::ChatRuntime::gate_verdict`] for this card.
    fn offer(&self, chat_id: &str, i: &crate::types::Interaction) -> GateOffer;

    /// A question raised through a tool channel. The default passes, so a build that has not
    /// wired the channels keeps their old behaviour.
    fn offer_ask(&self, _chat_id: &str, _ask: &AskSpec) -> GateOffer {
        GateOffer::Pass
    }
}

/// AFK's say over a chat, asked by the chat's actor. Implemented by the AFK crate; with nothing
/// installed every chat behaves as if AFK were off.
pub trait AfkPolicy: Send + Sync {
    /// Text every new turn of this chat carries while AFK is on for it, `None` otherwise.
    fn turn_text(&self, chat_id: &str) -> Option<String>;
    /// What to do with an interaction the worker raised (or, with `at_flip`, one already waiting
    /// when AFK turned on). The implementation records what it decides.
    fn decide(
        &self,
        chat_id: &str,
        interaction: &crate::types::Interaction,
        at_flip: bool,
    ) -> AfkAction;
}

/// A turn that just ended cleanly, as the stop judge sees it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TurnEnd {
    /// The run that ended. A verdict names it, so one for an earlier turn never lands on a later one.
    pub run_id: String,
    pub ended_at: i64,
}

/// What the stop judge makes of a turn end.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StopOffer {
    /// Not the judge's business: the turn finishes now, as it always did.
    Pass,
    /// The judge takes it: the finish is held back, and the chat shows that it is being checked,
    /// until the judge answers with [`crate::ChatRuntime::stop_verdict`] or the hold runs out.
    Hold,
}

/// Why a stop is one the user should see.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StopKind {
    /// Something only the user can clear.
    Blocker,
    /// The plan's usage limit, with nothing left that can run elsewhere.
    Limit,
    /// A failure that persists after retrying.
    Failure,
    /// The chat stopped to ask, and the judge would not answer for the user.
    Asked,
}

/// The judge's say over a held turn end.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StopOutcome {
    /// The chat is carrying on (the judge sends the nudge itself, after this is applied): the
    /// held finish is dropped, so nothing says the chat finished.
    Continue,
    /// A stop worth the user's eyes: `line` is written to the transcript as a row of the kind's
    /// own sort, and the finish is announced.
    Stop { kind: StopKind, line: String },
    /// The chat finished. `line` is an optional transcript note (a gatekeeper row); the finish is
    /// announced.
    Done { line: Option<String> },
    /// The judge could not tell: the finish is announced, as with no judge.
    Failed,
}

/// The stop judge: it looks at each clean end of a turn, and decides whether the chat should
/// carry on by itself. Implemented by the gatekeeper crate; with nothing installed every turn
/// end finishes as it always did. The actor never waits on it: after a [`StopOffer::Hold`] the
/// judge calls `ChatRuntime::stop_verdict` for that turn, and a deadline in the actor ends the
/// hold whatever the judge is doing.
pub trait StopJudge: Send + Sync {
    /// Called on the chat's actor at each clean turn end the actor may hand over (never a stop
    /// the user or the governor asked for, an interrupted turn, an archived chat, one with
    /// messages waiting, or one whose background agents still run). Returns at once; after a
    /// Hold the judge later calls [`crate::ChatRuntime::stop_verdict`] for this run.
    fn offer(&self, chat_id: &str, end: &TurnEnd) -> StopOffer;

    /// One line every new turn of this chat carries while the judge has something to say about
    /// how the chat is meant to work (a changed mode, say); `None` adds nothing.
    fn turn_line(&self, _chat_id: &str) -> Option<String> {
        None
    }

    /// The chat is now shown on board `board_id` because another chat's tool moved it there.
    /// How a chat works is the user's to set, so a move made by a chat does not change how the
    /// moved chat works: the judge ignores that board's own mode for it (the app's mode
    /// stands) until the user moves the chat or it starts again.
    fn agent_board_move(&self, _chat_id: &str, _board_id: &str) {}
}
