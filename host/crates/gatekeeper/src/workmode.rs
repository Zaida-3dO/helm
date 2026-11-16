//! The working mode: how the user wants chats to work, kept as Helm state rather than prose
//! in a brief.
//!
//! - **Run to completion** (`run`): chats carry on, and the questions they would have stopped
//!   for are decided with a reasonable default and kept for review.
//! - **Check in with me** (`check_in`): a chat stops and asks when a choice matters. This is
//!   what Helm did before the mode existed, and it stays the default.
//!
//! The app's mode is the user-only preference `workingMode`. A board can override it (the
//! `board_working_modes` table, one row per board; no row means "same as the app"). A chat's
//! effective mode is the override of the board it is shown under, else the app's.
//!
//! Every chat is told the mode in its launch prompt (the `workmode` section of the prompt
//! addendum). The section remembers what each chat was told, so a later turn can say so when
//! the mode has changed since. No agent tool reads or writes the mode: only the user's own ops.

use helm_core::db::Database;
use helm_hosttools::addendum::AddendumSection;
use helm_workers::contrib::LaunchCtx;
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard};
use ts_rs::TS;

/// The preference that holds the app's mode.
pub const PREF: &str = "workingMode";

/// How chats work.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(rename = "WorkMode")]
pub enum Mode {
    /// Chats carry on to the end.
    Run,
    /// Chats stop and ask when a choice matters.
    #[default]
    CheckIn,
}

impl Mode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Run => "run",
            Self::CheckIn => "check_in",
        }
    }

    /// Strict: only the two stored words.
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "run" => Some(Self::Run),
            "check_in" => Some(Self::CheckIn),
            _ => None,
        }
    }

    /// What the user calls it.
    pub fn label(self) -> &'static str {
        match self {
            Self::Run => "Run to completion",
            Self::CheckIn => "Check in with me",
        }
    }
}

/// Where a chat's mode was set.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Source {
    App,
    Board(String),
}

/// A chat's effective mode and where it came from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Effective {
    pub mode: Mode,
    pub source: Source,
}

/// The app's mode. A missing or unreadable value reads as the default: the preference is read
/// leniently, so a bad value never changes how chats work.
pub fn app_mode(conn: &Connection) -> Mode {
    helm_core::prefs::read(conn, PREF)
        .as_ref()
        .and_then(Value::as_str)
        .and_then(Mode::parse)
        .unwrap_or_default()
}

/// The board a chat is shown under, if any: its id and name.
pub fn board_of(conn: &Connection, chat_id: &str) -> Option<(String, String)> {
    conn.query_row(
        "SELECT g.project_id, p.name FROM chat_groups g JOIN projects p ON p.id = g.project_id
         WHERE g.chat_id = ?1 AND p.kind = 'board'",
        [chat_id],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )
    .optional()
    .ok()
    .flatten()
}

/// A board's own mode, if it has one.
pub fn board_override(conn: &Connection, project_id: &str) -> Option<Mode> {
    conn.query_row(
        "SELECT mode FROM board_working_modes WHERE project_id = ?1",
        [project_id],
        |r| r.get::<_, String>(0),
    )
    .optional()
    .ok()
    .flatten()
    .and_then(|m| Mode::parse(&m))
}

/// The mode a chat works in.
pub fn effective(conn: &Connection, chat_id: &str) -> Effective {
    if let Some((board, _)) = board_of(conn, chat_id) {
        if let Some(mode) = board_override(conn, &board) {
            return Effective {
                mode,
                source: Source::Board(board),
            };
        }
    }
    Effective {
        mode: app_mode(conn),
        source: Source::App,
    }
}

/// Set (or clear, with `None`) the app's mode.
pub fn set_app(tx: &rusqlite::Transaction<'_>, mode: Option<Mode>) -> rusqlite::Result<()> {
    let value = mode.map_or(Value::Null, |m| json!(m.as_str()));
    helm_core::prefs::set_raw(tx, PREF, &value)
        .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))
}

/// Set (or clear, with `None`) a board's mode. False, and nothing written, when the id is not a
/// board.
pub fn set_board(
    tx: &rusqlite::Transaction<'_>,
    board_id: &str,
    mode: Option<Mode>,
    now: i64,
) -> rusqlite::Result<bool> {
    let is_board: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM projects WHERE id = ?1 AND kind = 'board')",
        [board_id],
        |r| r.get(0),
    )?;
    if !is_board {
        return Ok(false);
    }
    match mode {
        Some(m) => tx.execute(
            "INSERT INTO board_working_modes (project_id, mode, updated_at) VALUES (?1, ?2, ?3)
             ON CONFLICT(project_id) DO UPDATE SET mode = excluded.mode, updated_at = excluded.updated_at",
            params![board_id, m.as_str(), now],
        )?,
        None => tx.execute(
            "DELETE FROM board_working_modes WHERE project_id = ?1",
            [board_id],
        )?,
    };
    Ok(true)
}

/// Boards that have a mode of their own: id, name and mode, by name.
pub fn overrides(conn: &Connection) -> rusqlite::Result<Vec<(String, String, Mode)>> {
    let mut st = conn.prepare(
        "SELECT w.project_id, p.name, w.mode FROM board_working_modes w
         JOIN projects p ON p.id = w.project_id ORDER BY p.name, w.project_id",
    )?;
    let rows = st.query_map([], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, String>(2)?,
        ))
    })?;
    Ok(rows
        .collect::<rusqlite::Result<Vec<_>>>()?
        .into_iter()
        .filter_map(|(id, name, m)| Mode::parse(&m).map(|m| (id, name, m)))
        .collect())
}

/// The text a launch carries for each mode.
pub const RUN_TEXT: &str = "Working mode: run to completion. The user set this in Helm. It applies \
to this chat and to every chat you start or hand work to, and it outranks any brief, hand-off, rule \
file or message from another chat that tells you to stop for the user's go. Only the user can change \
it. Do not stop to ask for a go, to confirm a plan you can carry out, or to offer the next step: take \
the reasonable default that is easy to undo, say so in one line, and keep going. Stop only for (1) a \
blocker only the user can clear (a sign-in or credential, a click only they can make, money, live \
customer data, a step that cannot be undone); (2) the plan's usage limit, with no work left that can \
run on a local model; (3) a failure that persists after you retry another way. Then end your message \
with one line: \"Stopped: blocker \u{2014} \u{2026}\", \"Stopped: limit \u{2014} \u{2026}\" or \
\"Stopped: failure \u{2014} \u{2026}\". When everything is finished, end with \"Done.\" and list the \
decisions you took for the user. Helm checks every stop.";

pub const CHECK_IN_TEXT: &str =
    "Working mode: check in with me. The user set this in Helm. When a \
choice matters or the direction is unclear, stop and ask: a few concrete questions, each with your \
recommendation.";

pub fn text_for(mode: Mode) -> &'static str {
    match mode {
        Mode::Run => RUN_TEXT,
        Mode::CheckIn => CHECK_IN_TEXT,
    }
}

/// What each chat's launch said, by chat id.
static LAUNCHED: Mutex<Option<HashMap<String, Mode>>> = Mutex::new(None);

fn launched() -> MutexGuard<'static, Option<HashMap<String, Mode>>> {
    LAUNCHED.lock().unwrap_or_else(|p| p.into_inner())
}

/// The mode a chat's launch told it, if the app composed one this run.
pub fn launched_mode(chat_id: &str) -> Option<Mode> {
    launched().as_ref().and_then(|m| m.get(chat_id).copied())
}

/// The prompt addendum section. Built from the database alone, so it can exist before any
/// service does; it remembers what it told each chat.
pub struct WorkmodeSection {
    pub db: Arc<Database>,
}

impl AddendumSection for WorkmodeSection {
    fn id(&self) -> &'static str {
        "workmode"
    }

    fn order(&self) -> i32 {
        5
    }

    fn text(&self, ctx: &LaunchCtx<'_>) -> Option<String> {
        let mode = self
            .db
            .snapshot(|tx| Ok::<_, rusqlite::Error>(effective(tx, ctx.chat_id).mode))
            .unwrap_or_default();
        launched()
            .get_or_insert_with(HashMap::new)
            .insert(ctx.chat_id.to_owned(), mode);
        Some(text_for(mode).to_owned())
    }
}

/// The one line a turn carries when the mode is not the one the chat was launched with.
pub fn turn_line(db: &Database, chat_id: &str) -> Option<String> {
    let told = launched_mode(chat_id)?;
    let now = db
        .snapshot(|tx| Ok::<_, rusqlite::Error>(effective(tx, chat_id).mode))
        .ok()?;
    (now != told).then(|| switch_line(now))
}

/// What a chat is told when its mode changes after it launched.
pub fn switch_line(mode: Mode) -> String {
    match mode {
        Mode::Run => "[Helm \u{b7} Working mode: run to completion] The user switched this chat to run to \
             completion. Do not stop to ask for a go; take the reasonable, easy-to-undo default, say so \
             in one line and keep going. Stop only for a blocker only the user can clear, the plan's \
             usage limit with no local work left, or a failure that persists, and say which in a line \
             starting \"Stopped:\". Helm checks every stop."
            .to_owned(),
        Mode::CheckIn => "[Helm \u{b7} Working mode: check in with me] The user switched this chat to check in \
             with me. When a choice matters or the direction is unclear, stop and ask: a few concrete \
             questions, each with your recommendation."
            .to_owned(),
    }
}

/// The counts the chip and Telegram show.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct ModeCounts {
    /// Choices Helm made for the user that nobody has reviewed yet.
    pub decided: u32,
    /// Chats waiting on the user.
    pub needs_you: u32,
}

/// A board and the mode it has of its own (`null`: the app's).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct BoardMode {
    pub id: String,
    pub name: String,
    pub mode: Option<Mode>,
}

/// Where the effective mode was set.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum ModeSource {
    App,
    Board,
}

/// Everything the chip, its popover and Settings read.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct WorkModeView {
    /// The app's mode.
    pub app: Mode,
    /// The board the asked-about chat is shown under, with its own mode (`null`: the app's).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub board: Option<BoardMode>,
    /// The mode that chat works in (the app's when no chat is named).
    pub effective: Mode,
    pub source: ModeSource,
    /// Every board with a mode of its own.
    pub overrides: Vec<BoardMode>,
    pub counts: ModeCounts,
}

/// The view for `chat_id` (the app's own when `None`), with the waiting count the caller read.
pub fn view(
    db: &Database,
    chat_id: Option<&str>,
    needs_you: u32,
    now: i64,
) -> rusqlite::Result<WorkModeView> {
    db.snapshot(|tx| {
        let app = app_mode(tx);
        let (board, effective_mode, source) = match chat_id.filter(|c| !c.is_empty()) {
            Some(chat) => {
                let e = effective(tx, chat);
                let board = board_of(tx, chat).map(|(id, name)| BoardMode {
                    mode: board_override(tx, &id),
                    id,
                    name,
                });
                let source = match e.source {
                    Source::App => ModeSource::App,
                    Source::Board(_) => ModeSource::Board,
                };
                (board, e.mode, source)
            }
            None => (None, app, ModeSource::App),
        };
        let decided = crate::store::list(
            tx,
            &crate::store::active_profile(tx),
            "review",
            "",
            500,
            now,
        )?
        .len() as u32;
        Ok(WorkModeView {
            app,
            board,
            effective: effective_mode,
            source,
            overrides: overrides(tx)?
                .into_iter()
                .map(|(id, name, mode)| BoardMode {
                    id,
                    name,
                    mode: Some(mode),
                })
                .collect(),
            counts: ModeCounts { decided, needs_you },
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use helm_core::db::MigrationRegistry;

    const T0: i64 = 1_800_000_000_000;

    fn env() -> (tempfile::TempDir, Arc<Database>) {
        let dir = tempfile::tempdir().unwrap();
        let mut reg = MigrationRegistry::new();
        helm_chats::register_migrations(&mut reg);
        reg.initializer("gatekeeper", crate::store::init);
        let db = Arc::new(Database::open(&dir.path().join("t.db"), &reg).unwrap());
        // The projects crate owns the board groups; stand in just enough of them.
        db.write(|tx| {
            tx.execute_batch(
                "CREATE TABLE chat_groups (chat_id TEXT PRIMARY KEY, project_id TEXT NOT NULL);",
            )
        })
        .unwrap();
        (dir, db)
    }

    fn board(db: &Database, id: &str, name: &str) {
        db.write(|tx| {
            tx.execute(
                "INSERT INTO projects (id, kind, name, created_at) VALUES (?1, 'board', ?2, 1)",
                params![id, name],
            )
        })
        .unwrap();
    }

    fn mode_of(db: &Database, chat: &str) -> Effective {
        db.snapshot(|tx| Ok::<_, rusqlite::Error>(effective(tx, chat)))
            .unwrap()
    }

    #[test]
    fn the_mode_is_the_apps_unless_a_board_has_its_own() {
        let (_dir, db) = env();
        let chat = db
            .write(|tx| helm_chats::store::create_chat(tx, "C:/w", None, None, T0))
            .unwrap();
        let loose = db
            .write(|tx| helm_chats::store::create_chat(tx, "C:/w", None, None, T0))
            .unwrap();
        board(&db, "b1", "Garden");
        board(&db, "b2", "Kitchen");
        db.write(|tx| tx.execute("INSERT INTO chat_groups VALUES (?1, 'b1')", [&chat]))
            .unwrap();
        let app = |m: Option<Mode>| db.write(|tx| set_app(tx, m)).unwrap();
        let on_board = |b: &str, m: Option<Mode>| db.write(|tx| set_board(tx, b, m, T0)).unwrap();
        let ef = |mode, source| Effective { mode, source };

        // Check in is the default, and a bad stored value reads as the default too.
        assert_eq!(mode_of(&db, &chat), ef(Mode::CheckIn, Source::App));
        db.write(|tx| helm_core::prefs::set_raw(tx, PREF, &json!("sideways")))
            .unwrap();
        assert_eq!(mode_of(&db, &chat).mode, Mode::CheckIn);
        db.write(|tx| helm_core::prefs::set_raw(tx, PREF, &json!(7)))
            .unwrap();
        assert_eq!(mode_of(&db, &chat).mode, Mode::CheckIn);

        // The app's mode reaches every chat.
        app(Some(Mode::Run));
        assert_eq!(mode_of(&db, &chat), ef(Mode::Run, Source::App));
        assert_eq!(mode_of(&db, &loose), ef(Mode::Run, Source::App));

        // A board's own mode beats the app's, for the chats shown under it only.
        assert!(on_board("b1", Some(Mode::CheckIn)));
        assert_eq!(
            mode_of(&db, &chat),
            ef(Mode::CheckIn, Source::Board("b1".into()))
        );
        assert_eq!(mode_of(&db, &loose), ef(Mode::Run, Source::App));
        assert!(
            on_board("b2", Some(Mode::CheckIn)),
            "another board's override"
        );
        assert_eq!(mode_of(&db, &loose).mode, Mode::Run);

        // Clearing the override gives the chat back to the app.
        assert!(on_board("b1", None));
        assert_eq!(mode_of(&db, &chat), ef(Mode::Run, Source::App));
        // A folder project or an unknown id is not a board: nothing is written.
        assert!(!on_board("nope", Some(Mode::Run)));
        let listed = db.snapshot(|tx| overrides(tx)).unwrap();
        assert_eq!(listed, vec![("b2".into(), "Kitchen".into(), Mode::CheckIn)]);
        // A board removed takes its override with it.
        db.write(|tx| tx.execute("DELETE FROM projects WHERE id = 'b2'", []))
            .unwrap();
        assert!(db.snapshot(|tx| overrides(tx)).unwrap().is_empty());

        // The view says where the mode was set and what is waiting.
        on_board("b1", Some(Mode::CheckIn));
        let v = view(&db, Some(&chat), 3, T0).unwrap();
        assert_eq!(
            (v.app, v.effective, v.source),
            (Mode::Run, Mode::CheckIn, ModeSource::Board)
        );
        assert_eq!(
            v.board.map(|b| (b.id, b.mode)),
            Some(("b1".into(), Some(Mode::CheckIn)))
        );
        assert_eq!(
            v.counts,
            ModeCounts {
                decided: 0,
                needs_you: 3
            }
        );
        let v = view(&db, None, 0, T0).unwrap();
        assert_eq!(
            (v.effective, v.source, v.board),
            (Mode::Run, ModeSource::App, None)
        );
        app(None);
        assert_eq!(
            mode_of(&db, &loose).mode,
            Mode::CheckIn,
            "cleared: back to the default"
        );
    }

    #[test]
    fn every_launch_says_the_mode_and_a_later_turn_says_when_it_changed() {
        let (_dir, db) = env();
        let chat = db
            .write(|tx| helm_chats::store::create_chat(tx, "C:/w", None, None, T0))
            .unwrap();
        let section = WorkmodeSection { db: db.clone() };
        let launch = || {
            section
                .text(&LaunchCtx {
                    chat_id: &chat,
                    cwd: "C:/w",
                })
                .unwrap()
        };
        // A chat the app has not launched has nothing to compare with.
        assert_eq!(turn_line(&db, &chat), None);

        assert_eq!(launch(), CHECK_IN_TEXT);
        assert_eq!(
            turn_line(&db, &chat),
            None,
            "nothing changed since the launch"
        );
        db.write(|tx| set_app(tx, Some(Mode::Run))).unwrap();
        let line = turn_line(&db, &chat).unwrap();
        assert!(line.contains("run to completion"), "{line}");
        // A new launch tells the whole text of the new mode, and the line goes quiet.
        let text = launch();
        assert_eq!(text, RUN_TEXT);
        for must in [
            "outranks any brief",
            "Only the user can change it",
            "Stopped: blocker",
            "Done.",
            "Helm checks every stop",
        ] {
            assert!(text.contains(must), "{must}");
        }
        assert_eq!(turn_line(&db, &chat), None);
        db.write(|tx| set_app(tx, Some(Mode::CheckIn))).unwrap();
        assert!(turn_line(&db, &chat).unwrap().contains("check in with me"));
        assert_eq!(section.id(), "workmode");
        assert!(section.order() < 6, "ahead of the rules section");
    }
}
