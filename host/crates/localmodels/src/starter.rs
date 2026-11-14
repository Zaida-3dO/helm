//! Starting a Strata server that is not running, when its settings allow it, and stopping one
//! when the user unloads local models.
//!
//! The install is the ordinary Python one: `strata.py` run by the interpreter the install ships
//! (or its own venv), never a command-file shim. It is started the way a detached background
//! service is started — its own process group, no window, no console — so it outlives Helm
//! entirely, and Helm never reads or logs a word of its output: stdin, stdout and stderr all go
//! nowhere.
//!
//! The service only starts a Strata its own checks found unreachable, at most one start at a
//! time. It stops one only when the user (or a game on their list) unloads local models: the
//! install's own `launch --stop`, which closes every server it launched. Neither is ever done when
//! the run has the test override in effect: in tests and checks the starter installed is a
//! stand-in, so the real one never runs there.

use crate::settings::{Server, DEFAULT_PRESET};
use std::path::{Path, PathBuf};

/// What the service asks when a Strata server it needs is not answering. An `Ok` means the
/// launch was started; the caller then waits for the server to answer its health check. The
/// handle (where one exists) lets that wait notice a launcher that died at once.
pub trait StrataStarter: Send + Sync {
    fn start(&self, server: &Server) -> Result<Option<std::process::Child>, String>;
    /// Stop the server's process, waiting for it to close.
    fn stop(&self, server: &Server) -> Result<Stopped, String>;
}

/// What a stop did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stopped {
    /// The install's stop ran to the end.
    Done,
    /// No install was found to run the stop from; the server, if any, keeps running.
    NoInstall,
}

/// How long the install's stop gets before it is killed. It waits for each server it launched
/// to close, which can take a few minutes for a large model.
pub const STOP_LIMIT: std::time::Duration = std::time::Duration::from_secs(200);

/// The standard install folder: the terminal install's current version.
pub fn default_folder() -> Option<PathBuf> {
    let local = std::env::var_os("LOCALAPPDATA")?;
    Some(
        Path::new(&local)
            .join("Strata")
            .join("terminal")
            .join("current"),
    )
}

/// The folder to start this server from: the one in its settings, or the standard one.
pub fn folder_for(server: &Server) -> Option<PathBuf> {
    match server
        .folder
        .as_deref()
        .map(str::trim)
        .filter(|f| !f.is_empty())
    {
        Some(f) => Some(PathBuf::from(f)),
        None => default_folder(),
    }
}

/// The preset to launch with: the one in its settings, or the default.
pub fn preset_for(server: &Server) -> &str {
    server
        .preset
        .as_deref()
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .unwrap_or(DEFAULT_PRESET)
}

/// The Python that ships in the install folder: the bundled one first, then its own venv.
/// `None` when the folder holds neither, which is how a wrong folder is noticed.
pub fn python_in(folder: &Path) -> Option<PathBuf> {
    for parts in [
        ["python", "python.exe"].as_slice(),
        [".venv", "Scripts", "python.exe"].as_slice(),
    ] {
        let p = parts
            .iter()
            .fold(folder.to_path_buf(), |acc, x| acc.join(x));
        if p.is_file() {
            return Some(p);
        }
    }
    None
}

/// The launch: the folder's `strata.py`, the chosen preset, no prompts, no terminal window.
/// It never carries a key-displaying switch — the server keeps its own key in its own store —
/// and never a stop, restart or unload verb.
pub fn launch_plan(python: &Path, folder: &Path, preset: &str) -> (PathBuf, Vec<String>) {
    let script = folder.join("strata.py");
    let args = vec![
        script.to_string_lossy().into_owned(),
        "launch".into(),
        "--preset".into(),
        preset.to_owned(),
        "--yes".into(),
        "--no-tui".into(),
    ];
    (python.to_path_buf(), args)
}

/// The stop: the folder's `strata.py`, `launch --stop`, which asks nothing.
pub fn stop_plan(python: &Path, folder: &Path) -> (PathBuf, Vec<String>) {
    let script = folder.join("strata.py");
    let args = vec![
        script.to_string_lossy().into_owned(),
        "launch".into(),
        "--stop".into(),
    ];
    (python.to_path_buf(), args)
}

pub struct ProcessStarter;

impl StrataStarter for ProcessStarter {
    fn start(&self, server: &Server) -> Result<Option<std::process::Child>, String> {
        let folder = folder_for(server)
            .ok_or("the Strata folder is not set and the standard one cannot be found")?;
        let python = python_in(&folder)
            .ok_or_else(|| format!("no Strata install was found in {}", folder.display()))?;
        let (program, args) = launch_plan(&python, &folder, preset_for(server));
        let mut cmd = std::process::Command::new(&program);
        cmd.args(args)
            .current_dir(&folder)
            .env("PYTHONUTF8", "1")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            const DETACHED_PROCESS: u32 = 0x0000_0008;
            const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
            cmd.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP);
        }
        cmd.spawn()
            .map(Some)
            .map_err(|e| format!("Strata could not be started: {e}"))
    }

    fn stop(&self, server: &Server) -> Result<Stopped, String> {
        let Some(folder) = folder_for(server) else {
            return Ok(Stopped::NoInstall);
        };
        let Some(python) = python_in(&folder) else {
            return Ok(Stopped::NoInstall);
        };
        let (program, args) = stop_plan(&python, &folder);
        let child = crate::task::quiet(&program)
            .args(args)
            .current_dir(&folder)
            .env("PYTHONUTF8", "1")
            .spawn()
            .map_err(|e| format!("its stop could not be run: {e}"))?;
        match crate::task::wait_or_kill(child, STOP_LIMIT) {
            Ok(s) if s.success() => Ok(Stopped::Done),
            Ok(s) => Err(match s.code() {
                Some(c) => format!("its stop exited with code {c}"),
                None => "its stop did not finish normally".to_owned(),
            }),
            Err(e) => Err(format!("its stop failed: {e}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::default_servers;
    use helm_chats::types::LocalServerKind;

    fn strata(folder: Option<&str>, preset: Option<&str>) -> Server {
        let mut s = default_servers()
            .into_iter()
            .find(|s| s.kind == LocalServerKind::Strata)
            .unwrap();
        s.folder = folder.map(str::to_owned);
        s.preset = preset.map(str::to_owned);
        s
    }

    #[test]
    fn the_folder_and_preset_come_from_the_settings_or_the_defaults() {
        assert_eq!(preset_for(&strata(None, None)), DEFAULT_PRESET);
        assert_eq!(preset_for(&strata(None, Some(" Solo "))), "Solo");
        assert_eq!(preset_for(&strata(None, Some("Fast"))), "Fast");
        let given = strata(Some(" D:/Strata/terminal/current "), None);
        assert_eq!(
            folder_for(&given),
            Some(PathBuf::from("D:/Strata/terminal/current"))
        );
        // With no folder named, the standard one, when the machine says where it is.
        if let Some(local) = std::env::var_os("LOCALAPPDATA") {
            assert_eq!(
                folder_for(&strata(None, None)),
                Some(
                    Path::new(&local)
                        .join("Strata")
                        .join("terminal")
                        .join("current")
                )
            );
        }
    }

    #[test]
    fn the_install_is_read_by_its_own_python_and_launched_without_shims() {
        let dir = tempfile::tempdir().unwrap();
        let folder = dir.path();
        // Neither interpreter: the folder is refused as not an install.
        assert_eq!(python_in(folder), None);
        // The venv alone is enough.
        std::fs::create_dir_all(folder.join(".venv/Scripts")).unwrap();
        std::fs::write(folder.join(".venv/Scripts/python.exe"), "").unwrap();
        assert_eq!(
            python_in(folder),
            Some(folder.join(".venv/Scripts/python.exe"))
        );
        // And the bundled one wins once it is there.
        std::fs::create_dir_all(folder.join("python")).unwrap();
        std::fs::write(folder.join("python/python.exe"), "").unwrap();
        let python = python_in(folder).unwrap();
        assert_eq!(python, folder.join("python/python.exe"));

        let (program, args) = launch_plan(&python, folder, "Solo");
        assert_eq!(program, python);
        assert_eq!(
            args,
            [
                folder.join("strata.py").to_string_lossy(),
                "launch".into(),
                "--preset".into(),
                "Solo".into(),
                "--yes".into(),
                "--no-tui".into(),
            ]
        );
        let joined = args.join(" ");
        assert!(
            !joined.contains("--show-key"),
            "the key stays on the server"
        );
        assert!(!joined.contains(".cmd"), "no command-file shims");
        assert!(
            !joined.contains("stop") && !joined.contains("unload"),
            "starting never says stop"
        );
        // The stop is the same install's own verb, by the same interpreter, and asks nothing.
        let (program, args) = stop_plan(&python, folder);
        assert_eq!(program, python);
        assert_eq!(
            args,
            [
                folder.join("strata.py").to_string_lossy(),
                "launch".into(),
                "--stop".into(),
            ]
        );
        assert!(
            program.to_string_lossy().ends_with("python.exe"),
            "the real interpreter, started directly"
        );
    }
}
