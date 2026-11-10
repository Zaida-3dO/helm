//! The plugin folders one chat launches with.
//!
//! A chat's skills come from three places, each loaded as its own Claude Code plugin through
//! `--plugin-dir` so the CLI shows them as `helm:<name>`, `personal:<name>` and `project:<name>`:
//!
//! | plugin | holds |
//! |---|---|
//! | `helm` | the Everyone layer's skills, and linked Everyone folders scoped to all projects |
//! | `personal` | the chat's profile layer's skills, and linked folders of that profile scoped to all projects |
//! | `project` | linked folders (either layer) scoped to the chat's project |
//!
//! The folders are generated, not the layers themselves: skills are **copied** into
//! `<profile>/launch-plugins/<hash>/<plugin>/skills/<name>/`, where the hash is taken over the
//! skills' contents, so the same inputs reuse one folder and a change builds a new one. Plain
//! copies, never links. A plugin with no skills is left out. When two sources give one plugin a
//! skill of the same name, the first wins (the layer's own folder before linked folders, linked
//! folders in list order) and the clash is reported.

use crate::launch::ChatKnowledge;
use crate::layers::Layer;
use crate::sources::{self, Projects};
use crate::{copy_skill, front_description, skill_files};
use serde::Serialize;
use serde_json::Value;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

/// The plugins, in the order they are listed and hashed.
pub const PLUGINS: [&str; 3] = ["helm", "personal", "project"];

const FOLDER: &str = "launch-plugins";
/// The newest folders always kept…
const KEEP: usize = 3;
/// …and any used within this long, since a running chat may still read its skills from one.
const KEEP_FOR: Duration = Duration::from_secs(24 * 60 * 60);
/// How many hash characters name a folder (the folder path must stay short on Windows).
const HASH_CHARS: usize = 16;

/// One skill a chat will load.
#[derive(Clone, Debug)]
pub struct PluginSkill {
    pub plugin: &'static str,
    pub name: String,
    pub description: String,
    pub layer: Layer,
    /// `folder` (the layer's own skills folder) or `linked:<id>`.
    pub source: String,
    /// The skill's folder to copy from.
    pub dir: PathBuf,
    fingerprint: String,
}

/// A skill name two sources both gave the same plugin: the first is loaded.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Clash {
    pub plugin: String,
    pub name: String,
    pub kept: String,
    pub dropped: String,
}

#[derive(Clone, Debug, Default)]
pub struct Collected {
    pub skills: Vec<PluginSkill>,
    pub clashes: Vec<Clash>,
}

struct Candidate {
    plugin: &'static str,
    name: String,
    layer: Layer,
    source: String,
    dir: PathBuf,
}

/// A hash over the skill's files (names, sizes and contents). Also the check that the skill is
/// within its limits.
fn fingerprint(dir: &Path) -> io::Result<String> {
    let mut lines = String::new();
    for f in skill_files(dir)? {
        let bytes = helm_core::privfs::read_capped(&f.path, crate::MAX_SKILL_FILE)?;
        lines.push_str(&format!(
            "{}\0{}\0{}\n",
            f.rel,
            f.len,
            helm_core::sha256_hex(&bytes)
        ));
    }
    Ok(helm_core::sha256_hex(lines.as_bytes()))
}

/// The skills this chat loads, with the clashes found. `who` says which profile layer and which
/// project apply; a chat with no known profile gets the Everyone layer only.
pub fn collect(profile_dir: &Path, who: &ChatKnowledge) -> Collected {
    let linked = sources::enabled(profile_dir);
    let project = who.project_id.as_deref();
    let mut layers = vec![Layer::Everyone];
    if let Some(p) = &who.profile {
        layers.push(Layer::Profile(p.id.clone()));
    }
    let mut cands: Vec<Candidate> = Vec::new();
    for layer in layers {
        let own = match layer {
            Layer::Everyone => "helm",
            Layer::Profile(_) => "personal",
        };
        for s in crate::list(&layer.root(profile_dir)) {
            cands.push(Candidate {
                plugin: own,
                name: s.name,
                layer: layer.clone(),
                source: "folder".into(),
                dir: PathBuf::from(s.path),
            });
        }
        for src in linked
            .iter()
            .filter(|s| Layer::parse(&s.layer).as_ref() == Some(&layer))
        {
            if !src.projects.includes(project) {
                continue;
            }
            let plugin = match src.projects {
                Projects::All => own,
                Projects::Only(_) => "project",
            };
            for (name, dir) in sources::skills_in(Path::new(&src.path)) {
                cands.push(Candidate {
                    plugin,
                    name,
                    layer: layer.clone(),
                    source: format!("linked:{}", src.id),
                    dir,
                });
            }
        }
    }

    let mut out = Collected::default();
    for c in cands {
        if let Some(kept) = out
            .skills
            .iter()
            .find(|s| s.plugin == c.plugin && s.name == c.name)
        {
            out.clashes.push(Clash {
                plugin: c.plugin.to_owned(),
                name: c.name,
                kept: kept.source.clone(),
                dropped: c.source,
            });
            continue;
        }
        let fingerprint = match fingerprint(&c.dir) {
            Ok(f) => f,
            Err(e) => {
                log::warn!("skills: {} ({}) is not loaded: {e}", c.name, c.source);
                continue;
            }
        };
        out.skills.push(PluginSkill {
            plugin: c.plugin,
            description: front_description(&c.dir.join("SKILL.md")),
            name: c.name,
            layer: c.layer,
            source: c.source,
            dir: c.dir,
            fingerprint,
        });
    }
    out
}

/// One skill in a layer as Settings shows it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ViewSkill {
    pub name: String,
    pub description: String,
    /// The skill's folder, for opening.
    pub path: String,
    /// `folder` or `linked:<id>`.
    pub source: String,
    pub projects: Projects,
}

/// A layer's skills as Settings shows them: its own folder, then the enabled linked folders of the
/// layer, and the clashes between them (one name given twice to the same set of projects).
pub fn layer_view(profile_dir: &Path, layer: &Layer) -> (Vec<ViewSkill>, Vec<Clash>) {
    let mut skills: Vec<ViewSkill> = crate::list(&layer.root(profile_dir))
        .into_iter()
        .map(|s| ViewSkill {
            name: s.name,
            description: s.description,
            path: s.path,
            source: "folder".into(),
            projects: Projects::All,
        })
        .collect();
    for src in sources::enabled(profile_dir)
        .into_iter()
        .filter(|s| Layer::parse(&s.layer).as_ref() == Some(layer))
    {
        for (name, dir) in sources::skills_in(Path::new(&src.path)) {
            skills.push(ViewSkill {
                description: front_description(&dir.join("SKILL.md")),
                name,
                path: dir.to_string_lossy().into_owned(),
                source: format!("linked:{}", src.id),
                projects: src.projects.clone(),
            });
        }
    }
    let mut clashes = Vec::new();
    for (i, later) in skills.iter().enumerate() {
        if let Some(first) = skills[..i]
            .iter()
            .find(|e| e.name == later.name && e.projects.overlaps(&later.projects))
        {
            clashes.push(Clash {
                plugin: String::new(),
                name: later.name.clone(),
                kept: first.source.clone(),
                dropped: later.source.clone(),
            });
        }
    }
    (skills, clashes)
}

/// What a launch gets from the built folder.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Built {
    /// The plugin folders for `--plugin-dir`, in [`PLUGINS`] order.
    pub plugin_dirs: Vec<String>,
    /// The agents file for `--agents`.
    pub agents_file: Option<String>,
}

fn manifest(plugin: &str) -> Vec<u8> {
    let about = match plugin {
        "helm" => "Skills for everyone who uses this Helm.",
        "personal" => "Skills for the person a chat is for.",
        _ => "Skills for one project.",
    };
    serde_json::to_vec_pretty(&serde_json::json!({
        "name": plugin,
        "description": about,
        "version": "1.0.0",
    }))
    .unwrap_or_default()
}

/// Build (or reuse) the folder for `skills` and the merged `agents` document, and say which
/// plugin folders and agents file a launch passes. Nothing to load gives an empty result and no
/// folder.
pub fn build(profile_dir: &Path, skills: &[PluginSkill], agents: &Value) -> io::Result<Built> {
    let agents_json = agents
        .as_object()
        .filter(|o| !o.is_empty())
        .map(|_| helm_core::revisions::canonical_json(agents));
    if skills.is_empty() && agents_json.is_none() {
        return Ok(Built::default());
    }

    let mut key = String::from("v1\n");
    for plugin in PLUGINS {
        let mut mine: Vec<&PluginSkill> = skills.iter().filter(|s| s.plugin == plugin).collect();
        mine.sort_by(|a, b| a.name.cmp(&b.name));
        for s in mine {
            key.push_str(&format!("{plugin}\0{}\0{}\n", s.name, s.fingerprint));
        }
    }
    key.push_str("agents\0");
    key.push_str(agents_json.as_deref().unwrap_or(""));
    let hash: String = helm_core::sha256_hex(key.as_bytes())
        .chars()
        .take(HASH_CHARS)
        .collect();

    let base = profile_dir.join(FOLDER);
    let dir = base.join(&hash);
    if !dir.is_dir() {
        make(&base, &hash, skills, agents_json.as_deref())?;
    }
    // Record the use, so pruning keeps the folders chats are starting with.
    if let Err(e) = helm_core::privfs::write_atomic(&dir.join(".used"), b"1\n") {
        log::warn!("skills: could not mark {hash} as used: {e}");
    }
    prune(&base, &hash);

    let plugin_dirs = PLUGINS
        .into_iter()
        .filter(|p| skills.iter().any(|s| s.plugin == *p))
        .map(|p| dir.join(p).to_string_lossy().into_owned())
        .collect();
    Ok(Built {
        plugin_dirs,
        agents_file: agents_json.map(|_| dir.join("agents.json").to_string_lossy().into_owned()),
    })
}

fn make(
    base: &Path,
    hash: &str,
    skills: &[PluginSkill],
    agents_json: Option<&str>,
) -> io::Result<()> {
    helm_core::privfs::create_private_dir_all(base)?;
    let temp = base.join(format!(".{hash}.{}.tmp", uuid::Uuid::new_v4().simple()));
    helm_core::privfs::create_private_dir_all(&temp)?;
    let built = (|| -> io::Result<()> {
        for plugin in PLUGINS {
            let mine: Vec<&PluginSkill> = skills.iter().filter(|s| s.plugin == plugin).collect();
            if mine.is_empty() {
                continue;
            }
            let root = temp.join(plugin);
            helm_core::privfs::create_private_dir_all(&root.join(".claude-plugin"))?;
            helm_core::privfs::write_atomic(
                &root.join(".claude-plugin").join("plugin.json"),
                &manifest(plugin),
            )?;
            for s in mine {
                let dest = root.join("skills").join(&s.name);
                helm_core::privfs::create_private_dir_all(&dest)?;
                copy_skill(&s.dir, &dest)?;
            }
        }
        if let Some(json) = agents_json {
            helm_core::privfs::write_atomic(&temp.join("agents.json"), json.as_bytes())?;
        }
        Ok(())
    })();
    let dir = base.join(hash);
    let placed = built.and_then(|()| fs::rename(&temp, &dir));
    match placed {
        Ok(()) => Ok(()),
        // Another launch placed the same folder first: use it.
        Err(_) if dir.is_dir() => {
            let _ = fs::remove_dir_all(&temp);
            Ok(())
        }
        Err(e) => {
            let _ = fs::remove_dir_all(&temp);
            Err(e)
        }
    }
}

fn used_at(dir: &Path) -> SystemTime {
    fs::metadata(dir.join(".used"))
        .or_else(|_| fs::metadata(dir))
        .and_then(|m| m.modified())
        .unwrap_or(SystemTime::UNIX_EPOCH)
}

/// Delete built folders beyond the newest [`KEEP`] that were not used lately, and abandoned
/// temporary ones. They are plain copies, so a plain delete is right.
fn prune(base: &Path, keep: &str) {
    let Ok(read) = fs::read_dir(base) else {
        return;
    };
    let now = SystemTime::now();
    let mut built: Vec<(PathBuf, SystemTime)> = Vec::new();
    for e in read.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        let path = e.path();
        if name.starts_with('.') {
            let old = used_at(&path)
                .elapsed()
                .map(|d| d > Duration::from_secs(60 * 60))
                .unwrap_or(false);
            if name.ends_with(".tmp") && old {
                let _ = fs::remove_dir_all(&path);
            }
        } else if path.is_dir() {
            let used = used_at(&path);
            built.push((path, used));
        }
    }
    built.sort_by(|a, b| b.1.cmp(&a.1));
    for (i, (path, used)) in built.into_iter().enumerate() {
        let recent = now
            .duration_since(used)
            .map(|d| d < KEEP_FOR)
            .unwrap_or(true);
        let is_keep = path
            .file_name()
            .is_some_and(|n| n.to_string_lossy() == keep);
        if i >= KEEP && !recent && !is_keep {
            let _ = fs::remove_dir_all(&path);
        }
    }
}
