//! Putting the git stand-in on a chat's `PATH`, only while some repository has worktrees off.
//!
//! With no repository off a launch is untouched: no `PATH` change, no variables. With one off,
//! the chat's `PATH` starts with a folder in the profile that holds the stand-in as `git.exe`,
//! and two variables name the real git and the off list.
//!
//! A `PATH` prefix alone does not hold in a bash login shell: Git for Windows' profile rebuilds
//! `PATH` with its own `mingw64/bin` (the real `git.exe`) in front of the inherited one, and Claude
//! Code's Bash tool starts bash as a login shell. So the launch also exports a `git` shell function
//! that calls the stand-in. Bash imports exported functions at start-up, login or not, and a
//! function wins over any `PATH` lookup. PowerShell, `cmd` and programs that start `git` by name
//! use the `PATH` prefix. Not covered: `command git`, and a git called by its full path.

use helm_workers::contrib::{LaunchContributor, LaunchCtx, LaunchExtras};
use helm_worktrees::{git::find_git, offlist};
use std::path::{Path, PathBuf};

/// The folder in the profile holding the stand-in.
pub const BIN_DIR: &str = "agent-bin";
const SHIM_NAME: &str = "helm-git.exe";

pub struct GitGuard {
    profile_dir: PathBuf,
    /// Where the stand-in program is built or installed; found beside the app when `None`.
    shim_source: Option<PathBuf>,
}

impl GitGuard {
    pub fn new(profile_dir: PathBuf) -> Self {
        // The folder goes on `PATH`, where Windows' verbatim prefix does not work.
        let plain = profile_dir.to_string_lossy();
        let profile_dir = match plain.strip_prefix(r"\\?\") {
            Some(rest) if rest.as_bytes().get(1) == Some(&b':') => PathBuf::from(rest),
            _ => profile_dir,
        };
        Self {
            profile_dir,
            shim_source: None,
        }
    }

    pub fn with_shim_source(mut self, p: PathBuf) -> Self {
        self.shim_source = Some(p);
        self
    }

    fn source(&self) -> Option<PathBuf> {
        let name = if cfg!(windows) { SHIM_NAME } else { "helm-git" };
        self.shim_source
            .clone()
            .or_else(|| std::env::var_os("HELM_GIT_SHIM").map(PathBuf::from))
            .or_else(|| Some(std::env::current_exe().ok()?.parent()?.join(name)))
            .filter(|p| p.is_file())
    }

    /// Copy the stand-in to `<profile>/agent-bin/git.exe` unless it is already the same file
    /// (same size, not older). Returns the folder.
    fn install(&self) -> Result<PathBuf, String> {
        let src = self
            .source()
            .ok_or("the git stand-in program was not found next to Helm")?;
        let dir = self.profile_dir.join(BIN_DIR);
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        let dest = dir.join(if cfg!(windows) { "git.exe" } else { "git" });
        let (sm, dm) = (std::fs::metadata(&src), std::fs::metadata(&dest));
        let same = match (&sm, &dm) {
            (Ok(s), Ok(d)) => {
                s.len() == d.len()
                    && matches!((s.modified(), d.modified()), (Ok(a), Ok(b)) if b >= a)
            }
            _ => false,
        };
        if !same {
            let tmp = dir.join("git.tmp");
            std::fs::copy(&src, &tmp).map_err(|e| e.to_string())?;
            if let Err(e) = std::fs::rename(&tmp, &dest) {
                let _ = std::fs::remove_file(&tmp);
                // A running chat may hold the old copy; keep using it.
                if dest.is_file() {
                    log::warn!("the git stand-in was not updated ({e}); the old copy stays");
                } else {
                    return Err(e.to_string());
                }
            }
        }
        Ok(dir)
    }
}

/// The variable that makes bash define `git` as a function in every shell it starts.
pub const BASH_GIT_FUNCTION_VAR: &str = "BASH_FUNC_git%%";

/// A Windows path as bash writes it: `C:\a\b` becomes `/c/a/b`. Anything else is left alone.
fn bash_path(p: &Path) -> String {
    let s = p.to_string_lossy();
    match s.as_bytes() {
        [d, b':', b'\\' | b'/', ..] if d.is_ascii_alphabetic() => {
            let rest = s[3..].replace('\\', "/");
            format!("/{}/{rest}", d.to_ascii_lowercase() as char)
        }
        _ => s.into_owned(),
    }
}

/// The value of [`BASH_GIT_FUNCTION_VAR`]: a function that runs the stand-in at `shim`.
pub fn bash_git_function(shim: &Path) -> String {
    let quoted = bash_path(shim).replace('\'', r"'\''");
    format!("() {{  '{quoted}' \"$@\"\n}}")
}

impl LaunchContributor for GitGuard {
    fn contribute(&self, _ctx: &LaunchCtx<'_>) -> LaunchExtras {
        let list = self.profile_dir.join(offlist::FILE_NAME);
        if offlist::read(&list).is_empty() {
            return LaunchExtras::default();
        }
        let dir = match self.install() {
            Ok(d) => d,
            Err(e) => {
                log::warn!(
                    "worktrees are off for a repository but the git stand-in is unavailable: {e}"
                );
                return LaunchExtras::default();
            }
        };
        let mut extras = LaunchExtras {
            path_prepend: vec![dir.clone()],
            env: vec![
                (
                    "HELM_WORKTREE_OFF_LIST".into(),
                    list.to_string_lossy().into_owned(),
                ),
                (
                    BASH_GIT_FUNCTION_VAR.into(),
                    bash_git_function(&dir.join(if cfg!(windows) { "git.exe" } else { "git" })),
                ),
            ],
            system_prompt: Vec::new(),
            ..Default::default()
        };
        if let Some(real) = find_git(Some(Path::new(&dir))) {
            extras
                .env
                .push(("HELM_REAL_GIT".into(), real.to_string_lossy().into_owned()));
        }
        extras
    }
}
