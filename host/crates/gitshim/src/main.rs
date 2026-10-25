//! Helm's git stand-in.
//!
//! Helm installs this as `git.exe` in a folder it puts first on a chat's `PATH`, but only while
//! some repository has worktrees switched off. It hands every command to the real git unchanged,
//! with two exceptions, and only for a repository on the "off" list:
//!
//! - `git worktree add` is refused with a short message and exit status 128;
//! - `git worktree remove` is refused while the worktree still holds links (junctions or
//!   symlinks), because a recursive delete could follow one into a shared folder. The message
//!   says to take the links out as directory entries first.
//!
//! Not covered: git aliases and autocorrected spellings of `worktree`, programs that do not use
//! `git.exe` (libgit2 and the like), and a git called by its full path. It is a guard against
//! accidents, not a sandbox.
//!
//! Environment (set by Helm when it starts a chat):
//! - `HELM_REAL_GIT`: the real git. Without it the stand-in walks `PATH` itself, skipping its own
//!   folder, and exits 127 when there is none.
//! - `HELM_WORKTREE_OFF_LIST`: the off list file.

use helm_worktrees::argv::{self, Command};
use helm_worktrees::git::{find_git_in, Git};
use helm_worktrees::{links, offlist};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

fn real_git() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("HELM_REAL_GIT").map(PathBuf::from) {
        if p.is_file() {
            return Some(p);
        }
    }
    let me = std::env::current_exe().ok()?;
    let own_dir = me.parent()?.to_path_buf();
    find_git_in(&std::env::var_os("PATH")?, Some(&own_dir))
}

/// Run the real git with this process's arguments and environment and return its status.
fn pass_through(real: &Path, args: &[String]) -> ExitCode {
    match std::process::Command::new(real).args(args).status() {
        Ok(s) => ExitCode::from(s.code().unwrap_or(1).clamp(0, 255) as u8),
        Err(e) => {
            eprintln!("helm git: could not start {}: {e}", real.display());
            ExitCode::from(127)
        }
    }
}

fn refuse(message: &str) -> ExitCode {
    eprintln!("{message}");
    ExitCode::from(128)
}

/// The off-list line for the repository a command would act on, if there is one. The repository
/// is found by asking the real git with the command's own `-C`, `--git-dir` and `-c` options.
fn off_repo(real: &Path, parsed: &argv::Parsed, list: &[offlist::Line]) -> Option<offlist::Line> {
    if list.is_empty() {
        return None;
    }
    let cwd = std::env::current_dir().ok()?;
    let mut args: Vec<&str> = parsed.globals.iter().map(String::as_str).collect();
    args.extend([
        "rev-parse",
        "--path-format=absolute",
        "--git-common-dir",
        "--show-toplevel",
    ]);
    let o = Git::new(real.to_path_buf()).run(&cwd, &args)?;
    if !o.ok() {
        return None;
    }
    let mut lines = o.text().lines();
    let (common, top) = (lines.next()?, lines.next());
    let common = PathBuf::from(common);
    let mut candidates = Vec::new();
    if let Some(top) = top {
        candidates.push(offlist::plain_canonical(Path::new(top)));
    }
    if common.file_name().is_some_and(|n| n == ".git") {
        if let Some(main) = common.parent() {
            candidates.push(offlist::plain_canonical(main));
        }
    }
    offlist::find(list, &candidates).cloned()
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(real) = real_git() else {
        eprintln!("helm git: no git found on PATH");
        return ExitCode::from(127);
    };
    let parsed = argv::parse(&args);
    if parsed.command == Command::Other {
        return pass_through(&real, &args);
    }
    let list = std::env::var_os("HELM_WORKTREE_OFF_LIST")
        .map(|p| offlist::read(Path::new(&p)))
        .unwrap_or_default();
    let Some(line) = off_repo(&real, &parsed, &list) else {
        return pass_through(&real, &args);
    };
    match &parsed.command {
        Command::WorktreeAdd { .. } => refuse(&format!(
            "Worktrees are switched off for {} in Helm. Work in the existing checkout; if it is busy, tell the user instead of making a worktree.",
            line.repo_id
        )),
        Command::WorktreeRemove { path: Some(path) } => {
            let base = parsed
                .dirs
                .iter()
                .fold(std::env::current_dir().unwrap_or_default(), |acc, d| acc.join(d));
            let target = base.join(path);
            match links::link_entries(&target) {
                Ok(l) if l.is_empty() => pass_through(&real, &args),
                Ok(l) => refuse(&format!(
                    "Not removing {}: it still holds links, for example {}. Take each link out on its own first (rmdir on a junction, never rm -rf), then run this again.",
                    target.display(),
                    l[0].display()
                )),
                // Not a folder we can read: git will say what is wrong.
                Err(_) => pass_through(&real, &args),
            }
        }
        _ => pass_through(&real, &args),
    }
}
