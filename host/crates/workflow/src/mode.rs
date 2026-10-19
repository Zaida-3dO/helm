//! The per-chat Orchestrator state, table `chat_workflow_modes`.
//!
//! - A chat with no row follows `newChats`: Always → on, Never → off, Auto → auto. The
//!   default is Auto, so a new chat starts off until its agent
//!   turns it on for a task or the user does.
//! - Rows are written only by the user's `workflow_chat_mode_set` op, by [`pin`] at a chat's first
//!   launch while the master switch is on (so changing `newChats` later leaves used chats alone),
//!   and by an Auto chat's own root agent turning it **on** ([`agent_on`]). Nothing else.
//! - The workflow runs for a chat only when the master switch is on, the chat is a Claude Code
//!   chat, it is not archived, and its state is on. (There are no restricted chats yet.)

use crate::types::*;
use rusqlite::{params, OptionalExtension, Transaction};

pub const MAX_REASON: usize = 500;
pub const MAX_TOOLTIP_REASON: usize = 300;

pub const OFF_WHILE_RUNNING: &str =
    "Wait for this chat's turn to finish, or stop it, before turning Orchestrator off.";

pub fn init(tx: &Transaction<'_>) -> rusqlite::Result<()> {
    tx.execute_batch(
        "CREATE TABLE IF NOT EXISTS chat_workflow_modes (
             chat_id    TEXT PRIMARY KEY REFERENCES chats(id) ON DELETE CASCADE,
             mode       TEXT NOT NULL CHECK (mode IN ('on', 'off', 'auto')),
             set_by     TEXT NOT NULL CHECK (set_by IN ('user', 'agent', 'default')),
             reason     TEXT CHECK (reason IS NULL OR length(reason) <= 500),
             updated_at INTEGER NOT NULL
         );",
    )
}

fn mode_str(m: ChatMode) -> &'static str {
    match m {
        ChatMode::On => "on",
        ChatMode::Off => "off",
        ChatMode::Auto => "auto",
    }
}

fn mode_of(s: &str) -> ChatMode {
    match s {
        "on" => ChatMode::On,
        "auto" => ChatMode::Auto,
        _ => ChatMode::Off,
    }
}

fn by_str(b: SetBy) -> &'static str {
    match b {
        SetBy::User => "user",
        SetBy::Agent => "agent",
        SetBy::Default => "default",
    }
}

fn by_of(s: &str) -> SetBy {
    match s {
        "user" => SetBy::User,
        "agent" => SetBy::Agent,
        _ => SetBy::Default,
    }
}

pub fn from_new_chats(n: NewChats) -> ChatMode {
    match n {
        NewChats::Always => ChatMode::On,
        NewChats::Never => ChatMode::Off,
        NewChats::Auto => ChatMode::Auto,
    }
}

/// What the host knows of a chat that the rules read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ChatFacts {
    pub claude: bool,
    pub archived: bool,
    /// A turn is starting, running or stopping.
    pub busy: bool,
}

pub fn facts(tx: &Transaction<'_>, chat_id: &str) -> rusqlite::Result<Option<ChatFacts>> {
    tx.query_row(
        "SELECT harness, archived, status FROM chats WHERE id = ?1",
        [chat_id],
        |r| {
            let status: String = r.get(2)?;
            Ok(ChatFacts {
                claude: r.get::<_, String>(0)? == "claude",
                archived: r.get::<_, i64>(1)? != 0,
                busy: matches!(status.as_str(), "starting" | "running" | "stopping"),
            })
        },
    )
    .optional()
}

/// The stored row, if any: (mode, set by, reason).
pub fn stored(
    tx: &Transaction<'_>,
    chat_id: &str,
) -> rusqlite::Result<Option<(ChatMode, SetBy, Option<String>)>> {
    tx.query_row(
        "SELECT mode, set_by, reason FROM chat_workflow_modes WHERE chat_id = ?1",
        [chat_id],
        |r| {
            Ok((
                mode_of(&r.get::<_, String>(0)?),
                by_of(&r.get::<_, String>(1)?),
                r.get(2)?,
            ))
        },
    )
    .optional()
}

/// The chat's state: its row, else `newChats`.
pub fn current(
    tx: &Transaction<'_>,
    chat_id: &str,
    s: &WorkflowSettings,
) -> rusqlite::Result<(ChatMode, SetBy, Option<String>)> {
    Ok(stored(tx, chat_id)?.unwrap_or((from_new_chats(s.new_chats), SetBy::Default, None)))
}

/// Why the chat cannot have the workflow at all, or `None`.
pub fn unavailable(s: &WorkflowSettings, f: &ChatFacts) -> Option<&'static str> {
    if !s.enabled {
        Some("Orchestrator is off in Settings, so it is off for every chat.")
    } else if !f.claude {
        Some("Orchestrator is available only in Claude Code chats.")
    } else if f.archived {
        Some("Restore this chat to change Orchestrator.")
    } else {
        None
    }
}

/// Whether the workflow runs for this chat now.
pub fn in_effect(
    tx: &Transaction<'_>,
    chat_id: &str,
    s: &WorkflowSettings,
) -> rusqlite::Result<bool> {
    let Some(f) = facts(tx, chat_id)? else {
        return Ok(false);
    };
    Ok(unavailable(s, &f).is_none() && current(tx, chat_id, s)?.0 == ChatMode::On)
}

/// Strip control, format (bidi overrides, zero-width) and line characters, then cut to 300.
pub fn tooltip_reason(r: &str) -> String {
    r.chars()
        .filter(|c| !is_invisible(*c))
        .take(MAX_TOOLTIP_REASON)
        .collect()
}

/// Control, format and separator characters other than a plain space.
pub fn is_invisible(c: char) -> bool {
    c.is_control()
        || matches!(c as u32,
            0x00AD | 0x034F | 0x061C | 0x115F | 0x1160 | 0x17B4 | 0x17B5 | 0x180B..=0x180F
            | 0x200B..=0x200F | 0x2028..=0x202E | 0x2060..=0x206F | 0x2800 | 0x3164
            | 0xFE00..=0xFE0F | 0xFEFF | 0xFFA0 | 0xFFF9..=0xFFFB | 0xE0000..=0xE007F)
        || (c.is_whitespace() && c != ' ')
}

/// `app_orchestrator_on`'s reason: one plain sentence of 1–500 characters.
pub fn check_reason(r: &str) -> Result<String, &'static str> {
    const MSG: &str = "reason must be one plain sentence of 1-500 characters";
    let t = r.trim();
    if t.is_empty() || t.chars().count() > MAX_REASON || t.chars().any(is_invisible) {
        return Err(MSG);
    }
    Ok(t.to_owned())
}

pub fn view(
    tx: &Transaction<'_>,
    chat_id: &str,
    s: &WorkflowSettings,
) -> rusqlite::Result<Option<ChatModeView>> {
    let Some(f) = facts(tx, chat_id)? else {
        return Ok(None);
    };
    let (mode, set_by, reason) = current(tx, chat_id, s)?;
    let unavailable = unavailable(s, &f).map(str::to_owned);
    Ok(Some(ChatModeView {
        chat_id: chat_id.to_owned(),
        mode,
        set_by,
        reason: reason.as_deref().map(tooltip_reason),
        in_effect: unavailable.is_none() && mode == ChatMode::On,
        unavailable,
        running: f.busy,
    }))
}

fn write(
    tx: &Transaction<'_>,
    chat_id: &str,
    mode: ChatMode,
    by: SetBy,
    reason: Option<&str>,
    now: i64,
) -> rusqlite::Result<()> {
    tx.execute(
        "INSERT INTO chat_workflow_modes(chat_id, mode, set_by, reason, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5)
         ON CONFLICT(chat_id) DO UPDATE SET mode = excluded.mode, set_by = excluded.set_by,
             reason = excluded.reason, updated_at = excluded.updated_at",
        params![chat_id, mode_str(mode), by_str(by), reason, now],
    )?;
    Ok(())
}

/// At a chat's first launch while the master switch is on, pin the state `newChats` gives it, so
/// a later change of `newChats` affects only chats never used. Returns the chat's state.
pub fn pin(
    tx: &Transaction<'_>,
    chat_id: &str,
    s: &WorkflowSettings,
    now: i64,
) -> rusqlite::Result<ChatMode> {
    if let Some((m, _, _)) = stored(tx, chat_id)? {
        return Ok(m);
    }
    let m = from_new_chats(s.new_chats);
    if s.enabled && facts(tx, chat_id)?.is_some() {
        write(tx, chat_id, m, SetBy::Default, None, now)?;
    }
    Ok(m)
}

/// What a state change did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Changed {
    pub from: ChatMode,
    pub to: ChatMode,
    /// The transcript line for this change.
    pub row: String,
}

/// The user's switch. A request matching the current state writes nothing (except Off on an Auto
/// chat, which leaves Auto). Off is refused while the chat's turn runs.
pub fn user_set(
    tx: &Transaction<'_>,
    chat_id: &str,
    to: ChatMode,
    s: &WorkflowSettings,
    now: i64,
) -> rusqlite::Result<Result<Option<Changed>, String>> {
    let Some(f) = facts(tx, chat_id)? else {
        return Ok(Err("No such chat.".into()));
    };
    if let Some(why) = unavailable(s, &f) {
        return Ok(Err(why.into()));
    }
    let (from, _, _) = current(tx, chat_id, s)?;
    // A request matching the current state writes nothing. (Off on an Auto chat differs, so it
    // writes off and the chat leaves Auto.)
    if from == to {
        return Ok(Ok(None));
    }
    // Off, or Auto from On, would stop the workflow mid-turn.
    if f.busy && (to == ChatMode::Off || (from == ChatMode::On && to == ChatMode::Auto)) {
        return Ok(Err(OFF_WHILE_RUNNING.into()));
    }
    write(tx, chat_id, to, SetBy::User, None, now)?;
    let row = match to {
        ChatMode::On => {
            "You turned Orchestrator on for this chat. It applies from the chat's next turn.".into()
        }
        ChatMode::Off => "You turned Orchestrator off for this chat.".into(),
        ChatMode::Auto => "You set Orchestrator to Auto for this chat: the agent may turn it on for a task. It applies from the chat's next turn.".into(),
    };
    Ok(Ok(Some(Changed { from, to, row })))
}

/// `app_orchestrator_on {reason}`: the root agent of an Auto chat turns that same chat on. Checks
/// in the documented order; only ever writes `on`, set by `agent`.
pub fn agent_on(
    tx: &Transaction<'_>,
    chat_id: &str,
    reason: &str,
    s: &WorkflowSettings,
    now: i64,
) -> rusqlite::Result<Result<Changed, String>> {
    let reason = match check_reason(reason) {
        Ok(r) => r,
        Err(e) => return Ok(Err(e.into())),
    };
    if !s.enabled {
        return Ok(Err(
            "Orchestrator is off in Settings, so this chat can't turn it on.".into(),
        ));
    }
    let Some(f) = facts(tx, chat_id)? else {
        return Ok(Err("Orchestrator can't be turned on for this chat.".into()));
    };
    if !f.claude || f.archived {
        return Ok(Err("Orchestrator can't be turned on for this chat.".into()));
    }
    let (from, _, _) = current(tx, chat_id, s)?;
    match from {
        ChatMode::On => return Ok(Err("Orchestrator is already on for this chat.".into())),
        ChatMode::Off => return Ok(Err(
            "Only the user can turn Orchestrator on for this chat; ask them if the task needs it."
                .into(),
        )),
        ChatMode::Auto => {}
    }
    write(tx, chat_id, ChatMode::On, SetBy::Agent, Some(&reason), now)?;
    Ok(Ok(Changed {
        from,
        to: ChatMode::On,
        row: format!(
            "Orchestrator turned on by the agent: {}",
            tooltip_reason(&reason)
        ),
    }))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use helm_core::db::{Database, MigrationRegistry};

    pub fn db(chats: &[(&str, &str)]) -> (tempfile::TempDir, Database) {
        let mut reg = MigrationRegistry::new();
        crate::register_migrations(&mut reg);
        let dir = tempfile::tempdir().unwrap();
        let db = Database::open(&dir.path().join("t.sqlite3"), &reg).unwrap();
        db.write::<_, rusqlite::Error>(|tx| {
            tx.execute("INSERT INTO projects(id, name, created_at) VALUES ('p', 'P', 1)", [])?;
            for (c, harness) in chats {
                tx.execute(
                    "INSERT INTO chats(id, project_id, title, harness, created_at, updated_at) VALUES (?1, 'p', 'T', ?2, 1, 1)",
                    params![c, harness],
                )?;
            }
            Ok(())
        })
        .unwrap();
        (dir, db)
    }

    /// The master switch on, and new chats on (Always; the default is Auto).
    fn on() -> WorkflowSettings {
        WorkflowSettings {
            enabled: true,
            new_chats: NewChats::Always,
            ..Default::default()
        }
    }

    fn w<T>(db: &Database, f: impl FnOnce(&Transaction<'_>) -> rusqlite::Result<T>) -> T {
        db.write(f).unwrap()
    }

    #[test]
    fn a_chat_without_a_row_follows_new_chats_and_is_pinned_at_first_launch() {
        let (_d, db) = db(&[("a", "claude"), ("b", "claude")]);
        let mut s = on();
        s.new_chats = NewChats::Auto;
        assert_eq!(w(&db, |tx| current(tx, "a", &s)).0, ChatMode::Auto);
        assert_eq!(w(&db, |tx| pin(tx, "a", &s, 5)), ChatMode::Auto);
        // Changing the default now leaves the used chat alone and moves the unused one.
        s.new_chats = NewChats::Always;
        assert_eq!(w(&db, |tx| current(tx, "a", &s)).0, ChatMode::Auto);
        assert_eq!(w(&db, |tx| current(tx, "b", &s)).0, ChatMode::On);
        // With the master switch off nothing is pinned.
        let off = WorkflowSettings::default();
        w(&db, |tx| pin(tx, "b", &off, 6));
        assert_eq!(w(&db, |tx| stored(tx, "b")), None);
    }

    #[test]
    fn in_effect_needs_master_on_claude_unarchived_and_on() {
        let (_d, db) = db(&[("a", "claude"), ("x", "codex")]);
        assert!(!w(&db, |tx| in_effect(
            tx,
            "a",
            &WorkflowSettings::default()
        )));
        assert!(w(&db, |tx| in_effect(tx, "a", &on())));
        assert!(!w(&db, |tx| in_effect(tx, "x", &on())));
        w(&db, |tx| {
            tx.execute("UPDATE chats SET archived = 1 WHERE id = 'a'", [])
        });
        assert!(!w(&db, |tx| in_effect(tx, "a", &on())));
        let v = w(&db, |tx| view(tx, "a", &on())).unwrap();
        assert_eq!(
            v.unavailable.as_deref(),
            Some("Restore this chat to change Orchestrator.")
        );
        assert!(w(&db, |tx| view(tx, "nope", &on())).is_none());
    }

    #[test]
    fn off_is_refused_mid_turn_and_matching_requests_write_nothing() {
        let (_d, db) = db(&[("a", "claude")]);
        let s = on();
        // Always for new chats: asking for on changes nothing.
        assert_eq!(
            w(&db, |tx| user_set(tx, "a", ChatMode::On, &s, 1)),
            Ok(None)
        );
        w(&db, |tx| {
            tx.execute("UPDATE chats SET status = 'running' WHERE id = 'a'", [])
        });
        assert_eq!(
            w(&db, |tx| user_set(tx, "a", ChatMode::Off, &s, 2)),
            Err(OFF_WHILE_RUNNING.to_owned())
        );
        assert_eq!(
            w(&db, |tx| stored(tx, "a")),
            None,
            "the refusal wrote nothing"
        );
        w(&db, |tx| {
            tx.execute("UPDATE chats SET status = 'idle' WHERE id = 'a'", [])
        });
        let c = w(&db, |tx| user_set(tx, "a", ChatMode::Off, &s, 3))
            .unwrap()
            .unwrap();
        assert_eq!((c.from, c.to), (ChatMode::On, ChatMode::Off));
        assert_eq!(w(&db, |tx| stored(tx, "a")).unwrap().1, SetBy::User);
        assert_eq!(
            w(&db, |tx| user_set(tx, "a", ChatMode::Off, &s, 4)),
            Ok(None)
        );
        // Turning on is allowed mid-turn (it applies next turn).
        w(&db, |tx| {
            tx.execute("UPDATE chats SET status = 'running' WHERE id = 'a'", [])
        });
        assert!(w(&db, |tx| user_set(tx, "a", ChatMode::On, &s, 5))
            .unwrap()
            .is_some());
        // Auto from On would stop the workflow mid-turn too.
        assert_eq!(
            w(&db, |tx| user_set(tx, "a", ChatMode::Auto, &s, 6)),
            Err(OFF_WHILE_RUNNING.to_owned())
        );
        // Master off refuses the switch outright.
        assert!(w(&db, |tx| user_set(
            tx,
            "a",
            ChatMode::Off,
            &WorkflowSettings::default(),
            6
        ))
        .is_err());
    }

    #[test]
    fn the_agent_may_only_turn_an_auto_chat_on() {
        let (_d, db) = db(&[("a", "claude"), ("b", "claude"), ("x", "codex")]);
        let mut s = on();
        s.new_chats = NewChats::Auto;
        // Bad reasons first.
        for bad in ["", "two\nlines", &"x".repeat(501), "bidi \u{202e} trick"] {
            assert_eq!(
                w(&db, |tx| agent_on(tx, "a", bad, &s, 1)).unwrap_err(),
                "reason must be one plain sentence of 1-500 characters"
            );
        }
        assert!(w(&db, |tx| agent_on(
            tx,
            "a",
            "Needs a plan.",
            &WorkflowSettings::default(),
            1
        ))
        .unwrap_err()
        .contains("off in Settings"));
        assert!(w(&db, |tx| agent_on(tx, "x", "Needs a plan.", &s, 1)).is_err());
        let c = w(&db, |tx| {
            agent_on(tx, "a", "This is a multi-PR change.", &s, 2)
        })
        .unwrap();
        assert_eq!(
            c.row,
            "Orchestrator turned on by the agent: This is a multi-PR change."
        );
        let (m, by, r) = w(&db, |tx| stored(tx, "a")).unwrap();
        assert_eq!(
            (m, by, r.as_deref()),
            (
                ChatMode::On,
                SetBy::Agent,
                Some("This is a multi-PR change.")
            )
        );
        assert!(w(&db, |tx| agent_on(tx, "a", "Again.", &s, 3))
            .unwrap_err()
            .contains("already on"));
        // Once the user picks Off, the chat has left Auto and the agent is done.
        w(&db, |tx| user_set(tx, "b", ChatMode::Off, &s, 4)).unwrap();
        assert!(w(&db, |tx| agent_on(tx, "b", "Please.", &s, 5))
            .unwrap_err()
            .contains("Only the user"));
    }

    #[test]
    fn tooltip_reasons_are_stripped_and_cut() {
        assert_eq!(tooltip_reason("a\u{202e}b\u{200b}c\td"), "abcd");
        assert_eq!(tooltip_reason(&"é".repeat(400)).chars().count(), 300);
    }
}
