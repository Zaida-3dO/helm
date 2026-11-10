//! The knowledge layers and where they live in the profile folder.
//!
//! ```text
//! <profile>/knowledge/everyone/{rules/*.md, skills/<name>/SKILL.md…, agents/*.md}
//! <profile>/knowledge/people/<profileId>/{rules, skills, agents}
//! ```
//!
//! A layer is named `everyone` or `profile:<profileId>` wherever it crosses an op or a file.
//! Folders written by earlier versions (`<profile>/skills/skills`, `<profile>/agents`) are copied
//! into the Everyone layer once ([`migrate`]); the originals are left where they are and no
//! longer read.

use crate::{agents, install_dir, is_valid_name};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// The folder holding every layer, inside the profile folder.
pub const FOLDER: &str = "knowledge";

/// Written once the earlier folders have been copied in.
const MIGRATED: &str = ".migrated";

/// Which knowledge layer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Layer {
    /// Applies to everyone who uses this Helm.
    Everyone,
    /// One profile's own layer.
    Profile(String),
}

impl Layer {
    /// `everyone` or `profile:<profileId>`; anything else is `None`.
    pub fn parse(s: &str) -> Option<Layer> {
        if s == "everyone" {
            return Some(Layer::Everyone);
        }
        let id = s.strip_prefix("profile:")?;
        valid_profile_id(id).then(|| Layer::Profile(id.to_owned()))
    }

    /// The name that crosses ops and files.
    pub fn key(&self) -> String {
        match self {
            Layer::Everyone => "everyone".to_owned(),
            Layer::Profile(id) => format!("profile:{id}"),
        }
    }

    /// The layer's root folder (the one holding `rules/`, `skills/` and `agents/`).
    pub fn root(&self, profile_dir: &Path) -> PathBuf {
        let base = knowledge_dir(profile_dir);
        match self {
            Layer::Everyone => base.join("everyone"),
            Layer::Profile(id) => base.join("people").join(id),
        }
    }
}

/// A profile id as it may name a folder: letters, digits, `-` and `_`.
pub fn valid_profile_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_')
}

/// The kinds of thing a layer holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Rule,
    Skill,
    Agent,
}

impl Kind {
    pub fn parse(s: &str) -> Option<Kind> {
        match s {
            "rule" => Some(Kind::Rule),
            "skill" => Some(Kind::Skill),
            "agent" => Some(Kind::Agent),
            _ => None,
        }
    }

    pub fn key(self) -> &'static str {
        match self {
            Kind::Rule => "rule",
            Kind::Skill => "skill",
            Kind::Agent => "agent",
        }
    }

    /// The folder inside a layer's root.
    pub fn folder(self) -> &'static str {
        match self {
            Kind::Rule => "rules",
            Kind::Skill => "skills",
            Kind::Agent => "agents",
        }
    }
}

pub fn knowledge_dir(profile_dir: &Path) -> PathBuf {
    profile_dir.join(FOLDER)
}

/// Serialises the one-time copy, so two launches starting together do not both run it.
static MIGRATION: Mutex<()> = Mutex::new(());

/// Copy the skills and agents earlier versions kept straight in the profile folder into the
/// Everyone layer, once. An item already there is left alone; one that cannot be copied is logged
/// and skipped. Never fails the caller.
pub fn migrate(profile_dir: &Path) {
    let _guard = MIGRATION.lock().unwrap_or_else(|p| p.into_inner());
    let marker = knowledge_dir(profile_dir).join(MIGRATED);
    if marker.exists() {
        return;
    }
    if let Err(e) = try_migrate(profile_dir, &marker) {
        log::warn!("knowledge: could not copy the earlier skills and agents: {e}");
    }
}

fn try_migrate(profile_dir: &Path, marker: &Path) -> io::Result<()> {
    let root = Layer::Everyone.root(profile_dir);
    helm_core::privfs::create_private_dir_all(&root)?;

    // Skills: <profile>/skills/skills/<name>/ (the folder was a plugin).
    if let Ok(read) = fs::read_dir(profile_dir.join("skills").join("skills")) {
        for e in read.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            if !is_valid_name(&name) || !e.path().join("SKILL.md").is_file() {
                continue;
            }
            let dest = crate::skills_dir(&root).join(&name);
            if dest.exists() {
                continue;
            }
            if let Err(err) = install_dir(&e.path(), &dest) {
                log::warn!("knowledge: skill {name} was not copied: {err}");
            }
        }
    }

    // Agents: <profile>/agents/<name>.md.
    if let Ok(read) = fs::read_dir(profile_dir.join("agents")) {
        for e in read.flatten() {
            let file = e.file_name().to_string_lossy().into_owned();
            let Some(name) = file.strip_suffix(".md") else {
                continue;
            };
            if !is_valid_name(name) || !e.path().is_file() {
                continue;
            }
            let dest = agents::agents_dir(&root).join(&file);
            if dest.exists() {
                continue;
            }
            let copied = helm_core::privfs::read_capped(&e.path(), agents::MAX_AGENT_FILE)
                .and_then(|bytes| {
                    helm_core::privfs::create_private_dir_all(&agents::agents_dir(&root))?;
                    helm_core::privfs::write_atomic(&dest, &bytes)
                });
            if let Err(err) = copied {
                log::warn!("knowledge: agent {name} was not copied: {err}");
            }
        }
    }

    helm_core::privfs::write_atomic(marker, b"1\n")
}
