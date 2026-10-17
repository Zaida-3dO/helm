//! The table-level settings class registry, beside the
//! preference keys' `CLASSIFIED`. Every SQLite table any crate creates is listed here with who may
//! change it:
//!
//! - `UserOnly`: settings-like rows only the user edits (in Settings); no agent tool writes them.
//! - `Tool`: rows an agent may change, only through that feature's own host tool.
//! - `Data`: the app's own records (chats, messages, projects, caches), never a setting.
//!
//! The app's `every_table_is_classified` test opens a database with every registered migration and
//! fails on any table missing from this list. A crate adding a table adds one line here.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TableClass {
    UserOnly(&'static str),
    Tool,
    Data,
}

use TableClass::*;

pub const CLASSIFIED_TABLES: &[(&str, TableClass)] = &[
    // Core.
    ("counters", Data),
    ("preferences", Data), // classified per key in prefs::classes
    ("projects", Data),
    ("chats", Data),
    ("messages", Data),
    ("submissions", Data),
    ("runs", Data),
    ("entries", Data),
    // Chats.
    ("chat_completions", Data),
    ("chat_file_cleanup", Data),
    ("chat_queue_holds", Data),
    ("chat_retry_markers", Data),
    ("chat_titles", Data),
    ("draft_queue_slots", Data),
    ("message_records", Data),
    ("queued_submissions", Data),
    ("restart_continuations", Data),
    ("submission_context", Data),
    ("submission_meta", Data),
    ("unread_marks", Data),
    ("chat_activity", Data),
    ("chat_forks", Data),
    ("fork_carry", Data),
    ("chat_attention", Data),
    (
        "chat_mutes",
        UserOnly("Muting a chat is yours to decide; agents never mute or unmute."),
    ),
    // The chat search index (an FTS5 table and its shadow tables).
    ("message_search", Data),
    ("message_search_data", Data),
    ("message_search_idx", Data),
    ("message_search_docsize", Data),
    ("message_search_config", Data),
    // Projects.
    ("board_projects", Data),
    ("board_sync_state", Data),
    ("chat_groups", Data),
    ("project_archive_members", Data),
    ("project_local", Data),
    ("project_parents", Data),
    ("project_top_level", Data),
    (
        "board_connection",
        UserOnly("The Agent Standup address is yours to set in Settings; agents never change it."),
    ),
    // Panel.
    ("panel_tab_sets", Data),
    // Search.
    ("search_opens", Data),
    // Actions.
    (
        "action_sets",
        UserOnly("Your own one-click buttons; only you edit them in Settings."),
    ),
];

/// The class of a table, or None when it is unclassified.
pub fn table_class(name: &str) -> Option<TableClass> {
    CLASSIFIED_TABLES
        .iter()
        .find(|(n, _)| *n == name)
        .map(|(_, c)| *c)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_unique_and_reasons_short() {
        let mut seen = std::collections::HashSet::new();
        for (n, c) in CLASSIFIED_TABLES {
            assert!(seen.insert(*n), "{n} is listed twice");
            if let UserOnly(why) = c {
                assert!(why.len() <= 130, "{n}: reason too long");
            }
        }
        assert_eq!(table_class("chats"), Some(Data));
        assert_eq!(table_class("nope"), None);
    }
}
