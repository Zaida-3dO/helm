//! Runtime status for Settings → Runtime: which Node and
//! which Claude Code executable a worker would use, their versions, and what is wrong if a worker
//! could not start.
//!
//! Node is found exactly as a worker launch finds it ([`crate::node::resolve_node`]). Claude Code is
//! found the way the worker's resolver does it (`worker/src/drivers/claude/resolve.ts`):
//! the `executable` in `claude-runtime.json`, then `HELM_CLAUDE_PATH`, then `claude(.exe)` on
//! `PATH`, then `~/.local/bin`, then the newest VS Code extension's native binary. Wrapper scripts
//! are refused. The version comes from `claude --version` (no prompt, no network), bounded by
//! [`VERSION_TIMEOUT`].

use crate::env::EnvMap;
use crate::launch::{ClaudeRuntime, TestClaude};
use crate::node::{resolve_node, NodeLookup};
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::time::Duration;

pub const VERSION_TIMEOUT: Duration = Duration::from_secs(5);

/// One component's status.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Found {
    pub path: Option<String>,
    /// Where it was found: `HELM_NODE_PATH`, `PATH`, `claude-runtime.json`, `HELM_CLAUDE_PATH`,
    /// `local-bin`, `vscode-extension`, or `HELM_TEST_CLAUDE` (debug builds' fake).
    pub source: Option<String>,
    pub version: Option<String>,
    /// What stops a worker from using it, in words for the user.
    pub problem: Option<String>,
}

/// Node, as a worker launch would resolve it.
pub fn node(env: &EnvMap) -> Found {
    let lookup = NodeLookup::for_worker(env);
    let source = if lookup.explicit.is_some() {
        "HELM_NODE_PATH"
    } else {
        "PATH"
    };
    match resolve_node(&lookup) {
        Ok(n) => Found {
            path: Some(n.path.display().to_string()),
            source: Some(source.into()),
            version: Some(n.version),
            problem: None,
        },
        Err(e) => Found {
            problem: Some(e.to_string()),
            ..Default::default()
        },
    }
}

const CLAUDE_NAME: &str = if cfg!(windows) {
    "claude.exe"
} else {
    "claude"
};

fn is_wrapper(p: &Path) -> bool {
    matches!(
        p.extension()
            .and_then(|e| e.to_str())
            .map(str::to_ascii_lowercase)
            .as_deref(),
        Some("cmd" | "bat" | "ps1" | "sh" | "vbs" | "js")
    )
}

/// Dotted numeric versions compare by number ("2.1.301" > "2.1.295").
fn version_key(v: &str) -> Vec<u64> {
    v.split(['.', '-'])
        .map(|p| p.parse().unwrap_or(0))
        .collect()
}

/// Find Claude Code the way the worker does. `home` is the user's home folder.
pub fn resolve_claude(
    env: &EnvMap,
    executable: &str,
    home: Option<&Path>,
) -> Result<(PathBuf, &'static str), String> {
    let check = |p: PathBuf, label: &str| -> Result<PathBuf, String> {
        if is_wrapper(&p) {
            return Err(format!(
                "{label} points at a wrapper script ({}); give the real claude executable",
                p.display()
            ));
        }
        if !p.is_file() {
            return Err(format!("{label} does not name a file: {}", p.display()));
        }
        Ok(p)
    };
    if !executable.trim().is_empty() {
        return check(
            PathBuf::from(executable.trim()),
            "The Claude Code executable",
        )
        .map(|p| (p, "claude-runtime.json"));
    }
    if let Some(p) = env.get("HELM_CLAUDE_PATH").filter(|p| !p.trim().is_empty()) {
        return check(PathBuf::from(p), "HELM_CLAUDE_PATH").map(|p| (p, "HELM_CLAUDE_PATH"));
    }
    if let Some(path) = env.get("PATH") {
        for dir in std::env::split_paths(path) {
            let c = dir.join(CLAUDE_NAME);
            if c.is_file() {
                return Ok((c, "PATH"));
            }
        }
    }
    if let Some(home) = home {
        let local = home.join(".local").join("bin").join(CLAUDE_NAME);
        if local.is_file() {
            return Ok((local, "local-bin"));
        }
        let mut best: Option<(Vec<u64>, PathBuf)> = None;
        for entry in std::fs::read_dir(home.join(".vscode").join("extensions"))
            .into_iter()
            .flatten()
            .flatten()
        {
            let name = entry.file_name().to_string_lossy().into_owned();
            let Some(rest) = name.strip_prefix("anthropic.claude-code-") else {
                continue;
            };
            let version: String = rest
                .chars()
                .take_while(|c| c.is_ascii_digit() || *c == '.')
                .collect();
            let file = entry
                .path()
                .join("resources")
                .join("native-binary")
                .join(CLAUDE_NAME);
            if version.is_empty() || !file.is_file() {
                continue;
            }
            let key = version_key(&version);
            if best.as_ref().is_none_or(|(k, _)| key > *k) {
                best = Some((key, file));
            }
        }
        if let Some((_, file)) = best {
            return Ok((file, "vscode-extension"));
        }
    }
    Err(
        "Claude Code was not found: install it, put claude on PATH, or give its executable below."
            .into(),
    )
}

/// `<path> --version`, first line, at most [`VERSION_TIMEOUT`]. `None` when it did not answer.
pub fn claude_version(path: &Path) -> Option<String> {
    let mut cmd = std::process::Command::new(path);
    cmd.arg("--version")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000);
    }
    let mut child = cmd.spawn().ok()?;
    let mut out = child.stdout.take()?;
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut s = String::new();
        let _ = std::io::Read::read_to_string(&mut out, &mut s);
        let _ = tx.send(s);
    });
    let text = match rx.recv_timeout(VERSION_TIMEOUT) {
        Ok(s) => s,
        Err(_) => {
            let _ = child.kill();
            return None;
        }
    };
    let _ = child.wait();
    let line = text.lines().map(str::trim).find(|l| !l.is_empty())?;
    // "2.1.301 (Claude Code)" → "2.1.301"
    Some(
        line.split(" (")
            .next()
            .unwrap_or(line)
            .chars()
            .take(64)
            .collect(),
    )
}

/// Claude Code, as the next worker would start it. In a debug build with the fake-claude seam the
/// fake is reported (and not run).
pub fn claude(
    env: &EnvMap,
    runtime: &ClaudeRuntime,
    test: Option<&TestClaude>,
    home: Option<&Path>,
) -> Found {
    if let Some(t) = test {
        return Found {
            path: Some(t.command.clone()),
            source: Some("HELM_TEST_CLAUDE".into()),
            version: None,
            problem: None,
        };
    }
    match resolve_claude(env, &runtime.executable, home) {
        Ok((path, source)) => {
            let version = claude_version(&path);
            Found {
                problem: version
                    .is_none()
                    .then(|| format!("{} did not answer --version.", path.display())),
                path: Some(path.display().to_string()),
                source: Some(source.into()),
                version,
            }
        }
        Err(problem) => Found {
            problem: Some(problem),
            ..Default::default()
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env_with(path: &Path) -> EnvMap {
        let mut e = EnvMap::default();
        e.set("PATH", path.display().to_string());
        e
    }

    #[test]
    fn the_runtime_file_wins_then_path_then_local_bin_then_the_newest_extension() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("home");
        let on_path = dir.path().join("bin");
        std::fs::create_dir_all(&on_path).unwrap();
        let ext = |v: &str| {
            let d = home
                .join(".vscode/extensions")
                .join(format!("anthropic.claude-code-{v}-win32-x64"))
                .join("resources/native-binary");
            std::fs::create_dir_all(&d).unwrap();
            std::fs::write(d.join(CLAUDE_NAME), "x").unwrap();
            d.join(CLAUDE_NAME)
        };
        let _old = ext("2.1.9");
        let newest = ext("2.1.10");
        let empty = EnvMap::default();
        assert_eq!(
            resolve_claude(&empty, "", Some(&home)).unwrap(),
            (newest, "vscode-extension"),
            "by version number, not text"
        );
        let local = home.join(".local/bin");
        std::fs::create_dir_all(&local).unwrap();
        std::fs::write(local.join(CLAUDE_NAME), "x").unwrap();
        assert_eq!(
            resolve_claude(&empty, "", Some(&home)).unwrap().1,
            "local-bin"
        );
        std::fs::write(on_path.join(CLAUDE_NAME), "x").unwrap();
        assert_eq!(
            resolve_claude(&env_with(&on_path), "", Some(&home))
                .unwrap()
                .1,
            "PATH"
        );
        let explicit = dir.path().join("mine").join(CLAUDE_NAME);
        std::fs::create_dir_all(explicit.parent().unwrap()).unwrap();
        std::fs::write(&explicit, "x").unwrap();
        assert_eq!(
            resolve_claude(
                &env_with(&on_path),
                &explicit.to_string_lossy(),
                Some(&home)
            )
            .unwrap(),
            (explicit, "claude-runtime.json")
        );
    }

    #[test]
    fn wrappers_and_missing_files_are_problems() {
        let dir = tempfile::tempdir().unwrap();
        let shim = dir.path().join("claude.cmd");
        std::fs::write(&shim, "@echo off").unwrap();
        let e = resolve_claude(&EnvMap::default(), &shim.to_string_lossy(), None).unwrap_err();
        assert!(e.contains("wrapper script"), "{e}");
        let e = resolve_claude(&EnvMap::default(), "C:/nowhere/claude.exe", None).unwrap_err();
        assert!(e.contains("does not name a file"), "{e}");
        let e = resolve_claude(&EnvMap::default(), "", Some(dir.path())).unwrap_err();
        assert!(e.contains("not found"), "{e}");
        let f = claude(
            &EnvMap::default(),
            &ClaudeRuntime::default(),
            None,
            Some(dir.path()),
        );
        assert!(f.path.is_none() && f.problem.is_some(), "{f:?}");
    }

    #[test]
    fn the_test_seam_is_reported_not_run_and_node_resolves() {
        let t = TestClaude {
            command: "node.exe".into(),
            args: vec![],
        };
        let f = claude(
            &EnvMap::default(),
            &ClaudeRuntime::default(),
            Some(&t),
            None,
        );
        assert_eq!(f.source.as_deref(), Some("HELM_TEST_CLAUDE"));
        let n = node(&EnvMap::from_process());
        assert!(n.version.is_some() && n.problem.is_none(), "{n:?}");
    }

    #[test]
    fn a_version_answer_is_read_from_the_first_line() {
        // Node answers --version like claude does ("v24.14.0"): a stand-in executable.
        let n = crate::node::resolve_node(&NodeLookup::from_env()).unwrap();
        assert_eq!(claude_version(&n.path), Some(n.version));
    }
}
