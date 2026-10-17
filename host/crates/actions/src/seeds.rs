//! The built-in buttons.
//!
//! The texts are plain prompts for the agent. No project id, path,
//! machine or project name belongs here (the repo is public): the new-session launcher is seeded
//! hidden and unbound, and the user binds it once in Settings.

use crate::types::{ActionButton, ActionKind, Placement};

/// Bump when a seed is added, and give the new seed that version in `since`.
pub const SEED_VERSION: i64 = 1;

#[derive(Clone, Copy)]
pub struct Seed {
    pub id: &'static str,
    pub since: i64,
    pub label: &'static str,
    pub kind: ActionKind,
    pub text: &'static str,
    pub placement: Placement,
    pub hidden: bool,
}

pub const SEEDS: &[Seed] = &[
    Seed {
        id: "update",
        since: 1,
        label: "Update",
        kind: ActionKind::Prompt,
        text: "Please give me a brief plain-language status: what is finished, what is under way, what is stuck, and what you need from me.",
        placement: Placement::Topbar,
        hidden: false,
    },
    Seed {
        id: "needs-me",
        since: 1,
        label: "Needs me",
        kind: ActionKind::Prompt,
        text: "What needs my attention right now, and why? Only list things that truly need a decision from me (check each one) and give me the options for each.",
        placement: Placement::Topbar,
        hidden: false,
    },
    Seed {
        id: "ship-it",
        since: 1,
        label: "Ship it",
        kind: ActionKind::Prompt,
        text: "I am happy with this. Commit only your own changes, push, open or update the pull request, and merge to main once review passes. Tell me what landed.",
        placement: Placement::Topbar,
        hidden: false,
    },
    Seed {
        id: "close-session",
        since: 1,
        label: "Close session",
        kind: ActionKind::Prompt,
        text: "Please wrap up this session: record what we learned, clean up worktrees and branches, then commit and push. Report each step as pass or fail.",
        placement: Placement::Topbar,
        hidden: false,
    },
    Seed {
        id: "new-session",
        since: 1,
        label: "New session",
        kind: ActionKind::NewChat,
        text: "Let's start a new session.",
        placement: Placement::Topbar,
        hidden: true,
    },
    Seed {
        id: "git-sync",
        since: 1,
        label: "Git sync",
        kind: ActionKind::Prompt,
        text: "Please sync this repo: fetch, fast-forward only if that is safe, and tell me about anything unexpected. Do not commit.",
        placement: Placement::Overflow,
        hidden: false,
    },
];

/// The seeds introduced after `have` (0 on first run), as buttons.
pub(crate) fn seeds_after_in(list: &[Seed], have: i64) -> Vec<ActionButton> {
    list.iter()
        .filter(|s| s.since > have)
        .map(|s| ActionButton {
            id: format!("seed-{}", s.id),
            label: s.label.into(),
            kind: s.kind,
            text: s.text.into(),
            project_id: None,
            placement: s.placement,
            hidden: s.hidden,
            seed_id: Some(s.id.into()),
        })
        .collect()
}
