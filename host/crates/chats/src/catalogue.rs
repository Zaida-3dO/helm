//! The Claude model catalogue.
//!
//! The list comes from the worker's `get_state` answer (the CLI's `initialize` response, no
//! inference request) and is cached in `<profile>/catalogue/claude.json`. With nothing cached,
//! the fallback aliases apply. Efforts are low to max, and new chats default to `xhigh`.

use crate::types::{CatalogueSource, Harness, ModelCatalogue, ModelInfo, SkillInfo};
use serde_json::Value;
use std::path::{Path, PathBuf};

pub const EFFORTS: [&str; 5] = ["low", "medium", "high", "xhigh", "max"];
pub const DEFAULT_EFFORT: &str = "xhigh";
const CACHE_MAX: u64 = 256 * 1024;

pub fn is_effort(s: &str) -> bool {
    EFFORTS.contains(&s)
}

fn cache_path(profile_dir: &Path) -> PathBuf {
    profile_dir.join("catalogue").join("claude.json")
}

fn fallback_models() -> Vec<ModelInfo> {
    [
        (
            "default",
            "Default",
            "The model Claude Code picks by default",
        ),
        ("opus", "Opus", "Most capable"),
        (
            "opus[1m]",
            "Opus (1M context)",
            "Most capable, long context",
        ),
        ("sonnet", "Sonnet", "Fast and capable"),
        (
            "sonnet[1m]",
            "Sonnet (1M context)",
            "Fast and capable, long context",
        ),
        ("haiku", "Haiku", "Fastest"),
    ]
    .into_iter()
    .map(|(id, name, description)| ModelInfo {
        id: id.into(),
        name: name.into(),
        description: description.into(),
    })
    .collect()
}

fn catalogue(models: Vec<ModelInfo>, source: CatalogueSource) -> ModelCatalogue {
    ModelCatalogue {
        harness: Harness::Claude,
        models,
        efforts: EFFORTS.iter().map(|s| s.to_string()).collect(),
        default_effort: DEFAULT_EFFORT.into(),
        source,
        local: Vec::new(),
    }
}

/// The cached catalogue, or the fallback list.
pub fn load(profile_dir: &Path) -> ModelCatalogue {
    let cached = helm_core::privfs::read_capped(&cache_path(profile_dir), CACHE_MAX)
        .ok()
        .and_then(|b| serde_json::from_slice::<Vec<ModelInfo>>(&b).ok())
        .filter(|m| !m.is_empty());
    match cached {
        Some(models) => catalogue(models, CatalogueSource::Cache),
        None => catalogue(fallback_models(), CatalogueSource::Fallback),
    }
}

/// Models from a `get_state` answer's `data.models`, if it has any.
pub fn from_get_state(data: &Value) -> Option<Vec<ModelInfo>> {
    let models: Vec<ModelInfo> = data
        .get("models")?
        .as_array()?
        .iter()
        .filter_map(|m| {
            let id = m.get("id")?.as_str()?.to_owned();
            Some(ModelInfo {
                name: m
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or(&id)
                    .to_owned(),
                description: m
                    .get("description")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned(),
                id,
            })
        })
        .collect();
    (!models.is_empty()).then_some(models)
}

/// Cache a fresh list from the CLI. Returns the catalogue as the CLI reported it.
pub fn store(profile_dir: &Path, models: Vec<ModelInfo>) -> std::io::Result<ModelCatalogue> {
    let path = cache_path(profile_dir);
    helm_core::privfs::create_private_dir_all(path.parent().unwrap())?;
    let bytes = serde_json::to_vec(&models).map_err(std::io::Error::other)?;
    helm_core::privfs::write_atomic(&path, &bytes)?;
    Ok(catalogue(models, CatalogueSource::Cli))
}

// ---- The skills each folder last reported ----
//
// A chat's `/` menu lists the commands and skills its own worker reported. A fresh chat has no
// worker yet, so the last list any worker reported for the same folder stands in. Kept in
// `<profile>/catalogue/skills-by-folder.json`, so it also survives a restart; at most
// [`SKILL_FOLDERS`] folders (the oldest goes) and [`SKILLS_EACH`] skills each.

pub const SKILL_FOLDERS: usize = 64;
pub const SKILLS_EACH: usize = 500;
const SKILLS_FILE_MAX: u64 = 4 * 1024 * 1024;

fn skills_path(profile_dir: &Path) -> PathBuf {
    profile_dir.join("catalogue").join("skills-by-folder.json")
}

/// One folder's entry: when it was last reported, and the list.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
struct FolderSkills {
    at: i64,
    skills: Vec<SkillInfo>,
}

/// The cache key for a chat folder: separators and a trailing slash do not matter, and on Windows
/// neither does case.
pub fn folder_key(cwd: &str) -> String {
    let s = cwd.trim().replace('\\', "/");
    let s = s.trim_end_matches('/').to_owned();
    if cfg!(windows) {
        s.to_lowercase()
    } else {
        s
    }
}

fn read_skills(profile_dir: &Path) -> std::collections::BTreeMap<String, FolderSkills> {
    helm_core::privfs::read_capped(&skills_path(profile_dir), SKILLS_FILE_MAX)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

/// The last list reported for `cwd`'s folder; empty when none was.
pub fn cached_skills(profile_dir: &Path, cwd: &str) -> Vec<SkillInfo> {
    read_skills(profile_dir)
        .remove(&folder_key(cwd))
        .map(|f| f.skills)
        .unwrap_or_default()
}

/// Remember `skills` as `cwd`'s folder's list (a no-op when it is unchanged or empty).
pub fn remember_skills(
    profile_dir: &Path,
    cwd: &str,
    skills: &[SkillInfo],
    now: i64,
) -> std::io::Result<()> {
    if skills.is_empty() || cwd.trim().is_empty() {
        return Ok(());
    }
    let key = folder_key(cwd);
    let mut all = read_skills(profile_dir);
    let list: Vec<SkillInfo> = skills.iter().take(SKILLS_EACH).cloned().collect();
    if all.get(&key).is_some_and(|f| f.skills == list) {
        return Ok(());
    }
    all.insert(
        key,
        FolderSkills {
            at: now,
            skills: list,
        },
    );
    while all.len() > SKILL_FOLDERS {
        let oldest = all.iter().min_by_key(|(_, f)| f.at).map(|(k, _)| k.clone());
        match oldest {
            Some(k) => {
                all.remove(&k);
            }
            None => break,
        }
    }
    let path = skills_path(profile_dir);
    helm_core::privfs::create_private_dir_all(path.parent().unwrap())?;
    let bytes = serde_json::to_vec(&all).map_err(std::io::Error::other)?;
    helm_core::privfs::write_atomic(&path, &bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn skill(n: &str) -> SkillInfo {
        SkillInfo {
            name: n.into(),
            description: format!("{n} does things"),
        }
    }

    #[test]
    fn skills_are_kept_per_folder_and_the_oldest_folder_goes() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path();
        assert!(cached_skills(p, "C:/work/app").is_empty());
        remember_skills(p, "C:/work/app", &[skill("review")], 1).unwrap();
        assert_eq!(
            cached_skills(p, "C:/work/app/"),
            [skill("review")],
            "a trailing slash is the same folder"
        );
        assert_eq!(
            cached_skills(p, r"C:\work\app"),
            [skill("review")],
            "so are backslashes"
        );
        assert!(cached_skills(p, "C:/work/other").is_empty());
        remember_skills(p, "C:/work/app", &[], 2).unwrap();
        assert_eq!(
            cached_skills(p, "C:/work/app"),
            [skill("review")],
            "an empty report keeps the list"
        );
        for i in 0..SKILL_FOLDERS {
            remember_skills(p, &format!("C:/f{i}"), &[skill("x")], 10 + i as i64).unwrap();
        }
        assert!(
            cached_skills(p, "C:/work/app").is_empty(),
            "the oldest folder went"
        );
        assert_eq!(cached_skills(p, "C:/f0"), [skill("x")]);
    }

    #[test]
    fn fallback_then_cache() {
        let dir = tempfile::tempdir().unwrap();
        let c = load(dir.path());
        assert_eq!(c.source, CatalogueSource::Fallback);
        assert_eq!(c.default_effort, "xhigh");
        assert_eq!(c.efforts, ["low", "medium", "high", "xhigh", "max"]);
        assert!(c.models.iter().any(|m| m.id == "haiku"));
        let models = from_get_state(&json!({"models": [{"id": "claude-x", "name": "X"}]})).unwrap();
        assert_eq!(
            store(dir.path(), models).unwrap().source,
            CatalogueSource::Cli
        );
        let c = load(dir.path());
        assert_eq!(c.source, CatalogueSource::Cache);
        assert_eq!(c.models[0].id, "claude-x");
        assert!(from_get_state(&json!({"models": []})).is_none());
    }
}
