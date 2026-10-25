//! The git stand-in against a real git: it refuses worktree commands only for repositories on
//! the off list, and everything else behaves like git.

use helm_worktrees::{links, offlist};
use std::path::Path;
use std::process::{Command, Output};

fn git(dir: &Path, args: &[&str]) {
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

fn repo(dir: &Path) {
    std::fs::create_dir_all(dir).unwrap();
    git(dir, &["init", "-q", "-b", "main"]);
    std::fs::write(dir.join("a.txt"), "a").unwrap();
    git(dir, &["add", "a.txt"]);
    git(dir, &["commit", "-q", "-m", "one"]);
}

fn shim(dir: &Path, list: &Path, real: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_helm-git"))
        .current_dir(dir)
        .env("HELM_REAL_GIT", real)
        .env("HELM_WORKTREE_OFF_LIST", list)
        .args(args)
        .output()
        .unwrap()
}

#[test]
fn worktree_commands_are_refused_only_in_repositories_that_are_off() {
    let real = helm_worktrees::git::find_git(None).expect("git on PATH");
    let d = tempfile::tempdir().unwrap();
    let (off, on) = (d.path().join("off"), d.path().join("on"));
    repo(&off);
    repo(&on);
    let list = d.path().join("worktrees-off.tsv");
    let line = offlist::Line {
        repo_id: "github.com/o/off".into(),
        path: offlist::plain_canonical(&off),
    };
    offlist::write(&list, &[line]).unwrap();
    let wt = |name: &str| d.path().join(name).to_string_lossy().into_owned();

    // Off: add is refused (status 128, nothing created), through -C as well; reads pass.
    let o = shim(&off, &list, &real, &["worktree", "add", &wt("w1")]);
    assert_eq!(o.status.code(), Some(128));
    assert!(String::from_utf8_lossy(&o.stderr).contains("github.com/o/off"));
    assert!(!d.path().join("w1").exists());
    let o = shim(
        d.path(),
        &list,
        &real,
        &["-C", off.to_str().unwrap(), "worktree", "add", &wt("w1")],
    );
    assert_eq!(o.status.code(), Some(128));
    assert!(shim(&off, &list, &real, &["status", "--short"])
        .status
        .success());
    assert!(shim(&off, &list, &real, &["worktree", "list"])
        .status
        .success());

    // On: the same command goes through to git.
    assert!(shim(
        &on,
        &list,
        &real,
        &["worktree", "add", "-q", "-b", "w2", &wt("w2")]
    )
    .status
    .success());
    assert!(d.path().join("w2").join("a.txt").exists());

    // A linked worktree of an off repository is still that repository.
    git(&off, &["worktree", "add", "-q", "-b", "w3", &wt("w3")]);
    let o = shim(
        &d.path().join("w3"),
        &list,
        &real,
        &["worktree", "add", &wt("w4")],
    );
    assert_eq!(o.status.code(), Some(128));

    // Removal in an off repository: refused while the worktree holds a link, allowed after.
    let shared = d.path().join("shared");
    std::fs::create_dir(&shared).unwrap();
    std::fs::write(shared.join("keep.txt"), "k").unwrap();
    links::make_dir_link(&d.path().join("w3").join("node_modules"), &shared).unwrap();
    std::fs::write(off.join(".git/info/exclude"), "node_modules\n").unwrap();
    let o = shim(&off, &list, &real, &["worktree", "remove", &wt("w3")]);
    assert_eq!(o.status.code(), Some(128));
    assert!(String::from_utf8_lossy(&o.stderr).contains("links"));
    assert!(d.path().join("w3").exists());
    std::fs::remove_dir(d.path().join("w3").join("node_modules")).unwrap();
    assert!(shim(&off, &list, &real, &["worktree", "remove", &wt("w3")])
        .status
        .success());
    assert!(shared.join("keep.txt").exists());

    // With an empty list the stand-in is git.
    let none = d.path().join("none.tsv");
    assert!(shim(
        &off,
        &none,
        &real,
        &["worktree", "add", "-q", "-b", "w5", &wt("w5")]
    )
    .status
    .success());
}
