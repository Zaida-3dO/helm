//! Linked skill folders: folders elsewhere on this PC whose subfolders each hold a skill.
//!
//! `<profile>/skill-sources.json` lists up to 16 of them. Each says which layer its skills join
//! (Everyone, or one profile) and which projects they load in (all, or a list of project ids),
//! and can be switched off. A linked folder is read in place at each launch and never copied into
//! the layer or synced to other devices. The list is revisioned like the MCP server list: a save
//! carries the revision it was based on and is refused when the file has moved on.

use crate::layers::Layer;
use crate::{is_inside, is_valid_name};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::{json, Value};
use std::fs;
use std::io::{self, ErrorKind};
use std::path::{Path, PathBuf};

pub const FILE: &str = "skill-sources.json";
/// At most this many linked folders.
pub const MAX_SOURCES: usize = 16;
/// A source scoped to projects names at most this many.
const MAX_PROJECTS: usize = 64;

/// Which projects a linked folder's skills load in.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Projects {
    All,
    Only(Vec<String>),
}

impl Serialize for Projects {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match self {
            Projects::All => s.serialize_str("all"),
            Projects::Only(ids) => ids.serialize(s),
        }
    }
}

impl<'de> Deserialize<'de> for Projects {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Raw {
            Word(String),
            List(Vec<String>),
        }
        match Raw::deserialize(d)? {
            Raw::Word(w) if w == "all" => Ok(Projects::All),
            Raw::Word(_) => Err(serde::de::Error::custom(
                "projects is \"all\" or a list of ids",
            )),
            Raw::List(ids) => Ok(Projects::Only(ids)),
        }
    }
}

impl Projects {
    /// Whether a chat in `project` (if it has one) loads the source's skills.
    pub fn includes(&self, project: Option<&str>) -> bool {
        match self {
            Projects::All => true,
            Projects::Only(ids) => project.is_some_and(|p| ids.iter().any(|i| i == p)),
        }
    }

    /// Whether two scopes can load in the same chat as the same kind of skill: both "all", or two
    /// lists with a project in common.
    pub fn overlaps(&self, other: &Projects) -> bool {
        match (self, other) {
            (Projects::All, Projects::All) => true,
            (Projects::Only(a), Projects::Only(b)) => a.iter().any(|x| b.contains(x)),
            _ => false,
        }
    }
}

/// One linked folder.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SkillSource {
    /// Made when a source is first saved without one.
    #[serde(default)]
    pub id: String,
    pub path: String,
    /// `everyone` or `profile:<profileId>`.
    pub layer: String,
    #[ts(type = "\"all\" | Array<string>")]
    pub projects: Projects,
    pub enabled: bool,
}

/// The list as stored, with its revision.
#[derive(Clone, Debug, PartialEq, Serialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct Snapshot {
    pub sources: Vec<SkillSource>,
    pub revision: String,
}

fn file(profile_dir: &Path) -> PathBuf {
    profile_dir.join(FILE)
}

/// A hash of the list's content, so a pretty-printed or hand-edited file has the revision of what
/// it says, not of its bytes.
pub fn revision_of(sources: &[SkillSource]) -> String {
    let v = serde_json::to_value(sources).unwrap_or(Value::Null);
    helm_core::sha256_hex(helm_core::revisions::canonical_json(&v).as_bytes())
}

fn snapshot(sources: Vec<SkillSource>) -> Snapshot {
    Snapshot {
        revision: revision_of(&sources),
        sources,
    }
}

/// The saved list. A missing file is an empty list; an unreadable one is an error naming it.
pub fn load(profile_dir: &Path) -> io::Result<Snapshot> {
    let bytes = match helm_core::privfs::read_capped(&file(profile_dir), 256 * 1024) {
        Ok(b) => b,
        Err(e) if e.kind() == ErrorKind::NotFound => return Ok(snapshot(Vec::new())),
        Err(e) => return Err(e),
    };
    let doc: Value = serde_json::from_slice(&bytes)
        .map_err(|_| io::Error::new(ErrorKind::InvalidData, "skill-sources.json cannot be read"))?;
    let list = doc
        .get("sources")
        .cloned()
        .unwrap_or_else(|| Value::Array(Vec::new()));
    let sources: Vec<SkillSource> = serde_json::from_value(list)
        .map_err(|_| io::Error::new(ErrorKind::InvalidData, "skill-sources.json cannot be read"))?;
    Ok(snapshot(sources))
}

/// The enabled sources, or none when the file cannot be read (a launch never fails on it).
pub fn enabled(profile_dir: &Path) -> Vec<SkillSource> {
    match load(profile_dir) {
        Ok(s) => s.sources.into_iter().filter(|x| x.enabled).collect(),
        Err(e) => {
            log::warn!("skills: the linked folders are not loaded: {e}");
            Vec::new()
        }
    }
}

fn store(profile_dir: &Path, sources: Vec<SkillSource>) -> io::Result<Snapshot> {
    let body = json!({ "version": 1, "sources": sources });
    helm_core::privfs::write_atomic(
        &file(profile_dir),
        serde_json::to_vec_pretty(&body)
            .map_err(io::Error::other)?
            .as_slice(),
    )?;
    Ok(snapshot(sources))
}

fn invalid(why: impl Into<String>) -> io::Error {
    io::Error::new(ErrorKind::InvalidInput, why.into())
}

fn stale() -> io::Error {
    io::Error::new(
        ErrorKind::AlreadyExists,
        "The linked folders changed since they were read. Read them again, then retry.",
    )
}

/// Why a folder may not be linked, or its checked path. Refused: anything but a complete local
/// path (shares, devices, relative paths), a drive root, Helm's own data folder or a folder
/// holding it, and a folder that is not there.
fn check_path(profile_dir: &Path, raw: &str) -> io::Result<PathBuf> {
    let path = helm_platform::pathgate::check(raw).map_err(|r| invalid(r.message()))?;
    if path.parent().is_none() || path.file_name().is_none() {
        return Err(invalid("A whole drive cannot be linked; pick a folder."));
    }
    let mut roots = vec![profile_dir.to_path_buf()];
    if let Ok(d) = helm_core::profile::default_profile_dir() {
        roots.push(d);
    }
    let own = helm_platform::private::PrivateRoots::new(&roots).contains(&path);
    if own || roots.iter().any(|r| is_inside(r, &path)) {
        return Err(invalid(
            "Helm's own data folder, or a folder that holds it, cannot be linked.",
        ));
    }
    if !path.is_dir() {
        return Err(invalid("That folder is not here."));
    }
    Ok(path)
}

fn clean(profile_dir: &Path, mut s: SkillSource) -> io::Result<SkillSource> {
    s.path = check_path(profile_dir, s.path.trim())?
        .to_string_lossy()
        .into_owned();
    if Layer::parse(&s.layer).is_none() {
        return Err(invalid("The layer is `everyone` or `profile:<id>`."));
    }
    if let Projects::Only(ids) = &s.projects {
        let ok = !ids.is_empty()
            && ids.len() <= MAX_PROJECTS
            && ids
                .iter()
                .all(|i| !i.is_empty() && i.len() <= 128 && !helm_core::hidden::has_hidden(i));
        if !ok {
            return Err(invalid(
                "Choose all projects, or between 1 and 64 projects by id.",
            ));
        }
    }
    let id = s.id.trim();
    s.id = if id.is_empty() {
        uuid::Uuid::new_v4().simple().to_string()
    } else if id.len() <= 64 && id.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-') {
        id.to_owned()
    } else {
        return Err(invalid("That source id is not valid."));
    };
    Ok(s)
}

/// Add a source, or replace the one with the same id. `revision` is the list's revision when it
/// was read (`None` skips the check, for a first save).
pub fn save(
    profile_dir: &Path,
    source: SkillSource,
    revision: Option<&str>,
) -> io::Result<Snapshot> {
    let mut list = load(profile_dir)?.sources;
    if revision.is_some_and(|r| r != revision_of(&list)) {
        return Err(stale());
    }
    let source = clean(profile_dir, source)?;
    let same_folder = |a: &SkillSource| {
        a.id != source.id
            && a.layer == source.layer
            && is_inside(Path::new(&a.path), Path::new(&source.path))
            && is_inside(Path::new(&source.path), Path::new(&a.path))
    };
    if list.iter().any(same_folder) {
        return Err(invalid("That folder is already linked to that layer."));
    }
    match list.iter_mut().find(|x| x.id == source.id) {
        Some(slot) => *slot = source,
        None => {
            if list.len() >= MAX_SOURCES {
                return Err(invalid("At most 16 folders can be linked."));
            }
            list.push(source);
        }
    }
    store(profile_dir, list)
}

/// Remove a source by id.
pub fn remove(profile_dir: &Path, id: &str, revision: Option<&str>) -> io::Result<Snapshot> {
    let mut list = load(profile_dir)?.sources;
    if revision.is_some_and(|r| r != revision_of(&list)) {
        return Err(stale());
    }
    let before = list.len();
    list.retain(|s| s.id != id);
    if list.len() == before {
        return Err(io::Error::new(
            ErrorKind::NotFound,
            "That linked folder is no longer listed.",
        ));
    }
    store(profile_dir, list)
}

/// The skill folders directly inside a linked folder: a subfolder with a `SKILL.md` and a valid
/// skill name. A link inside is not followed. Sorted by name.
pub fn skills_in(source_path: &Path) -> Vec<(String, PathBuf)> {
    let Ok(read) = fs::read_dir(source_path) else {
        return Vec::new();
    };
    let mut out: Vec<(String, PathBuf)> = read
        .flatten()
        .filter_map(|e| {
            let meta = e.metadata().ok()?;
            if helm_platform::fs::is_link_like(&meta) || !meta.is_dir() {
                return None;
            }
            let name = e.file_name().to_string_lossy().into_owned();
            (is_valid_name(&name) && e.path().join("SKILL.md").is_file()).then(|| (name, e.path()))
        })
        .collect();
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}
