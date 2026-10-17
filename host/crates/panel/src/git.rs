//! The hardened git runner for local review.
//!
//! Every call runs the real `git.exe` found on `PATH` (never a shim), with no shell, no console
//! window, and these on every read: no filesystem monitor, no pager, no external diff or text
//! conversion, no colour, no optional locks, literal pathspecs. Variables that could point git at
//! another repository, index, object store or config are removed from its environment (the user's
//! chosen global/system config files are kept). Output is bounded. Reads are killed after a
//! deadline; the one write path (staging) is never killed, so it cannot leave an index lock.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

pub const READ_DEADLINE: Duration = Duration::from_secs(10);
pub const OUTPUT_CAP: usize = 16 * 1024 * 1024;

/// The empty tree's object id, for diffs on a branch with no commits yet.
pub const EMPTY_TREE: &str = "4b825dc642cb6eb9a060e54bf8d69288fbee4904";

/// Variables kept even though they start with `GIT_`: the user's own config file choices.
const KEEP: &[&str] = &[
    "GIT_CONFIG_GLOBAL",
    "GIT_CONFIG_SYSTEM",
    "GIT_CONFIG_NOSYSTEM",
];
/// Other variables that redirect output or diffs.
const DROP_EXTRA: &[&str] = &["PAGER", "LESS", "EDITOR", "VISUAL"];

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GitError {
    NotFound,
    NotARepository,
    Failed(String),
    TimedOut,
    TooLarge,
}

impl GitError {
    pub fn message(&self) -> String {
        match self {
            Self::NotFound => "Git is not installed or not on PATH.".into(),
            Self::NotARepository => "No Git repository".into(),
            Self::Failed(m) => m.clone(),
            Self::TimedOut => "Git did not answer in time.".into(),
            Self::TooLarge => "Git's output was too large.".into(),
        }
    }
}

/// The first `git.exe` (or `git` elsewhere) on `PATH`, skipping `skip` (the wrapper's own folder).
pub fn find_git(skip: Option<&Path>) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    let name = if cfg!(windows) { "git.exe" } else { "git" };
    std::env::split_paths(&path).find_map(|dir| {
        if skip.is_some_and(|s| s == dir) {
            return None;
        }
        let candidate = dir.join(name);
        candidate.is_file().then_some(candidate)
    })
}

/// The hardening flags that precede every subcommand.
pub fn base_args() -> Vec<&'static str> {
    vec![
        "--no-pager",
        "--literal-pathspecs",
        "--no-optional-locks",
        "-c",
        "core.fsmonitor=false",
        "-c",
        "core.pager=cat",
        "-c",
        "diff.external=",
        "-c",
        "color.ui=false",
        "-c",
        "core.quotepath=false",
    ]
}

/// True for a variable git's child must not inherit: anything `GIT_` except the user's config file
/// choices, and the pager and editor variables.
pub fn should_drop(name: &str) -> bool {
    let key = name.to_ascii_uppercase();
    (key.starts_with("GIT_") && !KEEP.contains(&key.as_str())) || DROP_EXTRA.contains(&key.as_str())
}

fn scrubbed(cmd: &mut Command) {
    for (k, _) in std::env::vars_os() {
        if should_drop(&k.to_string_lossy()) {
            cmd.env_remove(&k);
        }
    }
    cmd.env("GIT_TERMINAL_PROMPT", "0");
}

pub struct Git {
    exe: PathBuf,
}

pub struct Output {
    pub status: i32,
    pub stdout: Vec<u8>,
    pub stderr: String,
}

impl Git {
    pub fn locate() -> Result<Self, GitError> {
        find_git(None)
            .map(|exe| Self { exe })
            .ok_or(GitError::NotFound)
    }

    pub fn with_exe(exe: PathBuf) -> Self {
        Self { exe }
    }

    fn command(&self, dir: &Path, args: &[&str]) -> Command {
        let mut cmd = Command::new(&self.exe);
        cmd.current_dir(dir).args(base_args()).args(args);
        scrubbed(&mut cmd);
        cmd.stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            cmd.creation_flags(CREATE_NO_WINDOW);
        }
        cmd
    }

    /// Runs a read. Killed after `deadline`; output beyond the cap fails the call.
    pub fn read(&self, dir: &Path, args: &[&str]) -> Result<Output, GitError> {
        self.run(dir, args, Some(READ_DEADLINE))
    }

    /// Runs the one kind of write (staging): bounded output, never killed.
    pub fn write(&self, dir: &Path, args: &[&str]) -> Result<Output, GitError> {
        self.run(dir, args, None)
    }

    fn run(
        &self,
        dir: &Path,
        args: &[&str],
        deadline: Option<Duration>,
    ) -> Result<Output, GitError> {
        let mut child = self.command(dir, args).spawn().map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                GitError::NotFound
            } else {
                GitError::Failed(e.to_string())
            }
        })?;
        let mut out = child.stdout.take().expect("piped");
        let mut err = child.stderr.take().expect("piped");
        let reader = std::thread::spawn(move || {
            let mut buf = Vec::new();
            let n = (&mut out).take(OUTPUT_CAP as u64 + 1).read_to_end(&mut buf);
            // Drain the rest so git never blocks on a full pipe.
            std::io::copy(&mut out, &mut std::io::sink()).ok();
            n.map(|_| buf)
        });
        let err_reader = std::thread::spawn(move || {
            let mut buf = Vec::new();
            (&mut err).take(64 * 1024).read_to_end(&mut buf).ok();
            std::io::copy(&mut err, &mut std::io::sink()).ok();
            String::from_utf8_lossy(&buf).into_owned()
        });
        let start = Instant::now();
        let status = loop {
            match child.try_wait() {
                Ok(Some(s)) => break s,
                Ok(None) => {}
                Err(e) => return Err(GitError::Failed(e.to_string())),
            }
            if deadline.is_some_and(|d| start.elapsed() > d) {
                let _ = child.kill();
                let _ = child.wait();
                return Err(GitError::TimedOut);
            }
            std::thread::sleep(Duration::from_millis(5));
        };
        let stdout = reader
            .join()
            .map_err(|_| GitError::Failed("git output lost".into()))?
            .map_err(|e| GitError::Failed(e.to_string()))?;
        let stderr = err_reader.join().unwrap_or_default();
        if stdout.len() > OUTPUT_CAP {
            return Err(GitError::TooLarge);
        }
        Ok(Output {
            status: status.code().unwrap_or(-1),
            stdout,
            stderr,
        })
    }

    /// The repository top containing `dir`, or `NotARepository`.
    pub fn toplevel(&self, dir: &Path) -> Result<PathBuf, GitError> {
        let o = self.read(dir, &["rev-parse", "--show-toplevel"])?;
        if o.status != 0 {
            return Err(GitError::NotARepository);
        }
        let s = String::from_utf8_lossy(&o.stdout)
            .trim()
            .replace('/', std::path::MAIN_SEPARATOR_STR);
        if s.is_empty() {
            return Err(GitError::NotARepository);
        }
        Ok(PathBuf::from(s))
    }

    /// The current branch name, or None (detached or unborn).
    pub fn branch(&self, top: &Path) -> Option<String> {
        let o = self
            .read(top, &["symbolic-ref", "--quiet", "--short", "HEAD"])
            .ok()?;
        (o.status == 0)
            .then(|| String::from_utf8_lossy(&o.stdout).trim().to_owned())
            .filter(|s| !s.is_empty())
    }

    /// True when HEAD names a commit (false on an unborn branch).
    pub fn has_head(&self, top: &Path) -> bool {
        self.read(top, &["rev-parse", "--verify", "--quiet", "HEAD"])
            .map(|o| o.status == 0)
            .unwrap_or(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_call_carries_the_hardening_flags() {
        let a = base_args().join(" ");
        for flag in [
            "--no-pager",
            "--literal-pathspecs",
            "--no-optional-locks",
            "core.fsmonitor=false",
            "diff.external=",
            "color.ui=false",
        ] {
            assert!(a.contains(flag), "{flag}");
        }
    }

    #[test]
    fn redirecting_variables_are_removed_but_config_file_choices_kept() {
        for v in [
            "GIT_DIR",
            "git_work_tree",
            "GIT_INDEX_FILE",
            "GIT_OBJECT_DIRECTORY",
            "GIT_EXTERNAL_DIFF",
            "GIT_CONFIG",
            "GIT_CONFIG_PARAMETERS",
            "PAGER",
            "EDITOR",
        ] {
            assert!(should_drop(v), "{v}");
        }
        for v in [
            "GIT_CONFIG_GLOBAL",
            "GIT_CONFIG_SYSTEM",
            "PATH",
            "USERPROFILE",
            "HOME",
        ] {
            assert!(!should_drop(v), "{v}");
        }
    }
}
