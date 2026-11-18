//! The sources a new install starts with, so search works without any setup.
//!
//! On every start the service looks for what this person already has and registers what is not
//! registered yet: their Helm chats, Claude Code's session records, Claude's imported
//! conversations, and every notes vault Obsidian lists. A source the person removed is remembered
//! (in the settings) and never comes back; a vault that appears later is added once, when it is
//! first seen. Everything here is this person's own home: nothing is read from another machine
//! or another user's folders. What is found is the household's by default: the sources share with
//! everyone, and the Helm chats source reads every profile on this computer the hub knows.

use crate::model::{Kind, Scope, Settings, Source};
use crate::store;
use helm_core::db::Database;
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

/// Where to look. A test names its own folders; a debug build with none named seeds nothing, so a
/// scratch profile never registers the developer's real folders.
#[derive(Clone, Debug, Default)]
pub struct SeedPaths {
    /// `<home>/.claude/projects`.
    pub claude_projects: Option<PathBuf>,
    /// Obsidian's `obsidian.json`.
    pub obsidian_config: Option<PathBuf>,
}

/// What a vault folder never contributes to search: retired and backup material, tool folders,
/// drawings, and the study kit folder some vaults carry (matched by name wherever it sits).
const VAULT_EXCLUDE: &[&str] = &[
    "Bible Study Kit",
    "archived",
    "*.bak*",
    "node_modules",
    ".obsidian",
    ".git",
    "Excalidraw",
    "*.excalidraw.md",
];

/// Project folders of Claude Code that are test runs and scratch work.
pub const CLAUDE_CODE_EXCLUDE: &[&str] = &["*helm-live*", "*scratchpad*"];

/// Derived copies Helm keeps beside an imported conversation.
const IMPORT_EXCLUDE: &[&str] = &["*.first-message.md", "*.opening.md"];

fn strings(list: &[&str]) -> Vec<String> {
    list.iter().map(|s| (*s).to_owned()).collect()
}

fn source(id: String, kind: Kind, path: String, exclude: &[&str], at: i64) -> Source {
    Source {
        id,
        kind,
        path,
        person_id: String::new(),
        scope: Scope::Everyone,
        include: Vec::new(),
        exclude: strings(exclude),
        enabled: true,
        created_at: at,
    }
}

/// The vault folders named in Obsidian's registry (`{"vaults": {"<id>": {"path": …}}}`).
fn vaults(config: &Path) -> Vec<PathBuf> {
    let Ok(text) = std::fs::read_to_string(config) else {
        return Vec::new();
    };
    let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) else {
        return Vec::new();
    };
    let mut out: Vec<PathBuf> = v
        .get("vaults")
        .and_then(|m| m.as_object())
        .into_iter()
        .flat_map(|m| m.values())
        .filter_map(|e| e.get("path")?.as_str().map(PathBuf::from))
        .filter(|p| p.is_absolute() && p.is_dir())
        .collect();
    out.sort();
    out.dedup();
    out
}

/// A vault's source id: stable for the folder, so the same vault is never registered twice.
fn vault_id(path: &Path) -> String {
    let h = Sha256::digest(path.to_string_lossy().to_lowercase().as_bytes());
    format!("vault-{}", &format!("{h:x}")[..8])
}

/// Every source this install should have by default, whether or not it is registered.
pub fn defaults(paths: &SeedPaths, at: i64) -> Vec<Source> {
    let mut out = vec![source(
        Kind::HelmChats.as_str().into(),
        Kind::HelmChats,
        String::new(),
        &[],
        at,
    )];
    if let Some(projects) = paths.claude_projects.as_deref().filter(|p| p.is_dir()) {
        out.push(source(
            Kind::ClaudeCode.as_str().into(),
            Kind::ClaudeCode,
            String::new(),
            CLAUDE_CODE_EXCLUDE,
            at + 1,
        ));
        let imports = projects.parent().map(|d| d.join("chat-imports"));
        if let Some(dir) = imports.filter(|d| d.is_dir()) {
            out.push(source(
                "chat-imports".into(),
                Kind::Folder,
                dir.to_string_lossy().into_owned(),
                IMPORT_EXCLUDE,
                at + 2,
            ));
        }
    }
    if let Some(config) = paths.obsidian_config.as_deref() {
        for (n, dir) in vaults(config).into_iter().enumerate() {
            out.push(source(
                vault_id(&dir),
                Kind::Folder,
                dir.to_string_lossy().into_owned(),
                VAULT_EXCLUDE,
                at + 3 + n as i64,
            ));
        }
    }
    out
}

/// A folder path as one comparable string: slashes forward, no verbatim prefix, no trailing
/// slash, case folded (Windows paths), and resolved through the file system when it exists.
fn comparable(path: &str) -> String {
    let resolved = std::fs::canonicalize(path)
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|_| path.to_owned());
    let s = resolved.replace('\\', "/");
    let s = match s.strip_prefix("//?/UNC/") {
        Some(rest) => format!("//{rest}"),
        None => s.strip_prefix("//?/").map(str::to_owned).unwrap_or(s),
    };
    s.trim_end_matches('/').to_lowercase()
}

/// Whether the folder `have` already reads everything in `want`: the same folder, or one above it.
fn covers(have: &str, want: &str) -> bool {
    let (have, want) = (comparable(have), comparable(want));
    !have.is_empty()
        && (want == have
            || want
                .strip_prefix(&have)
                .is_some_and(|rest| rest.starts_with('/')))
}

/// Register the defaults that are missing. Returns how many were added.
///
/// A source someone already set up (however they spelled its path, and whatever it shares) is
/// never registered a second time. On an install that was set up before seeding remembered
/// anything, nothing is added: what is not registered then is taken as removed on purpose.
pub fn seed(db: &Database, paths: &SeedPaths) -> rusqlite::Result<usize> {
    let wanted = defaults(paths, helm_core::now_ms());
    db.write(|tx| {
        let mut settings: Settings = store::load_settings(tx)?;
        let have = store::sources(tx)?;
        let known = |s: &Source| {
            have.iter().any(|h| {
                h.id == s.id
                    || (h.kind == s.kind && s.kind.has_path() && covers(&h.path, &s.path))
                    || (h.kind == Kind::Folder
                        && s.kind == Kind::Folder
                        && (covers(&h.path, &s.path)
                            // A folder inside it that was kept private: the shared vault
                            // above would index it as shared.
                            || (h.scope != Scope::Everyone && covers(&s.path, &h.path))))
            })
        };
        if !settings.seeded {
            let used_before = !have.is_empty() || store::settings_saved(tx)?;
            settings.seeded = true;
            if used_before {
                for s in &wanted {
                    if !known(s) && !settings.removed.contains(&s.id) {
                        settings.removed.push(s.id.clone());
                    }
                }
                let extra = settings.removed.len().saturating_sub(200);
                settings.removed.drain(..extra);
                store::save_settings(tx, &settings)?;
                return Ok(0);
            }
            store::save_settings(tx, &settings)?;
        }
        let mut added = 0;
        for s in wanted {
            if known(&s) || settings.removed.contains(&s.id) {
                continue;
            }
            store::save_source(tx, &s)?;
            added += 1;
        }
        Ok(added)
    })
}

/// Remember that a source was removed, so seeding leaves it alone.
pub fn note_removed(db: &Database, id: &str) -> rusqlite::Result<()> {
    db.write(|tx| {
        let mut s = store::load_settings(tx)?;
        if !s.removed.iter().any(|r| r == id) {
            s.removed.push(id.to_owned());
            let extra = s.removed.len().saturating_sub(200);
            s.removed.drain(..extra);
            store::save_settings(tx, &s)?;
        }
        Ok(())
    })
}
