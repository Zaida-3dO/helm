//! The built-in buttons.
//!
//! The texts are plain prompts for the agent. No project id, path,
//! machine or project name belongs here (the repo is public): the new-session launcher is seeded
//! hidden and unbound, and the user binds it once in Settings.
//!
//! Most requests that used to be buttons are skills now (Report progress, Needs me, Ship it,
//! Close session, Git sync): the agent loads them by name, and the chat itself is the entry. The
//! seeds retired in the version that moved them are listed in [`RETIRED`] so an install made by an
//! older build loses them on the version bump — a button the user edited is theirs and stays.

use crate::types::{ActionButton, ActionKind, Placement};

/// Bump when a seed is added, and give the new seed that version in `since`. Bumping also applies
/// the `relabel_from` fixes below and the [`RETIRED`] removals to installs made by an older build.
pub const SEED_VERSION: i64 = 3;

#[derive(Clone, Copy)]
pub struct Seed {
    pub id: &'static str,
    pub since: i64,
    pub label: &'static str,
    /// A label this button shipped with in an older build. On the version bump, a stored copy of
    /// this seed still carrying exactly that label gets the current one; a label the user changed
    /// is left alone.
    pub relabel_from: Option<&'static str>,
    pub kind: ActionKind,
    pub text: &'static str,
    pub placement: Placement,
    pub hidden: bool,
}

pub const SEEDS: &[Seed] = &[Seed {
    id: "new-session",
    since: 1,
    label: "New session",
    relabel_from: None,
    kind: ActionKind::NewChat,
    text: "Let's start a new session.",
    placement: Placement::Topbar,
    hidden: true,
}];

/// A seed a later build retired: its request moved to the skills layer. On the version bump that
/// retired it (`retired_in`), a stored button of that seed is removed — but only while it is still
/// exactly what was shipped, label and text both. A button the user touched is theirs and stays.
#[derive(Clone, Copy)]
pub struct Retired {
    pub id: &'static str,
    pub retired_in: i64,
    pub label: &'static str,
    /// A label it shipped with in an older build: a stored copy still carrying that exact label
    /// retires too.
    pub relabel_from: Option<&'static str>,
    pub text: &'static str,
}

pub const RETIRED: &[Retired] = &[
    Retired {
        id: "update",
        retired_in: 3,
        label: "Report progress",
        relabel_from: Some("Update"),
        text: "Please give me a brief plain-language status: what is finished, what is under way, what is stuck, and what you need from me.",
    },
    Retired {
        id: "needs-me",
        retired_in: 3,
        label: "Needs me",
        relabel_from: None,
        text: "What needs my attention right now, and why? Only list things that truly need a decision from me (check each one) and give me the options for each.",
    },
    Retired {
        id: "ship-it",
        retired_in: 3,
        label: "Ship it",
        relabel_from: None,
        text: "I am happy with this. Commit only your own changes, push, open or update the pull request, and merge to main once review passes. Tell me what landed.",
    },
    Retired {
        id: "close-session",
        retired_in: 3,
        label: "Close session",
        relabel_from: None,
        text: "Please wrap up this session: record what we learned, clean up worktrees and branches, then commit and push. Report each step as pass or fail.",
    },
    Retired {
        id: "git-sync",
        retired_in: 3,
        label: "Git sync",
        relabel_from: None,
        text: "Please sync this repo: fetch, fast-forward only if that is safe, and tell me about anything unexpected. Do not commit.",
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
