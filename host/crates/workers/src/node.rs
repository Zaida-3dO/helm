//! Finding Node for the worker (spawn real executables, never `.cmd` shims).
//!
//! Order: `HELM_NODE_PATH`, then `node.exe` (`node` on Linux) on `PATH`. A wrapper script is
//! refused, and so is a Node older than 22.19 (the worker runs TypeScript directly, which needs
//! type stripping).
//!
//! The version check runs `node --version` once per executable: the answer is kept, keyed by the
//! path and its modification time, so an upgraded Node is checked again (it ran
//! on every worker start).

use std::collections::HashMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;
use std::time::SystemTime;

/// `node --version` answers by path, with the file's modification time when it was asked.
static VERSIONS: Mutex<Option<HashMap<PathBuf, (SystemTime, String)>>> = Mutex::new(None);

fn modified(path: &Path) -> Option<SystemTime> {
    std::fs::metadata(path).and_then(|m| m.modified()).ok()
}

/// The version for `path`, from the cache while the file is unchanged.
fn cached_version(path: &Path) -> Result<String, NodeError> {
    let stamp = modified(path);
    if let Some(stamp) = stamp {
        let cache = VERSIONS.lock().unwrap();
        if let Some((at, v)) = cache.as_ref().and_then(|c| c.get(path)) {
            if *at == stamp {
                return Ok(v.clone());
            }
        }
    }
    let v = node_version(path)?;
    if let Some(stamp) = stamp {
        remember(path, stamp, &v);
    }
    Ok(v)
}

fn remember(path: &Path, stamp: SystemTime, version: &str) {
    VERSIONS
        .lock()
        .unwrap()
        .get_or_insert_with(HashMap::new)
        .insert(path.to_path_buf(), (stamp, version.to_owned()));
}

pub const MIN_NODE: (u32, u32) = (22, 19);

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum NodeError {
    #[error("Node.js was not found. Install Node {}.{} or newer, or set HELM_NODE_PATH.", MIN_NODE.0, MIN_NODE.1)]
    NotFound,
    #[error("{0} is a wrapper script; Helm needs the real node executable (set HELM_NODE_PATH).")]
    Wrapper(String),
    #[error("Node {found} is too old; Helm needs {}.{} or newer.", MIN_NODE.0, MIN_NODE.1)]
    TooOld { found: String },
    #[error("could not run {path}: {why}")]
    Unusable { path: String, why: String },
}

#[derive(Debug, Clone)]
pub struct Node {
    pub path: PathBuf,
    pub version: String,
}

/// What the lookup reads, so tests can supply it.
#[derive(Debug, Clone, Default)]
pub struct NodeLookup {
    pub explicit: Option<OsString>,
    pub path_var: Option<OsString>,
}

impl NodeLookup {
    /// Look Node up on the PATH the worker will get (the registry PATH), so a Node
    /// installed after Helm started is still found. `HELM_NODE_PATH` still comes from Helm's own
    /// environment.
    pub fn for_worker(env: &crate::env::EnvMap) -> Self {
        Self {
            explicit: std::env::var_os("HELM_NODE_PATH").filter(|v| !v.is_empty()),
            path_var: env.get("PATH").map(OsString::from),
        }
    }

    pub fn from_env() -> Self {
        Self {
            explicit: std::env::var_os("HELM_NODE_PATH").filter(|v| !v.is_empty()),
            path_var: std::env::var_os("PATH"),
        }
    }
}

const NODE_NAME: &str = if cfg!(windows) { "node.exe" } else { "node" };

fn is_wrapper(p: &Path) -> bool {
    matches!(
        p.extension()
            .and_then(|e| e.to_str())
            .map(str::to_ascii_lowercase)
            .as_deref(),
        Some("cmd" | "bat" | "ps1" | "sh" | "vbs" | "js")
    )
}

/// Locate a usable Node and check its version.
pub fn resolve_node(lookup: &NodeLookup) -> Result<Node, NodeError> {
    let path = match &lookup.explicit {
        Some(p) => {
            let p = PathBuf::from(p);
            if is_wrapper(&p) {
                return Err(NodeError::Wrapper(p.display().to_string()));
            }
            if !p.is_file() {
                return Err(NodeError::NotFound);
            }
            p
        }
        None => {
            let dirs = lookup.path_var.clone().unwrap_or_default();
            std::env::split_paths(&dirs)
                .map(|d| d.join(NODE_NAME))
                .find(|p| p.is_file())
                .ok_or(NodeError::NotFound)?
        }
    };
    let version = cached_version(&path)?;
    if !version_ok(&version) {
        return Err(NodeError::TooOld { found: version });
    }
    Ok(Node { path, version })
}

fn node_version(path: &Path) -> Result<String, NodeError> {
    let mut cmd = Command::new(path);
    cmd.arg("--version");
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000);
    }
    let out = cmd.output().map_err(|e| NodeError::Unusable {
        path: path.display().to_string(),
        why: e.to_string(),
    })?;
    let v = String::from_utf8_lossy(&out.stdout).trim().to_owned();
    if !out.status.success() || v.is_empty() {
        return Err(NodeError::Unusable {
            path: path.display().to_string(),
            why: format!("`--version` failed ({})", out.status),
        });
    }
    Ok(v)
}

/// `v24.14.0` style versions at or above [`MIN_NODE`].
pub fn version_ok(v: &str) -> bool {
    let mut parts = v.trim().trim_start_matches('v').split('.');
    let major: u32 = parts.next().and_then(|s| s.parse().ok()).unwrap_or(0);
    let minor: u32 = parts.next().and_then(|s| s.parse().ok()).unwrap_or(0);
    (major, minor) >= MIN_NODE
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_are_compared_by_major_then_minor() {
        assert!(version_ok("v24.14.0"));
        assert!(version_ok("v22.19.0"));
        assert!(!version_ok("v22.18.9"));
        assert!(!version_ok("v20.30.0"));
        assert!(!version_ok("garbage"));
    }

    #[test]
    fn a_wrapper_script_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let shim = dir.path().join("node.cmd");
        std::fs::write(&shim, "@echo off").unwrap();
        let r = resolve_node(&NodeLookup {
            explicit: Some(shim.into_os_string()),
            path_var: None,
        });
        assert!(matches!(r, Err(NodeError::Wrapper(_))));
    }

    #[test]
    fn nothing_on_path_is_not_found() {
        let dir = tempfile::tempdir().unwrap();
        let r = resolve_node(&NodeLookup {
            explicit: None,
            path_var: Some(dir.path().as_os_str().to_owned()),
        });
        assert_eq!(r.unwrap_err(), NodeError::NotFound);
    }

    #[test]
    fn the_version_is_kept_until_the_file_changes() {
        let dir = tempfile::tempdir().unwrap();
        // Not a program: running it would fail, so a hit can only come from the cache.
        let fake = dir.path().join(NODE_NAME);
        std::fs::write(&fake, "not node").unwrap();
        let lookup = NodeLookup {
            explicit: Some(fake.clone().into_os_string()),
            path_var: None,
        };
        remember(&fake, modified(&fake).unwrap(), "v24.14.0");
        assert_eq!(resolve_node(&lookup).unwrap().version, "v24.14.0");
        let later = modified(&fake).unwrap() + std::time::Duration::from_secs(5);
        std::fs::File::options()
            .write(true)
            .open(&fake)
            .unwrap()
            .set_modified(later)
            .unwrap();
        assert!(
            matches!(resolve_node(&lookup), Err(NodeError::Unusable { .. })),
            "a changed file is checked again"
        );
    }

    #[test]
    fn the_installed_node_resolves() {
        let n = resolve_node(&NodeLookup::from_env()).expect("node is installed for the tests");
        assert!(version_ok(&n.version), "{}", n.version);
    }
}
