//! The Claude model catalogue.
//!
//! The list comes from the worker's `get_state` answer (the CLI's `initialize` response, no
//! inference request) and is cached in `<profile>/catalogue/claude.json`. With nothing cached,
//! the fallback aliases apply. Efforts are low to max, and new chats default to `xhigh`.

use crate::types::{CatalogueSource, Harness, ModelCatalogue, ModelInfo};
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

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
