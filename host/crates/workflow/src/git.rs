//! Reading a commit's tree from git (the tree is read from git, never taken from the
//! agent's word). The engine takes a [`TreeReader`] so tests use a fake.

use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// Longest a single git lookup may take (1 s per lookup; a tree read is one).
pub const LOOKUP: Duration = Duration::from_secs(2);

pub trait TreeReader: Send + Sync {
    /// The tree sha of `tip` in the repository at `cwd`, or `None` when git cannot say (unknown
    /// commit, not a repository, timeout). An unknown tree counts as a different tree.
    fn tree(&self, cwd: &Path, tip: &str) -> Option<String>;
}

/// A full 40-character lowercase hex sha.
pub fn is_sha(s: &str) -> bool {
    s.len() == 40
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// The real reader: `git -C <cwd> rev-parse --verify --quiet <tip>^{tree}`.
pub struct GitTrees;

impl TreeReader for GitTrees {
    fn tree(&self, cwd: &Path, tip: &str) -> Option<String> {
        if !is_sha(tip) {
            return None;
        }
        let mut child = Command::new("git")
            .arg("-C")
            .arg(cwd)
            .args(["rev-parse", "--verify", "--quiet"])
            .arg(format!("{tip}^{{tree}}"))
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .ok()?;
        let started = Instant::now();
        loop {
            match child.try_wait() {
                Ok(Some(_)) => break,
                Ok(None) if started.elapsed() < LOOKUP => {
                    std::thread::sleep(Duration::from_millis(20))
                }
                _ => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return None;
                }
            }
        }
        let out = child.wait_with_output().ok()?;
        if !out.status.success() {
            return None;
        }
        let t = String::from_utf8(out.stdout).ok()?.trim().to_owned();
        is_sha(&t).then_some(t)
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::sync::Mutex;

    /// tip → tree, set by the test.
    #[derive(Default)]
    pub struct FakeTrees(pub Mutex<HashMap<String, String>>);

    impl FakeTrees {
        pub fn set(&self, tip: &str, tree: &str) {
            self.0.lock().unwrap().insert(tip.into(), tree.into());
        }
    }

    impl TreeReader for FakeTrees {
        fn tree(&self, _cwd: &Path, tip: &str) -> Option<String> {
            self.0.lock().unwrap().get(tip).cloned()
        }
    }

    #[test]
    fn shas_are_forty_lowercase_hex() {
        assert!(is_sha(&"a".repeat(40)));
        assert!(!is_sha(&"A".repeat(40)));
        assert!(!is_sha(&"a".repeat(39)));
        assert!(!is_sha(&"g".repeat(40)));
    }

    /// Needs git on PATH (as the panel crate's tests do).
    #[test]
    fn the_real_reader_reads_a_tree_and_answers_none_for_an_unknown_commit() {
        let dir = tempfile::tempdir().unwrap();
        let git = |args: &[&str]| {
            Command::new("git")
                .arg("-C")
                .arg(dir.path())
                .args(args)
                .output()
                .unwrap()
        };
        git(&["init", "-q"]);
        std::fs::write(dir.path().join("f"), "x").unwrap();
        git(&["add", "f"]);
        git(&[
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@example.invalid",
            "commit",
            "-qm",
            "c",
        ]);
        let head = String::from_utf8(git(&["rev-parse", "HEAD"]).stdout).unwrap();
        let tree = GitTrees.tree(dir.path(), head.trim()).unwrap();
        assert!(is_sha(&tree));
        assert_ne!(tree, head.trim());
        assert_eq!(GitTrees.tree(dir.path(), &"0".repeat(40)), None);
        assert_eq!(
            GitTrees.tree(dir.path(), "HEAD"),
            None,
            "only a full sha is read"
        );
    }
}
