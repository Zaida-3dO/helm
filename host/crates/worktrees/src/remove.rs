//! Removing a finished worktree, carefully.
//!
//! A worktree is removed only when it is positively known to be finished: merged into the
//! repository's base branch, clean, and not in use. Unknown means keep. Removal then takes every
//! link out of the worktree first ([`crate::links`]) and uses git's own non-forced
//! `worktree remove`, so git refuses if anything it cares about is left. Branches are never
//! deleted.

use crate::config;
use crate::git::Git;
use crate::links;
use std::path::{Path, PathBuf};

/// What is known about a worktree. `None` is "could not tell", which keeps the worktree.
#[derive(Debug, Clone, Default)]
pub struct Facts {
    /// The main checkout, not a linked worktree.
    pub is_main: bool,
    /// HEAD is already in the base branch (so nothing would be lost).
    pub merged: Option<bool>,
    /// No uncommitted or untracked changes.
    pub clean: Option<bool>,
    /// A chat or a program is working in it.
    pub in_use: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    Remove,
    Keep(String),
}

/// The rule, in one place: all three must be known and favourable.
pub fn decide(f: &Facts) -> Decision {
    let keep = |why: &str| Decision::Keep(why.to_owned());
    if f.is_main {
        return keep("It is the main checkout, not a linked worktree.");
    }
    match (f.merged, f.clean, f.in_use) {
        (Some(false), _, _) => keep("Its work is not merged into the base branch yet."),
        (None, _, _) => keep("It could not be checked against the base branch."),
        (_, Some(false), _) => keep("It has changes that are not committed."),
        (_, None, _) => keep("Its changes could not be checked."),
        (_, _, Some(true)) => keep("A chat is working in it."),
        (_, _, None) => keep("It could not be checked for use."),
        (Some(true), Some(true), Some(false)) => Decision::Remove,
    }
}

/// The main working tree of the repository a worktree belongs to.
pub fn main_checkout(git: &Git, worktree: &Path) -> Option<PathBuf> {
    let o = git.run(
        worktree,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    )?;
    if !o.ok() {
        return None;
    }
    let common = PathBuf::from(o.text());
    (common.file_name()? == ".git").then(|| common.parent().map(Path::to_path_buf))?
}

/// Look at a worktree. `in_use` comes from the caller (Helm knows which chats run where).
pub fn gather(git: &Git, worktree: &Path, in_use: Option<bool>) -> Facts {
    let mut f = Facts {
        in_use,
        is_main: config::is_clone_root(worktree),
        ..Default::default()
    };
    if f.is_main {
        return f;
    }
    f.clean = git
        .run(
            worktree,
            &["status", "--porcelain", "--untracked-files=all"],
        )
        .filter(|o| o.ok())
        .map(|o| o.text().is_empty());
    let main = main_checkout(git, worktree);
    let exists = |r: &str| {
        git.run(worktree, &["rev-parse", "--verify", "--quiet", r])
            .is_some_and(|o| o.ok())
    };
    let base = main
        .as_deref()
        .and_then(config::default_branch)
        .or_else(|| {
            let o = git.run(
                worktree,
                &["symbolic-ref", "--short", "refs/remotes/origin/HEAD"],
            )?;
            o.ok()
                .then(|| o.text().trim_start_matches("origin/").to_owned())
        });
    let base = base.or_else(|| {
        ["main", "master"]
            .into_iter()
            .find(|b| exists(&format!("refs/heads/{b}")))
            .map(str::to_owned)
    });
    if let Some(base) = base {
        // Merged when HEAD is already in the remote base or the local one. A base ref that does
        // not exist is skipped; an answer git cannot give is "unknown".
        let (mut any_yes, mut any_no, mut unknown) = (false, false, false);
        for b in [
            format!("refs/remotes/origin/{base}"),
            format!("refs/heads/{base}"),
        ] {
            if !exists(&b) {
                continue;
            }
            match git.run(worktree, &["merge-base", "--is-ancestor", "HEAD", &b]) {
                Some(o) if o.code == 0 => any_yes = true,
                Some(o) if o.code == 1 => any_no = true,
                _ => unknown = true,
            }
        }
        f.merged = if any_yes {
            Some(true)
        } else if any_no && !unknown {
            Some(false)
        } else {
            None
        };
    }
    f
}

/// What happened.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    Removed { links_removed: usize },
    Kept(String),
}

/// Remove `worktree` if [`decide`] says so: unlink every link, then `git worktree remove` without
/// force and `git worktree prune`. Anything unexpected keeps the worktree and says why.
pub fn remove(git: &Git, worktree: &Path, in_use: Option<bool>) -> Outcome {
    let facts = gather(git, worktree, in_use);
    if let Decision::Keep(why) = decide(&facts) {
        return Outcome::Kept(why);
    }
    let Some(main) = main_checkout(git, worktree) else {
        return Outcome::Kept("Its repository could not be found.".into());
    };
    let unlinked = match links::unlink_all(worktree) {
        Ok(l) => l,
        Err(why) => return Outcome::Kept(format!("Kept: {why}.")),
    };
    let target = worktree.to_string_lossy().into_owned();
    match git.run(&main, &["worktree", "remove", &target]) {
        Some(o) if o.ok() => {
            let _ = git.run(&main, &["worktree", "prune"]);
            Outcome::Removed {
                links_removed: unlinked.len(),
            }
        }
        Some(o) => Outcome::Kept(format!(
            "Git refused to remove it{}: {}",
            if unlinked.is_empty() {
                ""
            } else {
                " (its links were already taken out)"
            },
            o.stderr.trim()
        )),
        None => Outcome::Kept("Git could not be started.".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    fn git_ok(dir: &Path, args: &[&str]) {
        let o = Command::new("git")
            .current_dir(dir)
            .args([
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@example.com",
                "-c",
                "commit.gpgsign=false",
            ])
            .args(args)
            .output()
            .expect("git on PATH");
        assert!(
            o.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&o.stderr)
        );
    }

    /// The decision table, then the real thing against a temp repository: a merged, clean
    /// worktree with a `node_modules` junction goes and leaves the shared folder alone; dirty,
    /// unmerged, busy and unknown ones stay.
    #[test]
    fn only_merged_clean_unused_worktrees_go_and_links_are_never_followed() {
        let (t, f) = (Some(true), Some(false));
        let facts = |merged, clean, in_use| Facts {
            is_main: false,
            merged,
            clean,
            in_use,
        };
        for (fx, remove) in [
            (facts(t, t, f), true),
            (facts(f, t, f), false),
            (facts(t, f, f), false),
            (facts(t, t, t), false),
            (facts(None, t, f), false),
            (facts(t, None, f), false),
            (facts(t, t, None), false),
            (
                Facts {
                    is_main: true,
                    ..facts(t, t, f)
                },
                false,
            ),
        ] {
            assert_eq!(decide(&fx) == Decision::Remove, remove, "{fx:?}");
        }

        let Some(git) = Git::locate() else {
            panic!("git must be on PATH for this test");
        };
        let d = tempfile::tempdir().unwrap();
        let main = d.path().join("main");
        std::fs::create_dir(&main).unwrap();
        git_ok(&main, &["init", "-q", "-b", "main"]);
        std::fs::write(main.join("a.txt"), "a").unwrap();
        git_ok(&main, &["add", "a.txt"]);
        git_ok(&main, &["commit", "-q", "-m", "one"]);
        let shared = d.path().join("shared_modules");
        std::fs::create_dir(&shared).unwrap();
        std::fs::write(shared.join("keep.txt"), "keep").unwrap();

        let add = |name: &str| {
            let wt = d.path().join(name);
            git_ok(
                &main,
                &["worktree", "add", "-q", "-b", name, wt.to_str().unwrap()],
            );
            wt
        };
        let link_modules = |wt: &Path| {
            links::make_dir_link(&wt.join("node_modules"), &shared).unwrap();
            // The link is untracked unless ignored: ignore it the way a project would.
            std::fs::write(main.join(".git/info/exclude"), "node_modules\n").unwrap();
        };

        // Merged (the branch is at main's commit), clean, with a junction.
        let done = add("done");
        link_modules(&done);
        assert_eq!(links::link_entries(&done).unwrap().len(), 1);
        assert_eq!(
            remove(&git, &done, Some(false)),
            Outcome::Removed { links_removed: 1 }
        );
        assert!(!done.exists(), "the worktree is gone");
        assert!(
            shared.join("keep.txt").exists(),
            "the shared folder was not followed"
        );

        // Busy, then dirty, then unmerged: each stays, links untouched.
        let busy = add("busy");
        link_modules(&busy);
        assert!(matches!(remove(&git, &busy, Some(true)), Outcome::Kept(_)));
        assert!(matches!(remove(&git, &busy, None), Outcome::Kept(_)));
        assert!(
            busy.join("node_modules").exists(),
            "a kept worktree keeps its links"
        );
        std::fs::write(busy.join("new.txt"), "x").unwrap();
        assert!(
            matches!(remove(&git, &busy, Some(false)), Outcome::Kept(m) if m.contains("not committed"))
        );
        std::fs::remove_file(busy.join("new.txt")).unwrap();
        std::fs::write(busy.join("a.txt"), "changed").unwrap();
        git_ok(&busy, &["commit", "-q", "-am", "ahead"]);
        assert!(
            matches!(remove(&git, &busy, Some(false)), Outcome::Kept(m) if m.contains("not merged"))
        );
        assert!(shared.join("keep.txt").exists());

        // A broken link keeps the worktree (unknown means keep).
        let broken = add("broken");
        let gone = d.path().join("gone_target");
        std::fs::create_dir(&gone).unwrap();
        links::make_dir_link(&broken.join("node_modules"), &gone).unwrap();
        std::fs::remove_dir(&gone).unwrap();
        assert!(
            matches!(remove(&git, &broken, Some(false)), Outcome::Kept(m) if m.contains("missing"))
        );
        assert!(broken.exists());

        // The main checkout is never removed.
        assert!(matches!(remove(&git, &main, Some(false)), Outcome::Kept(_)));
        assert!(main.join("a.txt").exists());
    }
}
