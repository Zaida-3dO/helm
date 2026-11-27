//! What a chat's own tool list holds, and how the rest are found.
//!
//! Every host tool belongs to a group (what `app_list_tools` lists them under). A few are *core*:
//! they are in every chat's tool list, because chats use them all the time. Everything else is
//! found with `app_list_tools` and called with `app_mcp_call`, so a chat pays for the tools it
//! uses and for no others. Families of small tools are merged into one tool with an `action`
//! argument ([`crate::facade`]); the members keep their names and still answer to them.
//!
//! This file is the one place that says which tool is core, which group each tool is in and what
//! each merged tool is. [`apply`] runs once, after every crate has registered its tools, and
//! panics on a tool this file does not know, so a new tool is never left unclassified.

use crate::facade::FacadeSpec;
use crate::registry::HostToolRegistry;

/// Tools in every chat's own list.
pub const CORE: &[&str] = &[
    "app_this_chat",
    "app_send_message",
    "app_create_chat",
    "app_read_chat",
    "app_list_chats",
    "app_wait_for",
    "app_open_chat",
    "app_open_panel",
    "app_stop_chat",
    "app_telegram_send",
    "app_offer_options",
    "app_progress",
    "app_background_result",
    "app_help",
    // Offered only where the launch says so (a key is set), and then always listed.
    "app_web_search",
];

/// Tools in the chat's own list only while Orchestrator is on for it.
pub const CORE_WITH_ORCHESTRATOR: &[&str] = &["app_workflow", "app_workflow_report"];

/// Group -> tools. A merged tool is listed under its own group too.
const GROUPS: &[(&str, &[&str])] = &[
    (
        "chats",
        &[
            "app_this_chat",
            "app_list_chats",
            "app_read_chat",
            "app_open_chat",
            "app_open_panel",
            "app_forward_port",
            "app_send_message",
            "app_create_chat",
            "app_stop_chat",
            "app_chat",
            "app_update_chat",
            "app_fork_chat",
            "app_set_chat_archived",
            "app_roll_over",
            "app_queue_status",
            "app_queue_action",
            "app_wait_for",
            "app_background_result",
            "app_show_images",
            "app_offer_options",
            "app_progress",
            "app_link_item",
            "app_resolve_side_chat",
            "app_resume_saved_agent",
        ],
    ),
    (
        "attention",
        &[
            "app_attention",
            "app_flag_attention",
            "app_list_attention",
            "app_update_attention",
        ],
    ),
    (
        "feedback",
        &[
            "app_helm_feedback",
            "app_feedback",
            "app_feedback_summary",
            "app_create_feedback",
            "app_append_feedback",
            "app_get_feedback",
            "app_list_feedback",
            "app_update_feedback",
        ],
    ),
    (
        "schedules",
        &[
            "app_schedule",
            "app_create_schedule",
            "app_update_schedule",
            "app_cancel_schedule",
            "app_list_schedules",
        ],
    ),
    (
        "mcp",
        &[
            "app_mcp_servers",
            "app_list_mcp_servers",
            "app_add_mcp_server",
            "app_edit_mcp_server",
            "app_update_mcp_server",
            "app_remove_mcp_server",
            "app_set_mcp_secret",
            "app_set_mcp_level",
            "app_google_sign_in",
            "app_import_mcp_servers",
            "app_mcp_oauth_header",
            "app_browser_step",
            "app_browser_step_result",
        ],
    ),
    (
        "projects",
        &[
            "app_project",
            "app_list_projects",
            "app_create_project",
            "app_update_project",
            "app_read_project_context",
            "app_update_project_context",
            "app_project_startup",
            "app_startup_done",
            "app_startup_turn_end",
        ],
    ),
    (
        "workspace",
        &[
            "app_worktree_status",
            "app_register_checkout",
            "app_repo_locations",
            "app_update_repo",
            "app_ide_context",
        ],
    ),
    (
        "saved",
        &[
            "app_saved",
            "app_save_item",
            "app_list_saved_items",
            "app_update_saved_item",
        ],
    ),
    (
        "knowledge",
        &[
            "app_knowledge",
            "app_list_knowledge",
            "app_add_skill",
            "app_remove_skill",
            "app_remove_rule",
            "app_save_rule",
            "app_save_memory",
            "app_memory_index",
        ],
    ),
    ("pages", &["app_pages", "app_pin_page", "app_list_pages"]),
    (
        "settings",
        &[
            "app_settings",
            "app_get_settings",
            "app_update_settings",
            "app_stage_settings",
            "app_reload_configuration",
            "app_gpu",
        ],
    ),
    ("updates", &["app_update"]),
    (
        "workflow",
        &[
            "app_workflow",
            "app_workflow_report",
            "app_orchestrator_on",
            "app_request_allow",
            "app_workflow_gate",
            "app_workflow_consent",
            "app_workflow_take_over",
            "app_workflow_turn_end",
            "app_consent_check",
        ],
    ),
    ("telegram", &["app_telegram_send", "app_telegram_ask"]),
    ("usage", &["app_usage_summary"]),
    ("voice", &["app_voice_show", "app_make_room_for_speech"]),
    (
        "gmail",
        &[
            "app_gmail_search",
            "app_gmail_read",
            "app_gmail_thread",
            "app_gmail_labels",
            "app_gmail_label",
            "app_gmail_drafts",
            "app_gmail_draft",
            "app_gmail_trash",
            "app_gmail_send",
        ],
    ),
    ("web", &["app_web_search"]),
    ("help", &["app_help"]),
    ("waits", &["app_list_waiting", "app_answer_waiting"]),
];

/// The merged tools.
pub const FACADES: &[FacadeSpec] = &[
    FacadeSpec {
        name: "app_chat",
        group: "chats",
        description: "Change a chat: fork it into a new idle chat (fork), archive or restore it (archive), or rename, pin, move, nest, mute or change the model or effort of one (update). Act only when the user explicitly asked.",
        guidelines: "fork: chatId, optional messageId (copy up to it) and title; the copy is idle on the same board and folder.\narchive: chatId, archived (true or false). Archiving this chat itself ends your turn.\nupdate: chatId plus any of title, pinned, muted, model, effort, projectId (a board's id), parentChatId, reason. Say why in one sentence (reason). model can be a Claude model or local/<server>/<model>.\nConfirm an ambiguous chat with app_list_chats first, and report what changed.",
        core: false,
        actions: &[
            ("fork", "app_fork_chat"),
            ("archive", "app_set_chat_archived"),
            ("update", "app_update_chat"),
        ],
        fixed: &[],
    },
    FacadeSpec {
        name: "app_attention",
        group: "attention",
        description: "The user's inbox of chats needing attention: flag this chat with one question you cannot go on without (flag), list the chats that need attention (list), or mark one addressed (address) or snooze it (snooze).",
        guidelines: "flag: question and reason (why it blocks you), optional priority. One flag per chat; filing again replaces it. It clears when the user writes in the chat. At most 6 an hour. Use it only when you cannot go on without the user, never for progress; then end your turn.\nlist: no arguments.\naddress: chatId; marks the chat's attention addressed. snooze: chatId and minutes (1-10080). Only when the user asked.",
        core: false,
        actions: &[
            ("flag", "app_flag_attention"),
            ("list", "app_list_attention"),
            ("address", "app_update_attention"),
            ("snooze", "app_update_attention"),
        ],
        fixed: &[
            ("address", "action", "address"),
            ("snooze", "action", "snooze"),
        ],
    },
    FacadeSpec {
        name: "app_helm_feedback",
        group: "feedback",
        description: "Feedback about Helm itself. Rate a Helm feature you used (rate), file a new item (create), add to one (append), read one (get), list them (list), manage one (update), or summarise agents' ratings (summary).",
        guidelines: "rate: feature, score -2..2, kind (existing, missing, remove, tweak), note; once per feature per turn.\ncreate: title and text; forUser true when you record what the user told you. Check list first and append instead of filing a duplicate.\nappend: id and text (same rules as create).\nget: id. list: filters status, author, since, text, limit.\nupdate: id; status, note, boardItem, title, text, entryId, delete. Whatever you change is recorded under your chat.\nsummary: per-feature ratings; filters feature, since, minScore, maxScore, order, limit.\nNever put secrets in feedback. It is stored on this machine only.",
        core: false,
        actions: &[
            ("rate", "app_feedback"),
            ("create", "app_create_feedback"),
            ("append", "app_append_feedback"),
            ("get", "app_get_feedback"),
            ("list", "app_list_feedback"),
            ("update", "app_update_feedback"),
            ("summary", "app_feedback_summary"),
        ],
        fixed: &[],
    },
    FacadeSpec {
        name: "app_schedule",
        group: "schedules",
        description: "Schedules that send a saved message into a chat (or a fresh chat in a project), or show a reminder, once, every so often or on chosen weekdays: create, update (change, pause or resume), cancel or list. Only while Helm is open.",
        guidelines: "create: title, message, timing ({kind: once|interval|weekly, ...}); optional chatId (defaults to this chat), projectId, destination, kind.\nupdate: scheduleId plus anything to change; paused true or false pauses or resumes.\ncancel: scheduleId; for good.\nlist: includeInactive to include finished ones.\nAct only when the user explicitly asked for a schedule, and report what changed.",
        core: false,
        actions: &[
            ("create", "app_create_schedule"),
            ("update", "app_update_schedule"),
            ("cancel", "app_cancel_schedule"),
            ("list", "app_list_schedules"),
        ],
        fixed: &[],
    },
    FacadeSpec {
        name: "app_mcp_servers",
        group: "mcp",
        description: "Manage the MCP servers Helm connects to (Settings > MCP): list them with the revision every change needs (list), add, edit, enable or disable (set_enabled), or remove one, give a person their own value for a secret (set_secret), raise the Gmail or Calendar level (set_level), or start a Google sign-in (google_sign_in). Only on the user's explicit request.",
        guidelines: "Every change needs the current revision from list. Saving never connects or signs in; OAuth sign-in is the user's, in Settings > MCP.\nlist: no arguments; secret values are never shown.\nadd: revision and server {name, type, command/args or url, env, headers...}.\nedit: revision, serverId and the fields to change.\nset_enabled: revision, serverId, enabled.\nremove: revision, serverId.\nA change only the user may make is refused with how to ask: add askUser: true (and reason) to any change and the user gets an Approve card instead of Helm saving it.\nset_secret: server, profile, secret (the variable or header name), valueFile (read once on approval, then deleted unless keepFile) and reason. Always an Approve card; it never shows the value.\nset_level: server (the Google accounts server), service (gmail or calendar), level and reason. Always an Approve card; it covers every account on that server.\ngoogle_sign_in: optional server and email. An Approve card, then Google's own page opens on this PC for the user to pick the account and allow it; it cannot set the sign-in client, which only the user can.\nstdio servers run a real .exe; remote servers use https; plain http only for this PC or a private network with allowPrivateHttp (only when the user asked). Prefer environment or file references for credentials. If a reply is lost, list before retrying.",
        core: false,
        actions: &[
            ("list", "app_list_mcp_servers"),
            ("add", "app_add_mcp_server"),
            ("edit", "app_edit_mcp_server"),
            ("set_enabled", "app_update_mcp_server"),
            ("remove", "app_remove_mcp_server"),
            ("set_secret", "app_set_mcp_secret"),
            ("set_level", "app_set_mcp_level"),
            ("google_sign_in", "app_google_sign_in"),
        ],
        fixed: &[],
    },
    FacadeSpec {
        name: "app_project",
        group: "projects",
        description: "Helm's projects: list them with ids and folders (list), add a folder project (create), rename, pin, describe, colour or archive one (update), and read or replace the context note every chat in this chat's folder shares (read_context, update_context).",
        guidelines: "list: optional query. Boards (kind board) are where chats are shown; folder projects (kind folder) are where they run.\ncreate: name, optional path. Check list for duplicates first.\nupdate: projectId plus name, pinned, color, icon, description (one line, 200 characters), archived, primaryFolder.\nread_context: no arguments; returns text and revision.\nupdate_context: text (4000 characters at most, lasting facts only) and expectedRevision from read_context; read first and merge.\nAct only when the user asked, and report what changed.",
        core: false,
        actions: &[
            ("list", "app_list_projects"),
            ("create", "app_create_project"),
            ("update", "app_update_project"),
            ("read_context", "app_read_project_context"),
            ("update_context", "app_update_project_context"),
        ],
        fixed: &[],
    },
    FacadeSpec {
        name: "app_saved",
        group: "saved",
        description: "Items the user saved to come back to: save one with a title and body and an optional reminder (save), list them (list), or change, move between states or remove one (update).",
        guidelines: "save: title, body; messageId or captureKey so the same thing saved twice returns the existing item; remindAt (epoch ms).\nlist: query, state, page.\nupdate: id plus title, body, state (saved, filed, done, dismissed), remindAt (null clears), remove (only items this chat saved).\nAct only when the user asked, and report what changed.",
        core: false,
        actions: &[
            ("save", "app_save_item"),
            ("list", "app_list_saved_items"),
            ("update", "app_update_saved_item"),
        ],
        fixed: &[],
    },
    FacadeSpec {
        name: "app_knowledge",
        group: "knowledge",
        description: "The rules, skills and agents that apply to this chat: list them (list), add a skill from a folder (add_skill), remove a skill (remove_skill) or a rule (remove_rule). Save a rule with app_save_rule and a memory for someone else with app_save_memory. Only on the user's explicit request.",
        guidelines: "list: scope me, everyone or all. Skills show how they load.\nadd_skill: path to a folder holding a SKILL.md; scope me (default) or everyone.\nremove_skill: name, optional scope. remove_rule: path from list, optional scope.\nThe user is shown a notice with Undo. Say in one line where you saved it.",
        core: false,
        actions: &[
            ("list", "app_list_knowledge"),
            ("add_skill", "app_add_skill"),
            ("remove_skill", "app_remove_skill"),
            ("remove_rule", "app_remove_rule"),
        ],
        fixed: &[],
    },
    FacadeSpec {
        name: "app_pages",
        group: "pages",
        description: "Pinned pages: a local .html file (a dashboard, tracker or checklist you built) shown in the user's Pages list. Pin one (pin) or list the pinned pages (list). How a page is built: app_help helm/pages.",
        guidelines: "pin: path (absolute or relative to this chat's folder) and optional title. Pinning the same file again updates its title and links the page to this chat. Only when the user asked for a page.\nlist: id, title, path, linked chat and helm:// link of each; show one with app_open_panel {pageId}.",
        core: false,
        actions: &[("pin", "app_pin_page"), ("list", "app_list_pages")],
        fixed: &[],
    },
    FacadeSpec {
        name: "app_settings",
        group: "settings",
        description: "Helm settings: read them with options and current values (get), or change 1-16 live with Undo for the user (update). Every setting is either changed here or staged with app_stage_settings for one Approve click. Read first: a value outside a key's options is refused.",
        guidelines: "get: optional keys. Each key's options give its type, allowed values, default and what it does; options that say needsApproval mark the few settings you stage instead of changing.\nupdate: changes {key: value}, keys applied alphabetically, stopping at the first failure. A value replaces the stored one whole, except fontSizes, sounds and orchestrator (send only the fields to change); null restores the default. A setting that needs approval (or a part of one) changes nothing here: the answer is the exact app_stage_settings call to make.\nTwo ways only: change it yourself with update (the user is told and can Undo), or stage it with app_stage_settings (the user approves on one card). Nothing is left for the user to do on their own.",
        core: false,
        actions: &[("get", "app_get_settings"), ("update", "app_update_settings")],
        fixed: &[],
    },
];

/// The group a tool is filed under, or `None` for a tool this file does not know.
pub fn group_of(tool: &str) -> Option<&'static str> {
    GROUPS
        .iter()
        .find(|(_, tools)| tools.contains(&tool))
        .map(|(g, _)| *g)
}

/// Set every registered tool's group and core flag, and register the merged tools. Panics on a
/// registered tool that has no group here.
pub fn apply(reg: &mut HostToolRegistry) {
    for name in reg.names().collect::<Vec<_>>() {
        let Some(group) = group_of(name) else {
            panic!("{name} has no group in host/crates/hosttools/src/surface.rs");
        };
        reg.set_surface(
            name,
            group,
            CORE.contains(&name),
            CORE_WITH_ORCHESTRATOR.contains(&name),
        );
    }
    for f in FACADES {
        reg.facade(*f);
    }
    reg.slim_descriptions(MAX_SHORT_DESCRIPTION);
    // Last: it answers from a snapshot of everything above.
    crate::help::register(reg);
    set_help_surface(reg);
}

fn set_help_surface(reg: &mut HostToolRegistry) {
    reg.set_surface("app_help", "help", true, false);
}

/// The longest description a listed tool keeps in its list entry; the rest is on demand.
pub const MAX_SHORT_DESCRIPTION: usize = 440;
