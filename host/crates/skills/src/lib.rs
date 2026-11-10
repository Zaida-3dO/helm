//! Helm's own skills folder.
//!
//! `<profile>/skills` is a Claude Code **plugin folder**: a launch adds it with `--plugin-dir`, so
//! its skills load in every chat whatever its project — unlike Claude Code's own per-machine
//! skills folder. The folder shape is the plugin shape the CLI reads: a manifest under
//! `.claude-plugin/`, and one folder per skill under `skills/`, each with a `SKILL.md` (a name and
//! description in its front matter, then the instructions). Settings > Skills manages the folder;
//! the chat launch adds the flag in `helm_chats::runtime`. Agents have their own folder beside it
//! (see [`agents`]), and the example agents and skills Helm offers are in [`examples`].

pub mod agents;
pub mod examples;
pub mod ops;
pub use ops::register_ops;

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
    if let Err(e) = copy_tree(source, &dest, 0) {
        // Do not leave a half-copied skill behind: it would be listed, and a retry would be told
        // the name is taken.
        let _ = fs::remove_dir_all(&dest);
        return Err(e);
    }
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

        // One oversized file is refused, even one that sorts after SKILL.md, and leaves no folder.
        let big = tmp.path().join("bigskill");
        write(&big.join("SKILL.md"), "---\n---\n");
        write(
            &big.join("zdata"),
            &"x".repeat(MAX_SKILL_FILE as usize + 10),
        );
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

    #[test]
    fn examples_are_offered_once_and_every_stage_agent_ships() {
        use crate::{agents, examples};
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path();

        // Every agent a workflow stage defaults to is shipped, and each example is a valid file
        // whose front matter name is its file name.
        for stage in [
            "planner",
            "plan-reviewer",
            "coder",
            "reviewer",
            "pr-cleanup",
            "smoketester",
        ] {
            assert!(examples::AGENTS.iter().any(|(n, _)| *n == stage), "{stage}");
        }
        for (name, text) in examples::AGENTS {
            let a =
                agents::parse_agent(text).unwrap_or_else(|| panic!("{name} has no front matter"));
            assert_eq!(a.name, *name);
            assert!(!a.description.is_empty() && !a.prompt.is_empty(), "{name}");
        }
        for (name, text) in examples::SKILLS {
            assert!(text.contains(&format!("name: {name}\n")), "{name}");
        }

        examples::seed(p);
        assert_eq!(agents::list(p).len(), examples::AGENTS.len());
        assert_eq!(list(p).len(), examples::SKILLS.len());
        assert!(
            launch_plugin_dir(p).is_some(),
            "the example skills load into chats"
        );

        // A removed example stays removed; an edited one is left alone.
        agents::remove(p, "coder").unwrap();
        remove(p, "orchestrate").unwrap();
        let edited = p.join("agents/planner.md");
        fs::write(
            &edited,
            "---\nname: planner\ndescription: mine\n---\nmy plan\n",
        )
        .unwrap();
        examples::seed(p);
        assert!(!agents::agent_names(p).contains(&"coder".to_owned()));
        assert!(!skill_names(p).contains(&"orchestrate".to_owned()));
        assert!(fs::read_to_string(&edited).unwrap().contains("my plan"));

        // A file already there when an example is first offered is not overwritten.
        let tmp2 = tempfile::tempdir().unwrap();
        write(
            &tmp2.path().join("agents/reviewer.md"),
            "---\nname: reviewer\ndescription: house\n---\nhouse rules\n",
        );
        examples::seed(tmp2.path());
        assert_eq!(
            agents::list(tmp2.path())
                .iter()
                .find(|a| a.name == "reviewer")
                .unwrap()
                .description,
            "house"
        );
    }

    #[test]
    fn agents_parse_add_remove_and_reach_the_launch_file() {
        use crate::agents;
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path();
        assert_eq!(agents::launch_agents_file(p), None, "no agents, no file");

        // Front matter shapes: list or comma tools, comments, quotes, model aliases only.
        let a = agents::parse_agent(
            "\n---\n# a comment\nname: \"scout\"\ndescription: Looks around\ntools: [Read, \"Grep\"]\nmodel: OPUS\n---\n\nBody line\n",
        )
        .unwrap();
        assert_eq!(
            (a.name.as_str(), a.tools.clone()),
            ("scout", vec!["Read".to_owned(), "Grep".to_owned()])
        );
        assert_eq!(a.prompt, "Body line");
        assert_eq!(agents::parse_agent("no front matter"), None);
        assert_eq!(agents::parse_agent("---\nname: x\nnever closed"), None);

        let src = |name: &str, body: &str| {
            let f = tmp.path().join("picked").join(name);
            write(&f, body);
            f
        };
        let scout = src("scout.md", "---\nname: scout\ndescription: Looks around\ntools: Read, Grep\nmodel: OPUS\n---\nLook.\n");
        let added = agents::add(p, &scout).unwrap();
        assert_eq!(added.names, ["scout"]);
        let odd = src(
            "odd.md",
            "---\ndescription: Odd one\nmodel: gpt-5\n---\nDo odd things.\n",
        );
        assert_eq!(
            agents::add(p, &odd).unwrap().names,
            ["odd"],
            "the file name is the fallback"
        );

        // A folder takes each agent file in it and reports the ones it left out.
        write(
            &tmp.path().join("bundle/extra.md"),
            "---
description: From a folder
---
Work.
",
        );
        write(&tmp.path().join("bundle/notes.md"), "just notes");
        write(&tmp.path().join("bundle/readme.txt"), "ignored");
        let bundle = agents::add(p, &tmp.path().join("bundle")).unwrap();
        assert_eq!(bundle.names, ["extra"]);
        assert_eq!(bundle.skipped.len(), 1);
        assert!(bundle.skipped[0].starts_with("notes.md"));
        let empty = tmp.path().join("empty");
        fs::create_dir_all(&empty).unwrap();
        assert_eq!(
            agents::add(p, &empty).unwrap_err().kind(),
            ErrorKind::InvalidInput
        );
        agents::remove(p, "extra").unwrap();

        let doc = agents::launch_json(p);
        assert_eq!(doc["scout"]["prompt"], "Look.");
        assert_eq!(doc["scout"]["tools"], serde_json::json!(["Read", "Grep"]));
        assert_eq!(doc["scout"]["model"], "opus");
        assert!(doc["odd"].get("model").is_none() && doc["odd"].get("tools").is_none());
        let file = agents::launch_agents_file(p).unwrap();
        assert_eq!(
            fs::read_to_string(file).unwrap(),
            serde_json::to_string(&doc).unwrap()
        );

        // Refusals change nothing.
        for (body, kind) in [
            (
                "---\nname: scout\ndescription: dup\n---\nx\n",
                ErrorKind::AlreadyExists,
            ),
            (
                "---\nname: Bad Name\ndescription: d\n---\nx\n",
                ErrorKind::InvalidInput,
            ),
            ("---\nname: nodesc\n---\nx\n", ErrorKind::InvalidInput),
            (
                "---\nname: nobody\ndescription: d\n---\n",
                ErrorKind::InvalidInput,
            ),
            ("plain text", ErrorKind::InvalidInput),
        ] {
            let f = src("try.md", body);
            assert_eq!(agents::add(p, &f).unwrap_err().kind(), kind, "{body}");
        }
        assert_eq!(
            agents::add(p, &tmp.path().join("missing.md"))
                .unwrap_err()
                .kind(),
            ErrorKind::NotFound
        );
        assert_eq!(agents::agent_names(p), ["odd", "scout"]);

        agents::remove(p, "scout").unwrap();
        assert_eq!(
            agents::remove(p, "scout").unwrap_err().kind(),
            ErrorKind::NotFound
        );
        assert_eq!(
            agents::remove(p, "../x").unwrap_err().kind(),
            ErrorKind::InvalidInput
        );
        assert_eq!(agents::agent_names(p), ["odd"]);
    }
}
