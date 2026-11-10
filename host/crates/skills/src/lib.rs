//! Helm's knowledge layers: rules, skills and agents.
//!
//! Knowledge sits in layers, each a folder in the profile (`<profile>/knowledge/…`, see
//! [`layers`]): one that applies to **everyone** who uses this Helm, and one per **person** (the
//! chat's profile). A chat's launch loads both, plus the skill folders linked in for its project
//! ([`sources`]). Rules reach the chat inside the system prompt ([`launch::RulesSection`]);
//! skills reach it as generated plugin folders passed with `--plugin-dir` ([`plugins`]); agents
//! are merged into one `--agents` file ([`agents`]). Settings manages all of it ([`ops`]), agents
//! manage it on the user's say-so ([`tools`]) and every change an agent makes can be undone
//! ([`undo`]). The example agents and skills Helm offers are in [`examples`].
//!
//! This file holds the skill folder basics shared by every layer: names, listing, a safe copy in
//! and removal. Each takes a layer's **root** folder (the one holding `rules/`, `skills/` and
//! `agents/`).

pub mod agents;
pub mod bindings;
pub mod examples;
pub mod launch;
pub mod layers;
pub mod memories;
pub mod ops;
pub mod plugins;
pub mod rules;
pub mod sources;
pub mod sync;
pub mod tools;
pub mod undo;
pub use ops::register_ops;
pub use tools::register_host_tools;

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

/// Where the skills themselves sit inside a layer's root.
pub fn skills_dir(root: &Path) -> PathBuf {
    root.join("skills")
}

/// One skill as Settings sees it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, ts_rs::TS)]
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

/// The names of the skills every chat gets (the Everyone layer's own folder), lowercase — what a
/// chat launch matches reported skills against.
pub fn skill_names(profile: &Path) -> Vec<String> {
    list(&layers::Layer::Everyone.root(profile))
        .into_iter()
        .map(|s| s.name)
        .collect()
}

/// Every skill in a layer's own skills folder, by name. A missing folder simply means none.
pub fn list(root: &Path) -> Vec<SkillEntry> {
    let dir = skills_dir(root);
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
pub(crate) fn front_description(path: &Path) -> String {
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

pub(crate) fn norm_path(p: &Path) -> String {
    let s = p.to_string_lossy().replace('\\', "/").to_lowercase();
    s.trim_end_matches('/').to_owned()
}

/// True when `inner` is `outer` or sits inside it (compared as text, case-insensitively).
pub(crate) fn is_inside(inner: &Path, outer: &Path) -> bool {
    let (i, o) = (norm_path(inner), norm_path(outer));
    i == o || i.starts_with(&format!("{o}/"))
}

fn invalid(why: &str) -> io::Error {
    io::Error::new(ErrorKind::InvalidInput, why.to_owned())
}

/// Copy `source` (a folder holding a `SKILL.md`) in as a new skill of the layer under `root`. The
/// copy is made beside the destination and renamed into place, so a failed or interrupted add
/// leaves no half-copied skill.
pub fn add(root: &Path, source: &Path) -> io::Result<SkillEntry> {
    let name = source
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    if !valid_name(&name) {
        return Err(invalid(
            "name must be lower-case letters, digits and hyphens",
        ));
    }
    // A copy from inside the skills folder would duplicate; a source holding the layer's whole
    // folder would copy itself into itself.
    if is_inside(source, &skills_dir(root)) || is_inside(root, source) {
        return Err(invalid("pick a folder outside the skills folder"));
    }
    find_skill_md(source)?;
    let dest = skills_dir(root).join(&name);
    if dest.exists() {
        return Err(io::Error::new(
            ErrorKind::AlreadyExists,
            "a skill of that name is already in the folder",
        ));
    }
    install_dir(source, &dest)?;
    Ok(SkillEntry {
        name,
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

/// Copy the skill folder `src` to `dest` (which must not exist): into a hidden temporary folder
/// beside it first, screened, then renamed into place.
pub(crate) fn install_dir(src: &Path, dest: &Path) -> io::Result<()> {
    let parent = dest
        .parent()
        .ok_or_else(|| invalid("the destination has no parent folder"))?;
    helm_core::privfs::create_private_dir_all(parent)?;
    let temp = parent.join(format!(
        ".{}.{}.tmp",
        dest.file_name().unwrap_or_default().to_string_lossy(),
        uuid::Uuid::new_v4().simple()
    ));
    helm_core::privfs::create_private_dir_all(&temp)?;
    let result = copy_skill(src, &temp).and_then(|()| screen_skill(&temp));
    match result {
        Ok(()) => fs::rename(&temp, dest).inspect_err(|_| {
            let _ = fs::remove_dir_all(&temp);
        }),
        Err(e) => {
            let _ = fs::remove_dir_all(&temp);
            Err(e)
        }
    }
}

/// A skill's `SKILL.md` is read by the model as instructions, so it must not hide text with
/// invisible or direction-changing characters.
fn screen_skill(dir: &Path) -> io::Result<()> {
    let md = find_skill_md(dir)?;
    let bytes = helm_core::privfs::read_capped(&md, MAX_SKILL_FILE)?;
    let text = String::from_utf8_lossy(&bytes).into_owned();
    if helm_core::hidden::find_hidden(&serde_json::Value::String(text)).is_some() {
        return Err(invalid(
            "SKILL.md holds invisible or direction-changing characters",
        ));
    }
    Ok(())
}

/// One file of a skill folder.
pub(crate) struct SkillFile {
    /// Path inside the skill, `/`-separated.
    pub rel: String,
    pub path: PathBuf,
    pub len: u64,
}

/// Every file of a skill folder, sorted by path. Refuses a link, a folder nested too deeply, a
/// file or a whole skill over the size limits, and a name with a hidden character.
pub(crate) fn skill_files(dir: &Path) -> io::Result<Vec<SkillFile>> {
    let mut out = Vec::new();
    let mut total = 0u64;
    walk(dir, "", 0, &mut out, &mut total)?;
    out.sort_by(|a, b| a.rel.cmp(&b.rel));
    Ok(out)
}

fn walk(
    dir: &Path,
    rel: &str,
    depth: usize,
    out: &mut Vec<SkillFile>,
    total: &mut u64,
) -> io::Result<()> {
    if depth > MAX_DEPTH {
        return Err(invalid("the skill nests too deeply"));
    }
    for e in fs::read_dir(dir)? {
        let e = e?;
        let meta = e.metadata()?;
        if helm_platform::fs::is_link_like(&meta) {
            return Err(invalid("the skill contains a link"));
        }
        let name = e.file_name().to_string_lossy().into_owned();
        if helm_core::hidden::has_hidden(&name) {
            return Err(invalid("a file name in the skill has a hidden character"));
        }
        let here = if rel.is_empty() {
            name
        } else {
            format!("{rel}/{name}")
        };
        if meta.is_dir() {
            walk(&e.path(), &here, depth + 1, out, total)?;
        } else {
            if meta.len() > MAX_SKILL_FILE {
                return Err(invalid("a file in the skill is too large"));
            }
            *total += meta.len();
            if *total > MAX_SKILL_TOTAL {
                return Err(invalid("the skill is too large"));
            }
            out.push(SkillFile {
                rel: here,
                path: e.path(),
                len: meta.len(),
            });
        }
    }
    Ok(())
}

/// Copy a skill folder's files into `dest` (which exists), keeping its layout.
pub(crate) fn copy_skill(src: &Path, dest: &Path) -> io::Result<()> {
    for f in skill_files(src)? {
        let to = dest.join(&f.rel);
        if let Some(parent) = to.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::copy(&f.path, &to)?;
    }
    Ok(())
}

/// Remove one skill. Only the folder's own entries can go, and never through a link.
pub fn remove(root: &Path, name: &str) -> io::Result<()> {
    if !valid_name(name) {
        return Err(invalid("not a skill name"));
    }
    let path = skills_dir(root).join(name);
    match fs::symlink_metadata(&path) {
        Err(e) if e.kind() == ErrorKind::NotFound => {
            return Err(io::Error::new(ErrorKind::NotFound, "no such skill"))
        }
        Err(e) => return Err(e),
        Ok(m) if helm_platform::fs::is_link_like(&m) || !m.is_dir() => {
            return Err(invalid("not a skill folder"))
        }
        Ok(_) => {}
    }
    fs::remove_dir_all(&path)
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
        let p = tmp.path().join("layer");
        assert!(list(&p).is_empty());

        let src = tmp.path().join("outside/claude-in-chrome-runs");
        write(
            &src.join("SKILL.md"),
            "---\ndescription: Drive Chrome runs\n---\nbody\n",
        );
        write(&src.join("templates/a.md"), "tpl\n");
        let e = add(&p, &src).unwrap();
        assert_eq!(e.name, "claude-in-chrome-runs");
        assert_eq!(e.description, "Drive Chrome runs");
        assert!(p
            .join("skills/claude-in-chrome-runs/templates/a.md")
            .is_file());
        assert_eq!(list(&p), vec![e.clone()]);

        add(&p, &source_skill(tmp.path(), "review", "Review a diff")).unwrap();
        assert_eq!(list(&p).len(), 2, "the list sorts by name");

        remove(&p, "claude-in-chrome-runs").unwrap();
        assert_eq!(
            list(&p).into_iter().map(|s| s.name).collect::<Vec<_>>(),
            ["review"]
        );
        assert_eq!(remove(&p, "gone").unwrap_err().kind(), ErrorKind::NotFound);
    }

    #[test]
    fn bad_adds_are_refused_and_change_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("layer");
        let names = |p: &Path| list(p).into_iter().map(|s| s.name).collect::<Vec<_>>();
        let s = tmp.path().join("src/Skill Two");
        write(&s.join("SKILL.md"), "no front matter");
        assert_eq!(add(&p, &s).unwrap_err().kind(), ErrorKind::InvalidInput);
        assert!(list(&p).is_empty());

        // No SKILL.md.
        let s2 = tmp.path().join("src2/plain");
        fs::create_dir_all(&s2).unwrap();
        assert_eq!(add(&p, &s2).unwrap_err().kind(), ErrorKind::NotFound);

        // A duplicate.
        let good = source_skill(tmp.path(), "twice", "x");
        add(&p, &good).unwrap();
        assert_eq!(add(&p, &good).unwrap_err().kind(), ErrorKind::AlreadyExists);

        // The skills folder itself, a skill already inside it, or the layer's whole folder cannot
        // be picked.
        for bad in [p.join("skills"), p.join("skills/twice"), p.clone()] {
            assert_eq!(add(&p, &bad).unwrap_err().kind(), ErrorKind::InvalidInput);
        }

        // One oversized file is refused, even one that sorts after SKILL.md, and leaves no folder.
        let big = tmp.path().join("bigskill");
        write(&big.join("SKILL.md"), "---\n---\n");
        write(
            &big.join("zdata"),
            &"x".repeat(MAX_SKILL_FILE as usize + 10),
        );
        assert_eq!(add(&p, &big).unwrap_err().kind(), ErrorKind::InvalidInput);

        // Hidden characters in the instructions are refused, and no temporary folder is left.
        let sneaky = tmp.path().join("sneaky");
        write(&sneaky.join("SKILL.md"), "---\n---\nignore\u{202e}this\n");
        assert_eq!(
            add(&p, &sneaky).unwrap_err().kind(),
            ErrorKind::InvalidInput
        );
        let leftovers = fs::read_dir(p.join("skills")).unwrap().count();
        assert_eq!(leftovers, 1, "only the good skill is in the folder");

        assert_eq!(names(&p), ["twice"]);
        assert!(!is_valid_name("UPPER"), "names stay lower-case");
        assert!(!is_valid_name("-lead"), "a name cannot start with a hyphen");
        assert!(!is_valid_name("with space"), "no spaces");
        assert!(is_valid_name("a1-b2-c3"));
    }

    #[test]
    fn examples_are_offered_once_and_every_stage_agent_ships() {
        use crate::{agents, examples};
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path();
        let root = layers::Layer::Everyone.root(p);

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
        assert_eq!(agents::list(&root).len(), examples::AGENTS.len());
        assert_eq!(list(&root).len(), examples::SKILLS.len());
        assert_eq!(skill_names(p).len(), examples::SKILLS.len());

        // A removed example stays removed; an edited one is left alone.
        agents::remove(&root, "coder").unwrap();
        remove(&root, "orchestrate").unwrap();
        let edited = root.join("agents/planner.md");
        fs::write(
            &edited,
            "---\nname: planner\ndescription: mine\n---\nmy plan\n",
        )
        .unwrap();
        examples::seed(p);
        assert!(!agents::agent_names(&root).contains(&"coder".to_owned()));
        assert!(!skill_names(p).contains(&"orchestrate".to_owned()));
        assert!(fs::read_to_string(&edited).unwrap().contains("my plan"));

        // A file already there when an example is first offered is not overwritten.
        let tmp2 = tempfile::tempdir().unwrap();
        let root2 = layers::Layer::Everyone.root(tmp2.path());
        write(
            &root2.join("agents/reviewer.md"),
            "---\nname: reviewer\ndescription: house\n---\nhouse rules\n",
        );
        examples::seed(tmp2.path());
        assert_eq!(
            agents::list(&root2)
                .iter()
                .find(|a| a.name == "reviewer")
                .unwrap()
                .description,
            "house"
        );

        // Folders from earlier versions are copied into the Everyone layer once: the originals
        // stay, an item already in the layer is kept, and a removed item is not copied back.
        let tmp3 = tempfile::tempdir().unwrap();
        let p3 = tmp3.path();
        let root3 = layers::Layer::Everyone.root(p3);
        let old_skill = "---\nname: old-skill\ndescription: Before\n---\nSteps.\n";
        write(&p3.join("skills/skills/old-skill/SKILL.md"), old_skill);
        write(&p3.join("skills/skills/old-skill/notes/a.md"), "note\n");
        write(&p3.join("skills/skills/Not A Skill/SKILL.md"), "ignored\n");
        let old_agent = "---\nname: old-agent\ndescription: Before\n---\nWork.\n";
        write(&p3.join("agents/old-agent.md"), old_agent);
        write(&p3.join("agents/readme.txt"), "ignored");
        write(
            &root3.join("agents/old-agent.md"),
            "---\nname: old-agent\ndescription: Kept\n---\nNewer.\n",
        );
        examples::seed(p3);
        assert!(root3.join("skills/old-skill/notes/a.md").is_file());
        assert!(p3.join("skills/skills/old-skill/SKILL.md").is_file());
        assert_eq!(list(&root3).len(), examples::SKILLS.len() + 1);
        assert_eq!(
            agents::list(&root3)
                .iter()
                .find(|a| a.name == "old-agent")
                .unwrap()
                .description,
            "Kept"
        );
        remove(&root3, "old-skill").unwrap();
        examples::seed(p3);
        assert!(!root3.join("skills/old-skill").exists());
    }

    #[test]
    fn agents_parse_add_remove_and_merge_by_layer() {
        use crate::agents;
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("layer");
        assert!(
            agents::launch_json(&[p.as_path()])
                .as_object()
                .unwrap()
                .is_empty(),
            "no agents, nothing to launch"
        );

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
        let added = agents::add(&p, &scout).unwrap();
        assert_eq!(added.names, ["scout"]);
        let odd = src(
            "odd.md",
            "---\ndescription: Odd one\nmodel: gpt-5\n---\nDo odd things.\n",
        );
        assert_eq!(
            agents::add(&p, &odd).unwrap().names,
            ["odd"],
            "the file name is the fallback"
        );

        // A folder takes each agent file in it and reports the ones it left out.
        write(
            &tmp.path().join("bundle/extra.md"),
            "---\ndescription: From a folder\n---\nWork.\n",
        );
        write(&tmp.path().join("bundle/notes.md"), "just notes");
        write(&tmp.path().join("bundle/readme.txt"), "ignored");
        let bundle = agents::add(&p, &tmp.path().join("bundle")).unwrap();
        assert_eq!(bundle.names, ["extra"]);
        assert_eq!(bundle.skipped.len(), 1);
        assert!(bundle.skipped[0].starts_with("notes.md"));
        let empty = tmp.path().join("empty");
        fs::create_dir_all(&empty).unwrap();
        assert_eq!(
            agents::add(&p, &empty).unwrap_err().kind(),
            ErrorKind::InvalidInput
        );
        agents::remove(&p, "extra").unwrap();

        let doc = agents::launch_json(&[p.as_path()]);
        assert_eq!(doc["scout"]["prompt"], "Look.");
        assert_eq!(doc["scout"]["tools"], serde_json::json!(["Read", "Grep"]));
        assert_eq!(doc["scout"]["model"], "opus");
        assert!(doc["odd"].get("model").is_none() && doc["odd"].get("tools").is_none());

        // A later layer's agent replaces an earlier one of the same name and adds its own.
        let mine = tmp.path().join("mine");
        write(
            &mine.join("agents/scout.md"),
            "---\ndescription: Mine\n---\nMy look.\n",
        );
        write(
            &mine.join("agents/extra.md"),
            "---\ndescription: Extra\n---\nMore.\n",
        );
        let merged = agents::launch_json(&[p.as_path(), mine.as_path()]);
        assert_eq!(merged["scout"]["prompt"], "My look.");
        assert_eq!(merged["odd"]["prompt"], "Do odd things.");
        assert_eq!(merged["extra"]["prompt"], "More.");

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
            assert_eq!(agents::add(&p, &f).unwrap_err().kind(), kind, "{body}");
        }
        assert_eq!(
            agents::add(&p, &tmp.path().join("missing.md"))
                .unwrap_err()
                .kind(),
            ErrorKind::NotFound
        );
        assert_eq!(agents::agent_names(&p), ["odd", "scout"]);

        agents::remove(&p, "scout").unwrap();
        assert_eq!(
            agents::remove(&p, "scout").unwrap_err().kind(),
            ErrorKind::NotFound
        );
        assert_eq!(
            agents::remove(&p, "../x").unwrap_err().kind(),
            ErrorKind::InvalidInput
        );
        assert_eq!(agents::agent_names(&p), ["odd"]);
    }
}
