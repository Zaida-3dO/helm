//! Agents.
//!
//! A layer's `agents/` folder holds one Markdown file per agent: a front matter block (`name`,
//! `description`, optionally `tools` and `model`) and then the agent's instructions. Every chat
//! launch merges the Everyone layer's agents with the chat's profile layer (the profile wins on a
//! name clash) and hands them to the Claude Code CLI as one `--agents` file, so the agents are
//! known by their plain names (`planner`, `reviewer`, …) in every chat, whatever its project. The
//! plain names are what Settings > Orchestrator's stages point at. A plugin folder would give them
//! a `helm:` prefix, which no stage names.
//!
//! Agents are not part of the skills plugins, so the CLI never loads the same agent twice under
//! two names. Every function here takes a layer's root folder.

use serde::Serialize;
use serde_json::{json, Map, Value};
use std::fs;
use std::io::{self, ErrorKind};
use std::path::{Path, PathBuf};

/// One agent file holds at most this many bytes.
pub const MAX_AGENT_FILE: u64 = 128 * 1024;
/// A launch carries at most this many agents.
const MAX_AGENTS: usize = 64;
/// The model names the CLI accepts in an agent definition. Anything else is left out, so the agent
/// uses the chat's own model.
const MODEL_ALIASES: [&str; 4] = ["sonnet", "opus", "haiku", "inherit"];

/// The agents folder inside a layer's root.
pub fn agents_dir(root: &Path) -> PathBuf {
    root.join("agents")
}

/// One agent as Settings sees it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentEntry {
    /// The file name without `.md`: what a chat's Agent tool and a stage setting call it.
    pub name: String,
    pub description: String,
    /// The agent's file, for opening.
    pub path: String,
}

/// An agent file taken apart.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct ParsedAgent {
    pub name: String,
    pub description: String,
    pub tools: Vec<String>,
    pub model: String,
    pub prompt: String,
}

/// Split an agent file into its front matter fields and its instructions. `None` when there is no
/// front matter block at all. Values are single-line; a line starting with `#` inside the block is
/// a comment.
pub fn parse_agent(text: &str) -> Option<ParsedAgent> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let mut lines = text.lines();
    if lines.by_ref().find(|l| !l.trim().is_empty())?.trim() != "---" {
        return None;
    }
    let mut out = ParsedAgent::default();
    let mut closed = false;
    let mut body: Vec<&str> = Vec::new();
    for line in lines {
        if closed {
            body.push(line);
            continue;
        }
        let t = line.trim_end();
        if t == "---" {
            closed = true;
            continue;
        }
        if t.trim_start().starts_with('#') {
            continue;
        }
        let Some((key, value)) = t.split_once(':') else {
            continue;
        };
        let value = unquote(value.trim());
        match key.trim() {
            "name" => out.name = value.to_owned(),
            "description" => out.description = value.to_owned(),
            "model" => out.model = value.to_owned(),
            "tools" => out.tools = split_list(value),
            _ => {}
        }
    }
    if !closed {
        return None;
    }
    out.prompt = body.join("\n").trim().to_owned();
    Some(out)
}

fn unquote(v: &str) -> &str {
    v.trim_matches(|c| c == '"' || c == '\'')
}

/// `[Read, Grep]` or `Read, Grep`: the names, trimmed and unquoted.
fn split_list(v: &str) -> Vec<String> {
    v.trim_start_matches('[')
        .trim_end_matches(']')
        .split(',')
        .map(|s| unquote(s.trim()).to_owned())
        .filter(|s| !s.is_empty())
        .collect()
}

fn read_agent(path: &Path) -> Option<ParsedAgent> {
    let bytes = helm_core::privfs::read_capped(path, MAX_AGENT_FILE).ok()?;
    parse_agent(&String::from_utf8_lossy(&bytes))
}

/// Every agent in the folder, by name. A missing folder simply means none; a file that is not an
/// agent file (no front matter, no description, a bad name) is not listed.
pub fn list(root: &Path) -> Vec<AgentEntry> {
    let Ok(read) = fs::read_dir(agents_dir(root)) else {
        return Vec::new();
    };
    let mut out: Vec<AgentEntry> = read
        .flatten()
        .filter_map(|e| {
            let file = e.file_name().to_string_lossy().into_owned();
            let name = file.strip_suffix(".md")?;
            if !crate::is_valid_name(name) {
                return None;
            }
            let parsed = read_agent(&e.path())?;
            if parsed.description.is_empty() {
                return None;
            }
            Some(AgentEntry {
                name: name.to_owned(),
                description: parsed.description,
                path: e.path().to_string_lossy().into_owned(),
            })
        })
        .collect();
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

/// The agent names, for matching against what a stage or a chat asks for.
pub fn agent_names(root: &Path) -> Vec<String> {
    list(root).into_iter().map(|a| a.name).collect()
}

/// What an import brought in, and what it left out.
#[derive(Debug, Default)]
pub struct Added {
    pub names: Vec<String>,
    /// One line per file that was not taken: its name and the reason.
    pub skipped: Vec<String>,
}

/// Import `source`: one agent file, or a folder whose `.md` files (not its subfolders) are each
/// tried. A single bad file is an error; in a folder it is skipped and reported, and the import
/// fails only when nothing at all was taken.
pub fn add(root: &Path, source: &Path) -> io::Result<Added> {
    if !source.is_dir() {
        let one = add_file(root, source)?;
        return Ok(Added {
            names: vec![one.name],
            skipped: Vec::new(),
        });
    }
    let mut files: Vec<PathBuf> = fs::read_dir(source)?
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_file() && p.extension().is_some_and(|x| x.eq_ignore_ascii_case("md")))
        .collect();
    files.sort();
    let mut out = Added::default();
    for f in files {
        match add_file(root, &f) {
            Ok(a) => out.names.push(a.name),
            Err(e) => out.skipped.push(format!(
                "{}: {e}",
                f.file_name().unwrap_or_default().to_string_lossy()
            )),
        }
    }
    if out.names.is_empty() {
        let why = match out.skipped.first() {
            Some(first) => format!("no agent file in that folder could be added ({first})"),
            None => "that folder has no .md agent files".to_owned(),
        };
        return Err(io::Error::new(ErrorKind::InvalidInput, why));
    }
    Ok(out)
}

/// Copy one agent file in as a new agent. The agent takes the name in its front matter, or else the
/// file's own name.
pub fn add_file(root: &Path, source: &Path) -> io::Result<AgentEntry> {
    let bytes = helm_core::privfs::read_capped(source, MAX_AGENT_FILE).map_err(|e| {
        if e.kind() == ErrorKind::InvalidData {
            io::Error::new(ErrorKind::InvalidInput, "the agent file is too large")
        } else {
            io::Error::new(ErrorKind::NotFound, "that file cannot be read")
        }
    })?;
    let text = String::from_utf8_lossy(&bytes).into_owned();
    let bad = |why: &str| io::Error::new(ErrorKind::InvalidInput, why.to_owned());
    if helm_core::hidden::find_hidden(&Value::String(text.clone())).is_some() {
        return Err(bad(
            "the file holds invisible or direction-changing characters",
        ));
    }
    let parsed = parse_agent(&text).ok_or_else(|| bad("the file has no front matter block"))?;
    if parsed.description.is_empty() {
        return Err(bad("the front matter needs a description"));
    }
    if parsed.prompt.is_empty() {
        return Err(bad("the file has no instructions after the front matter"));
    }
    let stem = source
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let name = if parsed.name.is_empty() {
        stem
    } else {
        parsed.name.clone()
    };
    if !crate::is_valid_name(&name) {
        return Err(bad("name must be lower-case letters, digits and hyphens"));
    }
    let dest = agents_dir(root).join(format!("{name}.md"));
    if dest.exists() {
        return Err(io::Error::new(
            ErrorKind::AlreadyExists,
            "an agent of that name is already in the folder",
        ));
    }
    helm_core::privfs::create_private_dir_all(&agents_dir(root))?;
    helm_core::privfs::write_atomic(&dest, text.as_bytes())?;
    Ok(AgentEntry {
        name,
        description: parsed.description,
        path: dest.to_string_lossy().into_owned(),
    })
}

/// Remove one agent. Only a plain file in the folder can go, never through a link.
pub fn remove(root: &Path, name: &str) -> io::Result<()> {
    if !crate::is_valid_name(name) {
        return Err(io::Error::new(ErrorKind::InvalidInput, "not an agent name"));
    }
    let path = agents_dir(root).join(format!("{name}.md"));
    match fs::symlink_metadata(&path) {
        Err(e) if e.kind() == ErrorKind::NotFound => {
            Err(io::Error::new(ErrorKind::NotFound, "no such agent"))
        }
        Err(e) => Err(e),
        Ok(m) if m.file_type().is_symlink() || !m.is_file() => {
            Err(io::Error::new(ErrorKind::InvalidInput, "not an agent file"))
        }
        Ok(_) => fs::remove_file(&path),
    }
}

/// The `--agents` document for the layers' folders, earliest first (a later layer's agent replaces
/// an earlier one of the same name): `{name: {description, prompt, tools?, model?}}`.
pub fn launch_json(roots: &[&Path]) -> Value {
    let mut map = Map::new();
    for root in roots {
        for entry in list(root) {
            let Some(p) = read_agent(Path::new(&entry.path)) else {
                continue;
            };
            if p.prompt.is_empty() {
                continue;
            }
            let mut def = Map::new();
            def.insert("description".into(), json!(p.description));
            def.insert("prompt".into(), json!(p.prompt));
            if !p.tools.is_empty() {
                def.insert("tools".into(), json!(p.tools));
            }
            if MODEL_ALIASES.contains(&p.model.to_ascii_lowercase().as_str()) {
                def.insert("model".into(), json!(p.model.to_ascii_lowercase()));
            }
            map.insert(entry.name, Value::Object(def));
        }
    }
    // A launch carries a bounded number of agents; the earliest names stay.
    if map.len() > MAX_AGENTS {
        let keep: Vec<String> = map.keys().take(MAX_AGENTS).cloned().collect();
        map.retain(|k, _| keep.contains(k));
    }
    Value::Object(map)
}
