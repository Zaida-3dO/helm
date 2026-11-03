//! Helm's own skills folder.
//!
//! `<profile>/skills` is a Claude Code **plugin folder**: a launch adds it with `--plugin-dir`, so
//! its skills load in every chat whatever its project — unlike Claude Code's own per-machine
//! skills folder. The folder shape is the plugin shape the CLI reads: a manifest under
//! `.claude-plugin/`, and one folder per skill under `skills/`, each with a `SKILL.md` (a name and
//! description in its front matter, then the instructions). Settings > Skills manages the folder;
//! the chat launch adds the flag in `helm_chats::runtime`.

pub mod ops;

use serde::Serialize;
use std::fs;
use std::io::{self, ErrorKind};
use std::path::{Path, PathBuf};

/// A skill folder holds at most this many bytes in one file.
pub const MAX_SKILL_FILE: u64 = 2 * 1024 * 1024;
/// …and this many in the whole skill.
pub const MAX_SKILL_TOTAL: u64 = 8 * 1024 * 1024;
/// How deep a skill may nest its support folders.
const MAX_DEPTH: usize = 6;
/// The plugin name the manifest declares; the CLI shows its skills as `helm:<skill>`.
pub const PLUGIN_NAME: &str = "helm";

/// The plugin folder inside the profile.
pub fn plugin_dir(profile: &Path) -> PathBuf {
    profile.join("skills")
}

/// Where the skills themselves sit inside the plugin folder.
pub fn skills_dir(profile: &Path) -> PathBuf {
    plugin_dir(profile).join("skills")
}

/// One skill as Settings sees it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SkillEntry {
    pub name: String,
    /// From the skill's front matter; empty when it has none.
    pub description: String,
    /// The skill's own folder, for opening.
    pub path: String,
}

/// A skill's folder name: what the CLI invokes and what add and remove take.
fn valid_name(name: &str) -> bool {
    let b = name.as_bytes();
    !b.is_empty()
        && b.len() <= 64
        && b[0].is_ascii_lowercase()
        && b.iter()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == b'-')
}

pub fn is_valid_name(name: &str) -> bool {
    valid_name(name)
}

/// The skill folder names, lowercase — what a chat launch matches reported skills against.
pub fn skill_names(profile: &Path) -> Vec<String> {
    list(profile).into_iter().map(|s| s.name).collect()
}

/// Every skill in the folder, by name. A missing folder simply means none.
pub fn list(profile: &Path) -> Vec<SkillEntry> {
    let dir = skills_dir(profile);
    let Ok(read) = fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut out: Vec<SkillEntry> = read
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            if !valid_name(&name) || !e.path().join("SKILL.md").is_file() {
                return None;
            }
            Some(SkillEntry {
                name: name.clone(),
                description: front_description(&e.path().join("SKILL.md")),
                path: e.path().to_string_lossy().into_owned(),
            })
        })
        .collect();
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

/// The `description:` line of a SKILL.md front matter block; "" when there is none. Only the head
/// of the file is read: this labels a list, it does not render the skill.
fn front_description(path: &Path) -> String {
    let Ok(head) = helm_core::privfs::read_capped(path, 8 * 1024) else {
        return String::new();
    };
    let text = String::from_utf8_lossy(&head);
    let mut lines = text.lines().skip_while(|l| l.trim().is_empty());
    if lines.next().map(str::trim) != Some("---") {
        return String::new();
    }
    let mut folded = false;
    for line in lines {
        let t = line.trim_end();
        if t == "---" {
            break;
        }
        if let Some(v) = t.strip_prefix("description:") {
            let v = v.trim();
            if v.is_empty() || v.starts_with('>') || v.starts_with('|') {
                folded = true;
                continue;
            }
            return v.trim_matches(|c| c == '"' || c == '\'').to_owned();
        }
        if folded && !t.is_empty() {
            return t.to_owned();
        }
    }
    String::new()
}

fn norm_path(p: &Path) -> String {
    let s = p.to_string_lossy().replace('\\', "/").to_lowercase();
    s.trim_end_matches('/').to_owned()
}

/// Copy `source` (a folder holding a `SKILL.md`) in as a new skill.
pub fn add(profile: &Path, source: &Path) -> io::Result<SkillEntry> {
    let name = source
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    if !valid_name(&name) {
        return Err(io::Error::new(
            ErrorKind::InvalidInput,
            "name must be lower-case letters, digits and hyphens",
        ));
    }
    // A copy inside the plugin root would recurse or duplicate; the picked folder cannot be in it.
    if norm_path(source).starts_with(&norm_path(&plugin_dir(profile))) {
        return Err(io::Error::new(
            ErrorKind::InvalidInput,
            "pick a folder outside the skills folder",
        ));
    }
    find_skill_md(source)?;
    let dest = skills_dir(profile).join(&name);
    if dest.exists() {
        return Err(io::Error::new(
            ErrorKind::AlreadyExists,
            "a skill of that name is already in the folder",
        ));
    }
    helm_core::privfs::create_private_dir_all(&dest)?;
    copy_tree(source, &dest, 0)?;
    Ok(SkillEntry {
        name: name.clone(),
        description: front_description(&dest.join("SKILL.md")),
        path: dest.to_string_lossy().into_owned(),
    })
}

/// `SKILL.md` inside `dir`, checked case-insensitively the way Windows would open it.
fn find_skill_md(dir: &Path) -> io::Result<PathBuf> {
    let exact = dir.join("SKILL.md");
    if exact.is_file() {
        return Ok(exact);
    }
    for e in fs::read_dir(dir)? {
        let e = e?;
        if e.file_name()
            .to_string_lossy()
            .eq_ignore_ascii_case("SKILL.md")
            && e.path().is_file()
        {
            return Ok(e.path());
        }
    }
    Err(io::Error::new(
        ErrorKind::NotFound,
        "the folder has no SKILL.md",
    ))
}

fn copy_tree(src: &Path, dest: &Path, depth: usize) -> io::Result<u64> {
    if depth > MAX_DEPTH {
        return Err(io::Error::new(
            ErrorKind::InvalidInput,
            "the skill nests too deeply",
        ));
    }
    let mut total = 0u64;
    for e in fs::read_dir(src)? {
        let e = e?;
        let from = e.path();
        let ty = e.file_type()?;
        if ty.is_symlink() {
            return Err(io::Error::new(
                ErrorKind::InvalidInput,
                "the skill contains a link",
            ));
        }
        let to = dest.join(e.file_name());
        if ty.is_dir() {
            fs::create_dir_all(&to)?;
            total += copy_tree(&from, &to, depth + 1)?;
        } else {
            let len = e.metadata()?.len();
            if len > MAX_SKILL_FILE {
                return Err(io::Error::new(
                    ErrorKind::InvalidInput,
                    "a file in the skill is too large",
                ));
            }
            total += len;
            if total > MAX_SKILL_TOTAL {
                return Err(io::Error::new(
                    ErrorKind::InvalidInput,
                    "the skill is too large",
                ));
            }
            fs::copy(&from, &to)?;
        }
    }
    Ok(total)
}

/// Remove one skill. Only the folder's own entries can go, and never through a link.
pub fn remove(profile: &Path, name: &str) -> io::Result<()> {
    if !valid_name(name) {
        return Err(io::Error::new(ErrorKind::InvalidInput, "not a skill name"));
    }
    let path = skills_dir(profile).join(name);
    match fs::symlink_metadata(&path) {
        Err(e) if e.kind() == ErrorKind::NotFound => {
            return Err(io::Error::new(ErrorKind::NotFound, "no such skill"))
        }
        Err(e) => return Err(e),
        Ok(m) if m.file_type().is_symlink() || !m.is_dir() => {
            return Err(io::Error::new(
                ErrorKind::InvalidInput,
                "not a skill folder",
            ))
        }
        Ok(_) => {}
    }
    fs::remove_dir_all(&path)
}

/// Write the manifest if the plugin folder has none; a user's edit is left alone.
fn ensure_manifest(plugin: &Path) -> io::Result<()> {
    let manifest = plugin.join(".claude-plugin").join("plugin.json");
    if manifest.exists() {
        return Ok(());
    }
    helm_core::privfs::create_private_dir_all(manifest.parent().unwrap())?;
    let body = serde_json::json!({
        "name": PLUGIN_NAME,
        "description": "Skills Helm loads into every chat.",
        "version": "1.0.0",
    });
    helm_core::privfs::write_atomic(
        &manifest,
        serde_json::to_vec_pretty(&body).unwrap().as_slice(),
    )
}

/// The folder to pass as `--plugin-dir`, or None when the plugin has nothing to load.
pub fn launch_plugin_dir(profile: &Path) -> Option<String> {
    if list(profile).is_empty() {
        return None;
    }
    let plugin = plugin_dir(profile);
    if let Err(e) = ensure_manifest(&plugin) {
        log::warn!("skills: could not write the plugin manifest: {e}");
        return None;
    }
    Some(plugin.to_string_lossy().into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(path: &Path, body: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, body).unwrap();
    }

    fn source_skill(dir: &Path, name: &str, description: &str) -> PathBuf {
        let s = dir.join("picked").join(name);
        write(
            &s.join("SKILL.md"),
            &format!("---\nname: {name}\ndescription: {description}\n---\nDo the thing.\n"),
        );
        write(&s.join("scripts/run.sh"), "echo hi\n");
        s
    }

    #[test]
    fn add_list_and_remove_a_skill() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path();
        assert!(list(p).is_empty());
        assert_eq!(launch_plugin_dir(p), None, "nothing to load yet");

        let src = tmp.path().join("outside/claude-in-chrome-runs");
        write(
            &src.join("SKILL.md"),
            "---\ndescription: Drive Chrome runs\n---\nbody\n",
        );
        write(&src.join("templates/a.md"), "tpl\n");
        let e = add(p, &src).unwrap();
        assert_eq!(e.name, "claude-in-chrome-runs");
        assert_eq!(e.description, "Drive Chrome runs");
        assert!(p
            .join("skills/skills/claude-in-chrome-runs/templates/a.md")
            .is_file());
        assert_eq!(list(p), vec![e.clone()]);
        assert_eq!(skill_names(p), ["claude-in-chrome-runs"]);

        let dir = launch_plugin_dir(p).unwrap();
        assert_eq!(dir, p.join("skills").to_string_lossy());
        let manifest: serde_json::Value =
            serde_json::from_slice(&fs::read(p.join("skills/.claude-plugin/plugin.json")).unwrap())
                .unwrap();
        assert_eq!(manifest["name"], PLUGIN_NAME);

        add(p, &source_skill(tmp.path(), "review", "Review a diff")).unwrap();
        assert_eq!(list(p).len(), 2, "the list sorts by name");

        remove(p, "claude-in-chrome-runs").unwrap();
        assert_eq!(skill_names(p), ["review"]);
        assert_eq!(remove(p, "gone").unwrap_err().kind(), ErrorKind::NotFound);
    }

    #[test]
    fn bad_adds_are_refused_and_change_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path();
        let s = tmp.path().join("src/Skill Two");
        write(&s.join("SKILL.md"), "no front matter");
        assert_eq!(add(p, &s).unwrap_err().kind(), ErrorKind::InvalidInput);
        assert!(list(p).is_empty());

        // No SKILL.md.
        let s2 = tmp.path().join("src2/plain");
        fs::create_dir_all(&s2).unwrap();
        assert_eq!(add(p, &s2).unwrap_err().kind(), ErrorKind::NotFound);

        // A duplicate.
        let good = source_skill(tmp.path(), "twice", "x");
        add(p, &good).unwrap();
        assert_eq!(add(p, &good).unwrap_err().kind(), ErrorKind::AlreadyExists);

        // The skills folder itself, or a skill already inside it, cannot be picked.
        assert_eq!(
            add(p, &p.join("skills")).unwrap_err().kind(),
            ErrorKind::InvalidInput
        );
        assert_eq!(
            add(p, &p.join("skills/skills/twice")).unwrap_err().kind(),
            ErrorKind::InvalidInput
        );

        // One oversized file is refused.
        let big = tmp.path().join("bigskill");
        write(&big.join("SKILL.md"), "---\n---\n");
        write(&big.join("blob"), &"x".repeat(MAX_SKILL_FILE as usize + 10));
        assert_eq!(add(p, &big).unwrap_err().kind(), ErrorKind::InvalidInput);

        assert_eq!(skill_names(p), ["twice"]);
        assert!(!is_valid_name("UPPER"), "names stay lower-case");
        assert!(!is_valid_name("-lead"), "a name cannot start with a hyphen");
        assert!(!is_valid_name("with space"), "no spaces");
        assert!(is_valid_name("a1-b2-c3"));
    }

    #[test]
    fn a_hands_written_manifest_survives() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path();
        write(
            &p.join("skills/.claude-plugin/plugin.json"),
            r#"{"name":"mine","version":"9"}"#,
        );
        add(p, &source_skill(tmp.path(), "keep", "k")).unwrap();
        assert!(launch_plugin_dir(p).is_some());
        assert!(
            fs::read_to_string(p.join("skills/.claude-plugin/plugin.json"))
                .unwrap()
                .contains("\"mine\"")
        );
    }
}
