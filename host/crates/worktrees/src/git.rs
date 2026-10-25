//! A small, read-mostly git runner. It mirrors the review panel's hardened one (the real
//! `git.exe` found on `PATH`, no shell, no console window, variables that redirect git removed)
//! without its dependencies, because the git stand-in binary uses this crate too.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// What a git call produced.
#[derive(Debug, Clone)]
pub struct Out {
    pub code: i32,
    pub stdout: String,
    pub stderr: String,
}

impl Out {
    pub fn ok(&self) -> bool {
        self.code == 0
    }
    pub fn text(&self) -> &str {
        self.stdout.trim()
    }
}

/// The first `git.exe` (`git` elsewhere) in `path_var` outside `skip`.
pub fn find_git_in(path_var: &std::ffi::OsStr, skip: Option<&Path>) -> Option<PathBuf> {
    let name = if cfg!(windows) { "git.exe" } else { "git" };
    std::env::split_paths(path_var).find_map(|dir| {
        if skip
            .is_some_and(|s| crate::rules::same_path(&s.to_string_lossy(), &dir.to_string_lossy()))
        {
            return None;
        }
        let c = dir.join(name);
        c.is_file().then_some(c)
    })
}

/// The first real git on this process's `PATH`, skipping `skip` (a stand-in's own folder).
pub fn find_git(skip: Option<&Path>) -> Option<PathBuf> {
    find_git_in(&std::env::var_os("PATH")?, skip)
}

#[derive(Debug, Clone)]
pub struct Git {
    exe: PathBuf,
}

impl Git {
    pub fn new(exe: PathBuf) -> Self {
        Self { exe }
    }

    pub fn locate() -> Option<Self> {
        find_git(None).map(Self::new)
    }

    pub fn exe(&self) -> &Path {
        &self.exe
    }

    /// Run `git <args>` in `dir`. `None` when git cannot be started.
    pub fn run(&self, dir: &Path, args: &[&str]) -> Option<Out> {
        let mut cmd = Command::new(&self.exe);
        cmd.current_dir(dir)
            .args([
                "--no-pager",
                "--no-optional-locks",
                "-c",
                "core.fsmonitor=false",
            ])
            .args(args);
        for (k, _) in std::env::vars_os() {
            let key = k.to_string_lossy().to_ascii_uppercase();
            let keep = [
                "GIT_CONFIG_GLOBAL",
                "GIT_CONFIG_SYSTEM",
                "GIT_CONFIG_NOSYSTEM",
            ];
            if key.starts_with("GIT_") && !keep.contains(&key.as_str()) {
                cmd.env_remove(&k);
            }
        }
        cmd.env("GIT_TERMINAL_PROMPT", "0")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            cmd.creation_flags(0x0800_0000);
        }
        let o = cmd.output().ok()?;
        Some(Out {
            code: o.status.code().unwrap_or(-1),
            stdout: String::from_utf8_lossy(&o.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&o.stderr).into_owned(),
        })
    }
}
