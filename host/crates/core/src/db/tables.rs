//! The table-level settings class registry, beside the
//! preference keys' `CLASSIFIED`. Every SQLite table any crate creates is listed here with who may
//! change it:
//!
//! - `Tool`: rows an agent may change, only through that feature's own host tool or through the
//!   settings tools (`app_update_settings`, or `app_stage_settings` when the key needs approval).
//! - `Data`: the app's own records (chats, messages, projects, caches), never a setting.
//!
//! The app's `every_table_is_classified` test opens a database with every registered migration and
//! fails on any table missing from this list. A crate adding a table adds one line here.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TableClass {
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
    ("chat_summary", Data),
    ("chat_finished_sweep", Data),
    ("chat_finished_sweep_start", Data),
    ("messages", Data),
    ("submissions", Data),
    ("runs", Data),
    ("entries", Data),
    // Chats.
    ("chat_completions", Data),
    ("chat_file_cleanup", Data),
    ("chat_retry_markers", Data),
    ("chat_titles", Data),
    ("message_records", Data),
    ("draft_queue_slots", Data),
    ("queued_submissions", Data),
    ("restart_continuations", Data),
    ("submission_context", Data),
    ("submission_meta", Data),
    ("unread_marks", Data),
    ("chat_activity", Data),
    // A turn start held for capacity, changed only through the queue's own ops and tools.
    ("chat_dispatch_holds", Tool),
    ("chat_forks", Data),
    ("fork_carry", Data),
    ("pending_edits", Data),
    ("chat_attachments", Data),
    ("shown_galleries", Data),
    ("shown_images", Data),
    ("option_boards", Data),
    ("chat_attachment_refs", Data), // file, folder and chat references
    ("chat_attention", Data),
    ("chat_triage", Data),
    ("chat_flags", Data),
    // A turn end the stop judge is holding while it decides (cleared on every exit and at start-up).
    ("chat_stop_holds", Data),
    ("chat_turn_usage", Data),
    // What a roll-over successor continues, and what a chat was told and reported of its project's
    // startup instructions (the instructions themselves are a column of project_local).
    ("chat_succession", Data),
    // The features waiting to be run one after another, started by the host from the queue panel.
    ("build_queue", Data),
    ("chat_startup", Data),
    ("chat_startup_sent", Data),
    ("agent_waits", Data),
    ("saved_items", Data),
    ("saved_item_notes", Data),
    ("chat_mutes", Tool),
    // The chat search index (an FTS5 table and its shadow tables).
    ("message_search", Data),
    ("message_search_data", Data),
    ("message_search_idx", Data),
    ("message_search_docsize", Data),
    ("message_search_config", Data),
    // Claude Code's own chat records: Helm's index of them (an FTS5 table and its shadows).
    ("claude_chat_records", Data),
    ("claude_chat_search", Data),
    ("claude_chat_search_content", Data),
    ("claude_chat_search_data", Data),
    ("claude_chat_search_idx", Data),
    ("claude_chat_search_docsize", Data),
    ("claude_chat_search_config", Data),
    // Projects.
    ("board_projects", Data),
    ("board_sync_state", Data),
    ("chat_groups", Data),
    ("project_archive_members", Data),
    ("project_local", Data),
    ("project_parents", Data),
    ("project_pins", Data),
    ("project_top_level", Data),
    ("repositories", Tool),
    ("repo_checkouts", Tool),
    ("repo_ignored", Data),
    ("repo_worktree_settings", Tool),
    ("worktree_config", Tool),
    ("project_notes", Tool),
    ("board_connection", Tool),
    // Panel.
    ("panel_tab_sets", Data),
    ("panel_context", Tool),
    ("panel_context_sent", Data),
    ("review_comments", Data),
    ("panel_vaults", Tool),
    ("panel_vault_default", Tool),
    // Search.
    ("search_opens", Data),
    // Actions.
    ("action_sets", Tool),
    // Consent.
    ("consent_requests", Data),
    ("consent_asks", Data),
    ("consent_records", Data),
    // Host tools: the "Changed by chats" ring with Undo.
    ("settings_changes", Data),
    // A chat's own reply length; follows the chat's deletion.
    ("chat_verbosity", Data),
    ("agent_feedback", Data),
    ("feedback_items", Tool),
    ("feedback_entries", Tool),
    // A feedback item's history and its soft deletes, written through app_update_feedback.
    ("feedback_events", Tool),
    ("feedback_deleted", Tool),
    // Usage: the last plan-usage reading.
    ("plan_usage", Data),
    // Item links: which chats work on which board item. Stored only in Helm.
    ("item_chat_links", Data),
    // Schedules: saved prompts and their run receipts.
    ("schedules", Tool),
    ("schedule_runs", Data),
    ("schedule_reminders", Data),
    // Workflow: Settings > Orchestrator and the per-chat Orchestrator state.
    ("workflow_settings", Tool),
    // An Auto chat's own root agent may turn that chat on (app_orchestrator_on); nothing else.
    ("chat_workflow_modes", Tool),
    // Tasks, changes and their record: written through app_workflow / app_workflow_report.
    ("workflow_runs", Tool),
    ("workflow_changes", Tool),
    ("workflow_events", Data),
    // Stage records queued for Agent Standup (the app writes them; agents never do).
    ("workflow_standup_outbox", Data),
    // The land step's readiness waits.
    ("workflow_land_waits", Data),
    // Follow-ups queued from reviews and post-land reports (read and dropped through app_workflow).
    ("workflow_followups", Tool),
    // AFK and the usage governor.
    ("afk_settings", Tool),
    ("afk_spans", Data),
    ("afk_digests", Data),
    ("afk_events", Data),
    ("governor_settings", Tool),
    // The question gatekeeper: its settings and its audit trail of the asks it looked at.
    ("gate_settings", Tool),
    ("gate_asks", Data),
    // The run of nudges a chat is on (the gatekeeper's own count), and a board's working mode.
    ("gate_stop_chains", Data),
    // Chats another chat placed on a board, which keep the mode they had there.
    ("chat_agent_moves", Data),
    ("board_working_modes", Tool),
    // Local model servers: the addresses, switches and numbers around them.
    ("local_model_settings", Tool),
    // Which Ollama variants Helm made for chat windows; written and pruned by Helm alone.
    ("local_model_variants", Data),
    // Whether local models are unloaded and kept off, and since when.
    ("local_model_lock", Tool),
    // The GPU hold a chat sets while it uses the card (app_gpu hold and release).
    ("local_model_hold", Tool),
    // Web search: the Claude-chats switch (agents may change it through the settings tools) and
    // the last settings file imported from. The Brave key is in the credential store, not here.
    ("web_search_settings", Tool),
    // Profiles (the people who use Helm) and the label each chat carries; chats never change them.
    ("profiles", Tool),
    ("chat_profiles", Tool),
    ("chat_shares", Tool),
    // A person the user unlinked or deleted stays so until they link them on purpose.
    ("declined_people", Data),
    // Messages other devices sent here, kept a week so a repeat runs once.
    ("remote_sends", Data),
    // This PC's copy of the other devices of the household and the chats they run: what the hub
    // and those devices said, never a setting.
    ("devices", Data),
    ("mirror_projects", Data),
    ("mirror_chats", Data),
    ("mirror_rows", Data),
    ("mirror_cursor", Data),
    // Staged images and files waiting to cross to a chat another device runs.
    ("mirror_attachments", Data),
    // This install's own device id and name; written by Helm and the user's rename alone.
    ("device_self", Tool),
    // Sealed pages: pins are made through app_pin_page; a page's saved state is its own.
    ("pinned_pages", Tool),
    ("page_state", Data),
    // Memory indexing: what is uploaded to the hub's search index is the user's decision (agents
    // go through app_memory_index and the Allow card); the doc rows are only a record of what was sent.
    ("memindex_sources", Tool),
    ("memindex_settings", Tool),
    ("memindex_docs", Data),
    // Dictation: which speech-to-text server the microphone button sends audio to.
    ("dictation_settings", Tool),
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
    fn names_are_unique() {
        let mut seen = std::collections::HashSet::new();
        for (n, _) in CLASSIFIED_TABLES {
            assert!(seen.insert(*n), "{n} is listed twice");
        }
        assert_eq!(table_class("chats"), Some(Data));
        assert_eq!(table_class("nope"), None);
    }
}
