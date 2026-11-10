//! Undo for what an agent changes in the knowledge layers.
//!
//! When an agent saves or removes a rule or a skill, the tool first copies the item it is about to
//! replace or remove into `<profile>/knowledge/.trash/<id>/item`, then files the change in the
//! change ring under [`KEY`] with a note of what to put back. The user sees the usual notice with
//! Undo; Undo removes what the agent made and restores the saved copy ([`revert`]).
//!
//! The ring compares the state a change left with the state now before it undoes anything, so the
//! recorded "state" here is a digest of every layer's files ([`digest`]): if a rule or skill
//! changed since, whoever changed it, Undo is refused rather than overwrite it.

use crate::layers::{self, Kind, Layer};
use crate::{agents, install_dir, is_valid_name, rules};
use helm_hosttools::SettingsExtension;
use helm_ipc::AppEvent;
use rusqlite::Transaction;
use serde_json::{json, Value};
use std::fs;
use std::io::{self, ErrorKind};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

/// The change-ring key a knowledge change is filed under.
pub const KEY: &str = "knowledge";

const TRASH: &str = ".trash";
const KEEP_TRASH: usize = 50;
const TRASH_AGE: Duration = Duration::from_secs(14 * 24 * 60 * 60);

fn invalid(why: &str) -> io::Error {
    io::Error::new(ErrorKind::InvalidInput, why.to_owned())
}

/// A hash of every layer's files (path, size and modified time): equal before and after means
/// nothing in the layers changed in between. Folders the sync keeps its own books in (names
/// starting with a dot) and parked conflicts are left out.
pub fn digest(profile_dir: &Path) -> String {
    let base = layers::knowledge_dir(profile_dir);
    let mut lines: Vec<String> = Vec::new();
    let mut stack: Vec<(PathBuf, String)> = vec![(base, String::new())];
    while let Some((dir, rel)) = stack.pop() {
        let Ok(read) = fs::read_dir(&dir) else {
            continue;
        };
        for e in read.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            if name.starts_with('.') || (rel.is_empty() && name == "conflicts") {
                continue;
            }
            let here = if rel.is_empty() {
                name
            } else {
                format!("{rel}/{name}")
            };
            let Ok(meta) = e.metadata() else { continue };
            if meta.is_dir() {
                stack.push((e.path(), here));
            } else {
                let at = meta
                    .modified()
                    .ok()
                    .and_then(|t| t.duration_since(SystemTime::UNIX_EPOCH).ok())
                    .map(|d| d.as_millis())
                    .unwrap_or(0);
                lines.push(format!("{here}\0{}\0{at}", meta.len()));
            }
        }
    }
    lines.sort();
    helm_core::sha256_hex(lines.join("\n").as_bytes())
}

/// What Undo puts back for one item.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Restore {
    pub layer: Layer,
    pub kind: Kind,
    /// A rule's file name, a skill's or an agent's name.
    pub name: String,
    /// The saved copy of the item as it was, or `None` when it did not exist.
    pub trash: Option<String>,
}

impl Restore {
    pub fn to_json(&self) -> Value {
        json!({
            "layer": self.layer.key(),
            "kind": self.kind.key(),
            "name": self.name,
            "trash": self.trash,
        })
    }

    pub fn from_json(v: &Value) -> Option<Restore> {
        Some(Restore {
            layer: Layer::parse(v.get("layer")?.as_str()?)?,
            kind: Kind::parse(v.get("kind")?.as_str()?)?,
            name: v.get("name")?.as_str()?.to_owned(),
            trash: v
                .get("trash")
                .and_then(Value::as_str)
                .map(str::to_owned)
                .filter(|t| t.bytes().all(|c| c.is_ascii_alphanumeric())),
        })
    }
}

/// Where an item lives: a rule's file, a skill's folder or an agent's file, validated so a name
/// from outside can never leave its folder.
pub fn item_path(profile_dir: &Path, layer: &Layer, kind: Kind, name: &str) -> io::Result<PathBuf> {
    let root = layer.root(profile_dir);
    let leaf = match kind {
        Kind::Rule => rules::file_name_of(name)?,
        Kind::Skill if is_valid_name(name) => name.to_owned(),
        Kind::Agent if is_valid_name(name) => format!("{name}.md"),
        _ => return Err(invalid("that is not a valid name")),
    };
    Ok(root.join(kind.folder()).join(leaf))
}

fn trash_dir(profile_dir: &Path) -> PathBuf {
    layers::knowledge_dir(profile_dir).join(TRASH)
}

/// Save a copy of the item (when it exists) so a change to it can be undone.
pub fn stash(profile_dir: &Path, layer: &Layer, kind: Kind, name: &str) -> io::Result<Restore> {
    let item = item_path(profile_dir, layer, kind, name)?;
    let leaf = item
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let name = match kind {
        Kind::Agent => leaf.trim_end_matches(".md").to_owned(),
        _ => leaf,
    };
    let mut restore = Restore {
        layer: layer.clone(),
        kind,
        name,
        trash: None,
    };
    let Ok(meta) = fs::symlink_metadata(&item) else {
        return Ok(restore);
    };
    if helm_platform::fs::is_link_like(&meta) {
        return Err(invalid("that item is a link"));
    }
    let id = uuid::Uuid::new_v4().simple().to_string();
    let dir = trash_dir(profile_dir).join(&id);
    helm_core::privfs::create_private_dir_all(&dir)?;
    let copied = if meta.is_dir() {
        let to = dir.join("item");
        fs::create_dir_all(&to).and_then(|()| crate::copy_skill(&item, &to))
    } else {
        helm_core::privfs::read_capped(&item, rules::MAX_RULE_FILE.max(agents::MAX_AGENT_FILE))
            .and_then(|bytes| helm_core::privfs::write_atomic(&dir.join("item"), &bytes))
    };
    if let Err(e) = copied {
        let _ = fs::remove_dir_all(&dir);
        return Err(e);
    }
    restore.trash = Some(id);
    prune(profile_dir);
    Ok(restore)
}

/// Drop the saved copy of a change that was not made after all.
pub fn discard(profile_dir: &Path, restore: &Restore) {
    if let Some(id) = &restore.trash {
        let _ = fs::remove_dir_all(trash_dir(profile_dir).join(id));
    }
}

fn prune(profile_dir: &Path) {
    let Ok(read) = fs::read_dir(trash_dir(profile_dir)) else {
        return;
    };
    let mut all: Vec<(PathBuf, SystemTime)> = read
        .flatten()
        .filter_map(|e| Some((e.path(), e.metadata().ok()?.modified().ok()?)))
        .collect();
    all.sort_by(|a, b| b.1.cmp(&a.1));
    let now = SystemTime::now();
    for (i, (path, at)) in all.into_iter().enumerate() {
        let old = now
            .duration_since(at)
            .map(|d| d > TRASH_AGE)
            .unwrap_or(false);
        if i >= KEEP_TRASH || old {
            let _ = fs::remove_dir_all(path);
        }
    }
}

/// Put an item back as `restore` says: remove what is there now, then restore the saved copy (if
/// there was one).
pub fn revert(profile_dir: &Path, restore: &Restore) -> io::Result<()> {
    let item = item_path(profile_dir, &restore.layer, restore.kind, &restore.name)?;
    let saved = restore
        .trash
        .as_ref()
        .map(|id| trash_dir(profile_dir).join(id).join("item"));
    if saved.as_ref().is_some_and(|s| !s.exists()) {
        return Err(io::Error::new(
            ErrorKind::NotFound,
            "The earlier copy is no longer kept.",
        ));
    }
    if let Ok(meta) = fs::symlink_metadata(&item) {
        if helm_platform::fs::is_link_like(&meta) {
            return Err(invalid("that item is a link"));
        }
        if meta.is_dir() {
            fs::remove_dir_all(&item)?;
        } else {
            fs::remove_file(&item)?;
        }
    }
    if let Some(saved) = saved {
        if saved.is_dir() {
            install_dir(&saved, &item)?;
        } else {
            let bytes = helm_core::privfs::read_capped(
                &saved,
                rules::MAX_RULE_FILE.max(agents::MAX_AGENT_FILE),
            )?;
            if let Some(parent) = item.parent() {
                helm_core::privfs::create_private_dir_all(parent)?;
            }
            helm_core::privfs::write_atomic(&item, &bytes)?;
        }
        discard(profile_dir, restore);
    }
    Ok(())
}

/// The change-ring pair for a change: `before` carries what Undo restores, `after` just the digest
/// the layers must still have.
pub fn ring_values(before_digest: &str, restore: &Restore, after_digest: &str) -> (Value, Value) {
    (
        json!({"digest": before_digest, "restore": restore.to_json()}),
        json!({"digest": after_digest}),
    )
}

/// The extension to register as a [`helm_hosttools::SettingsExtensions`] entry: it lets the ring
/// read the layers' state and undo an agent's change to them.
pub fn settings_extension(profile_dir: &Path) -> Arc<dyn SettingsExtension> {
    Arc::new(Knowledge {
        profile_dir: profile_dir.to_path_buf(),
    })
}

struct Knowledge {
    profile_dir: PathBuf,
}

impl SettingsExtension for Knowledge {
    fn keys(&self) -> Vec<&'static str> {
        vec![KEY]
    }

    fn describe(&self, key: &str) -> Option<Value> {
        (key == KEY).then(|| {
            json!({
                "type": "object",
                "default": {},
                "about": "The rules, skills and agents of every layer. Read only here: change them with app_save_rule, app_remove_rule, app_add_skill and app_remove_skill. A change you make that way is filed so the user can undo it.",
            })
        })
    }

    fn needs_all_settings(&self, _key: &str) -> bool {
        true
    }

    fn read(&self, _tx: &Transaction<'_>, _key: &str) -> Value {
        json!({"digest": digest(&self.profile_dir)})
    }

    fn apply(
        &self,
        _tx: &Transaction<'_>,
        _key: &str,
        _value: &Value,
    ) -> Result<(Value, Value), String> {
        Err("Rules and skills are changed with app_save_rule, app_remove_rule, app_add_skill and app_remove_skill, not here.".into())
    }

    fn restore(&self, _tx: &Transaction<'_>, _key: &str, value: &Value) -> Result<(), String> {
        let restore = value
            .get("restore")
            .and_then(Restore::from_json)
            .ok_or("This change cannot be undone.")?;
        revert(&self.profile_dir, &restore).map_err(|e| e.to_string())
    }

    fn changed_event(&self, _key: &str) -> AppEvent {
        AppEvent::new("knowledge-changed")
    }
}
