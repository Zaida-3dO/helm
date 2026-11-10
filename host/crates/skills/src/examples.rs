//! The example agents and skills Helm offers.
//!
//! The files sit beside this crate in `examples/` and are compiled in. Each is copied into the
//! Everyone layer once, the first time Helm sees it, and is the user's from then on: edit it, replace
//! it or remove it, and it stays that way. A small record in the profile lists what has already
//! been offered, so a removed example does not come back, while an example added by a later
//! version is offered once. A file of the same name that is already there is never overwritten.

use serde_json::{json, Value};
use std::collections::BTreeSet;
use std::io;
use std::path::Path;

/// Agent files: the name (also the file name) and the text.
pub const AGENTS: &[(&str, &str)] = &[
    ("planner", include_str!("../examples/agents/planner.md")),
    (
        "plan-reviewer",
        include_str!("../examples/agents/plan-reviewer.md"),
    ),
    ("coder", include_str!("../examples/agents/coder.md")),
    ("reviewer", include_str!("../examples/agents/reviewer.md")),
    (
        "pr-cleanup",
        include_str!("../examples/agents/pr-cleanup.md"),
    ),
    (
        "smoketester",
        include_str!("../examples/agents/smoketester.md"),
    ),
];

/// Skills: the folder name and the text of its `SKILL.md`.
pub const SKILLS: &[(&str, &str)] = &[
    (
        "orchestrate",
        include_str!("../examples/skills/orchestrate/SKILL.md"),
    ),
    (
        "run-lessons",
        include_str!("../examples/skills/run-lessons/SKILL.md"),
    ),
    (
        "learn-from-pr",
        include_str!("../examples/skills/learn-from-pr/SKILL.md"),
    ),
];

/// Whether `rel` (a path inside a layer, like `agents/coder.md`) holds exactly the text Helm ships
/// for that example: a copy nobody has changed, which the household's own copy may replace.
pub fn is_shipped(rel: &str, bytes: &[u8]) -> bool {
    AGENTS
        .iter()
        .any(|(n, t)| rel == format!("agents/{n}.md") && t.as_bytes() == bytes)
        || SKILLS
            .iter()
            .any(|(n, t)| rel == format!("skills/{n}/SKILL.md") && t.as_bytes() == bytes)
}

fn record_path(profile: &Path) -> std::path::PathBuf {
    profile.join("examples-offered.json")
}

fn offered(record: &Value, key: &str) -> BTreeSet<String> {
    record
        .get(key)
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

/// Bring the earlier skills and agents folders into the Everyone layer ([`crate::layers::migrate`]),
/// then copy in every example not offered before. Never fails the caller: a problem is logged and
/// the example is tried again next time.
pub fn seed(profile: &Path) {
    crate::layers::migrate(profile);
    if let Err(e) = try_seed(profile) {
        log::warn!("skills: could not offer the example agents and skills: {e}");
    }
}

fn try_seed(profile: &Path) -> io::Result<()> {
    let path = record_path(profile);
    let record: Value = helm_core::privfs::read_capped(&path, 64 * 1024)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_else(|| json!({}));
    let root = crate::layers::Layer::Everyone.root(profile);
    let mut agents = offered(&record, "agents");
    let mut skills = offered(&record, "skills");
    let before = (agents.len(), skills.len());

    for (name, text) in AGENTS {
        if agents.contains(*name) {
            continue;
        }
        let dest = crate::agents::agents_dir(&root).join(format!("{name}.md"));
        if !dest.exists() {
            helm_core::privfs::create_private_dir_all(&crate::agents::agents_dir(&root))?;
            helm_core::privfs::write_atomic(&dest, text.as_bytes())?;
        }
        agents.insert((*name).to_owned());
    }
    for (name, text) in SKILLS {
        if skills.contains(*name) {
            continue;
        }
        let dest = crate::skills_dir(&root).join(name);
        if !dest.exists() {
            helm_core::privfs::create_private_dir_all(&dest)?;
            helm_core::privfs::write_atomic(&dest.join("SKILL.md"), text.as_bytes())?;
        }
        skills.insert((*name).to_owned());
    }

    if (agents.len(), skills.len()) != before {
        let body = json!({ "agents": agents, "skills": skills });
        helm_core::privfs::write_atomic(&path, serde_json::to_vec_pretty(&body)?.as_slice())?;
    }
    Ok(())
}
