//! The Changes view's host side:
//! `changes`, `diff`, `changes_stage`, `changes_unstage`.
//!
//! - `changes` reads `git status --porcelain=v1 -z` (at most 2,000 records) plus per-file counts,
//!   with paths relative to the repository top, and the app's private folders left out.
//! - `diff` reads one file's diff on one side (head, staged, unstaged), with full context when it
//!   fits, so the viewer can expand hidden lines; an untracked file is shown as all added.
//! - Staging is the only git write: one path, one write at a time process-wide (a second click is
//!   refused at once, never queued), `git status` re-read first to confirm the path is still in the
//!   state the button acts on, hooks disabled, `--` before the path, no force or all flags.

use crate::git::{Git, GitError, EMPTY_TREE};
use crate::private::PrivateRoots;
use serde::Serialize;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

pub const MAX_RECORDS: usize = 2000;
pub const DIFF_CAP: usize = 512 * 1024;
pub const MAX_REL_PATH: usize = 4096;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Section {
    Staged,
    Unstaged,
    Conflicts,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChangeRecord {
    /// Relative to the repository top, with forward slashes.
    pub path: String,
    /// The one-letter status for this section (M, A, D, R, C, T, U for untracked, or the two
    /// unmerged letters for a conflict).
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub from: Option<String>,
    pub section: Section,
    /// Also present in the other of staged/unstaged.
    pub partial: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub added: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub removed: Option<u64>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChangesView {
    pub repo_top: String,
    pub branch: Option<String>,
    pub records: Vec<ChangeRecord>,
    pub truncated: bool,
}

/// One raw status entry: the two letters, the path, and a rename's old path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatusEntry {
    pub x: char,
    pub y: char,
    pub path: String,
    pub from: Option<String>,
}

const UNMERGED: &[&str] = &["DD", "AU", "UD", "UA", "DU", "AA", "UU"];

/// Parses `git status --porcelain=v1 -z`.
pub fn parse_status(out: &[u8]) -> Vec<StatusEntry> {
    let text = String::from_utf8_lossy(out);
    let mut parts = text.split('\0');
    let mut entries = Vec::new();
    while let Some(rec) = parts.next() {
        if rec.len() < 4 {
            continue;
        }
        let mut chars = rec.chars();
        let x = chars.next().unwrap_or(' ');
        let y = chars.next().unwrap_or(' ');
        let path = rec[3..].to_owned();
        // A rename or copy, in either column (an intent-to-add file renamed in the worktree has
        // Y = R), carries its old path as the next field.
        let from = if matches!(x, 'R' | 'C') || matches!(y, 'R' | 'C') {
            parts.next().map(str::to_owned)
        } else {
            None
        };
        entries.push(StatusEntry { x, y, path, from });
    }
    entries
}

/// Splits status entries into the Staged / Unstaged / Conflicts sections.
pub fn sections(entries: &[StatusEntry]) -> Vec<ChangeRecord> {
    let mut out = Vec::new();
    for e in entries {
        let code: String = [e.x, e.y].iter().collect();
        if UNMERGED.contains(&code.as_str()) {
            out.push(ChangeRecord {
                path: e.path.clone(),
                status: code,
                from: None,
                section: Section::Conflicts,
                partial: false,
                added: None,
                removed: None,
            });
            continue;
        }
        if e.x == '?' {
            out.push(ChangeRecord {
                path: e.path.clone(),
                status: "U".into(),
                from: None,
                section: Section::Unstaged,
                partial: false,
                added: None,
                removed: None,
            });
            continue;
        }
        if e.x == '!' {
            continue;
        }
        let staged = e.x != ' ';
        let unstaged = e.y != ' ';
        if staged {
            out.push(ChangeRecord {
                path: e.path.clone(),
                status: e.x.to_string(),
                from: e.from.clone(),
                section: Section::Staged,
                partial: unstaged,
                added: None,
                removed: None,
            });
        }
        if unstaged {
            out.push(ChangeRecord {
                path: e.path.clone(),
                status: e.y.to_string(),
                from: None,
                section: Section::Unstaged,
                partial: staged,
                added: None,
                removed: None,
            });
        }
    }
    out
}

/// Parses `git diff --numstat -z` into path → (added, removed); binary files have no counts.
pub fn parse_numstat(out: &[u8]) -> HashMap<String, (u64, u64)> {
    let text = String::from_utf8_lossy(out);
    let mut map = HashMap::new();
    let mut parts = text.split('\0');
    while let Some(rec) = parts.next() {
        let mut f = rec.splitn(3, '\t');
        let (Some(a), Some(d), Some(p)) = (f.next(), f.next(), f.next()) else {
            continue;
        };
        // A rename is "a\td\t" then the old and new paths as two more fields.
        let path = if p.is_empty() {
            let _old = parts.next();
            parts.next().unwrap_or_default().to_owned()
        } else {
            p.to_owned()
        };
        if let (Ok(a), Ok(d)) = (a.parse(), d.parse()) {
            map.insert(path, (a, d));
        }
    }
    map
}

/// A repository-relative path from the renderer: non-empty, ≤ 4 KiB, no NUL, not absolute, no `..`.
pub fn check_rel(path: &str) -> Result<(), String> {
    if path.is_empty()
        || path.len() > MAX_REL_PATH
        || path.contains('\0')
        || path.chars().any(char::is_control)
    {
        return Err("The path is not valid.".into());
    }
    let n = path.replace('\\', "/");
    if n.starts_with('/') || (n.len() >= 2 && n.as_bytes()[1] == b':') {
        return Err("The path must be relative to the repository.".into());
    }
    if n.split('/').any(|s| s == "..") {
        return Err("The path must stay inside the repository.".into());
    }
    Ok(())
}

fn is_private(top: &Path, rel: &str, roots: &PrivateRoots) -> bool {
    roots.contains(&top.join(rel.replace('/', std::path::MAIN_SEPARATOR_STR)))
}

pub fn read_changes(
    git: &Git,
    folder: &Path,
    roots: &PrivateRoots,
) -> Result<ChangesView, GitError> {
    let top = git.toplevel(folder)?;
    let status = git.read(
        &top,
        &["status", "--porcelain=v1", "-z", "--untracked-files=all"],
    )?;
    if status.status != 0 {
        return Err(GitError::Failed(status.stderr.trim().to_owned()));
    }
    let entries = parse_status(&status.stdout);
    let mut records = sections(&entries);
    records.retain(|r| !is_private(&top, &r.path, roots));
    let truncated = records.len() > MAX_RECORDS;
    records.truncate(MAX_RECORDS);
    let staged = git
        .read(
            &top,
            &[
                "diff",
                "--no-ext-diff",
                "--no-textconv",
                "--cached",
                "--numstat",
                "-z",
                "-M",
            ],
        )
        .map(|o| parse_numstat(&o.stdout))
        .unwrap_or_default();
    let unstaged = git
        .read(
            &top,
            &["diff", "--no-ext-diff", "--no-textconv", "--numstat", "-z"],
        )
        .map(|o| parse_numstat(&o.stdout))
        .unwrap_or_default();
    for r in &mut records {
        let counts = match r.section {
            Section::Staged => staged.get(&r.path),
            Section::Unstaged => unstaged.get(&r.path),
            Section::Conflicts => None,
        };
        if let Some((a, d)) = counts {
            r.added = Some(*a);
            r.removed = Some(*d);
        }
    }
    Ok(ChangesView {
        repo_top: top.to_string_lossy().into_owned(),
        branch: git.branch(&top),
        records,
        truncated,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Side {
    Head,
    Staged,
    Unstaged,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DiffView {
    pub patch: String,
    /// The patch carries the whole file as context, so hidden lines can be expanded.
    pub full_context: bool,
    pub truncated: bool,
}

fn cap_text(bytes: &[u8]) -> (String, bool) {
    if bytes.len() <= DIFF_CAP {
        return (String::from_utf8_lossy(bytes).into_owned(), false);
    }
    let mut end = DIFF_CAP;
    while end > 0 && (bytes[end] & 0b1100_0000) == 0b1000_0000 {
        end -= 1;
    }
    (String::from_utf8_lossy(&bytes[..end]).into_owned(), true)
}

/// An untracked file as an all-added patch, read through the viewer's gate.
fn untracked_patch(top: &Path, rel: &str, roots: &PrivateRoots) -> Result<DiffView, GitError> {
    let abs = top.join(rel.replace('/', std::path::MAIN_SEPARATOR_STR));
    let view = crate::viewer::read(&abs.to_string_lossy(), roots)
        .map_err(|e| GitError::Failed(e.message()))?;
    let text = view.text.unwrap_or_default();
    let lines: Vec<&str> = if text.is_empty() {
        vec![]
    } else {
        text.trim_end_matches('\n').split('\n').collect()
    };
    let mut patch = format!(
        "--- /dev/null\n+++ b/{rel}\n@@ -0,0 +1,{} @@\n",
        lines.len()
    );
    for l in &lines {
        patch.push('+');
        patch.push_str(l);
        patch.push('\n');
    }
    Ok(DiffView {
        patch,
        full_context: !view.truncated,
        truncated: view.truncated,
    })
}

pub fn read_diff(
    git: &Git,
    folder: &Path,
    rel: &str,
    from: Option<&str>,
    side: Side,
    roots: &PrivateRoots,
) -> Result<DiffView, GitError> {
    check_rel(rel).map_err(GitError::Failed)?;
    if let Some(f) = from {
        check_rel(f).map_err(GitError::Failed)?;
    }
    let top = git.toplevel(folder)?;
    if is_private(&top, rel, roots) {
        return Err(GitError::Failed(crate::viewer::PRIVATE_MESSAGE.into()));
    }
    let base = if git.has_head(&top) {
        "HEAD"
    } else {
        EMPTY_TREE
    };
    let status = git.read(
        &top,
        &[
            "status",
            "--porcelain=v1",
            "-z",
            "--untracked-files=all",
            "--",
            rel,
        ],
    )?;
    if parse_status(&status.stdout)
        .iter()
        .any(|e| e.x == '?' && e.path == rel)
        && side != Side::Staged
    {
        return untracked_patch(&top, rel, roots);
    }
    let run = |context: &str| -> Result<Vec<u8>, GitError> {
        let mut args: Vec<&str> = vec![
            "diff",
            "--no-ext-diff",
            "--no-textconv",
            "--no-color",
            context,
        ];
        match side {
            Side::Head => args.push(base),
            Side::Staged => {
                args.extend(["--cached", "-M", base]);
            }
            Side::Unstaged => {}
        }
        args.push("--");
        if let Some(f) = from {
            args.push(f);
        }
        args.push(rel);
        let o = git.read(&top, &args)?;
        if o.status != 0 {
            return Err(GitError::Failed(o.stderr.trim().to_owned()));
        }
        Ok(o.stdout)
    };
    let full = run("--unified=100000")?;
    if full.len() <= DIFF_CAP {
        return Ok(DiffView {
            patch: String::from_utf8_lossy(&full).into_owned(),
            full_context: true,
            truncated: false,
        });
    }
    let (patch, truncated) = cap_text(&run("--unified=3")?);
    Ok(DiffView {
        patch,
        full_context: false,
        truncated,
    })
}

/// One staging write at a time, process-wide.
static STAGING: Mutex<()> = Mutex::new(());

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StageOp {
    Stage,
    Unstage,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StageResult {
    pub changes: ChangesView,
    /// Git changed nothing for the path.
    pub noop: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StageError {
    Invalid(String),
    Busy,
    Stale,
    Git(GitError),
}

impl StageError {
    pub fn message(&self) -> String {
        match self {
            Self::Invalid(m) => m.clone(),
            Self::Busy => "Another staging change is still running. Try again in a moment.".into(),
            Self::Stale => "The changes list is out of date. Refresh it and try again.".into(),
            Self::Git(e) => e.message(),
        }
    }
}

/// Stages or unstages one path. `hooks_dir` is an empty folder used as the hooks path.
pub fn stage(
    git: &Git,
    folder: &Path,
    rel: &str,
    from: Option<&str>,
    op: StageOp,
    roots: &PrivateRoots,
    hooks_dir: &Path,
) -> Result<StageResult, StageError> {
    // 1. Arguments.
    check_rel(rel).map_err(StageError::Invalid)?;
    if let Some(f) = from {
        check_rel(f).map_err(StageError::Invalid)?;
    }
    // 3. The repository top. 4. Private roots.
    let top = git.toplevel(folder).map_err(StageError::Git)?;
    if is_private(&top, rel, roots) || from.is_some_and(|f| is_private(&top, f, roots)) {
        return Err(StageError::Invalid(crate::viewer::PRIVATE_MESSAGE.into()));
    }
    // 5. One write at a time: a second request fails at once.
    let _guard = STAGING.try_lock().map_err(|_| StageError::Busy)?;
    // 6. A fresh status must still show the path in the state the button acts on.
    let before = git
        .read(
            &top,
            &["status", "--porcelain=v1", "-z", "--untracked-files=all"],
        )
        .map_err(StageError::Git)?;
    let entries = parse_status(&before.stdout);
    let entry = entries.iter().find(|e| e.path == rel).cloned();
    let ok = match (op, &entry) {
        (StageOp::Stage, Some(e)) => e.x == '?' || (e.y != ' ' && e.y != '?'),
        (StageOp::Unstage, Some(e)) => {
            e.x != ' ' && e.x != '?' && ((e.x != 'R' && e.x != 'C') || e.from.as_deref() == from)
        }
        _ => false,
    };
    // Conflicts have no stage controls: staging one would mark it resolved.
    if let Some(e) = &entry {
        if UNMERGED.contains(&[e.x, e.y].iter().collect::<String>().as_str()) {
            return Err(StageError::Invalid(
                "Resolve conflicts in your editor or terminal.".into(),
            ));
        }
    }
    if !ok {
        return Err(StageError::Stale);
    }
    // 7. The write: hooks off through an empty hooks path, `--` before the paths.
    let hooks = format!("core.hooksPath={}", hooks_dir.to_string_lossy());
    let mut args: Vec<&str> = vec!["-c", &hooks];
    match op {
        StageOp::Stage => args.extend(["add", "--"]),
        StageOp::Unstage if git.has_head(&top) => args.extend(["reset", "-q", "--"]),
        StageOp::Unstage => args.extend(["rm", "--cached", "-q", "--"]),
    }
    if op == StageOp::Unstage {
        if let Some(f) = from {
            args.push(f);
        }
    }
    args.push(rel);
    let out = git.write(&top, &args).map_err(StageError::Git)?;
    if out.status != 0 {
        // 8. An existing index lock is reported and left alone.
        let msg = if out.stderr.contains("index.lock") {
            "Another Git process holds the index lock. Wait for it to finish, then refresh."
                .to_owned()
        } else {
            out.stderr.trim().to_owned()
        };
        return Err(StageError::Git(GitError::Failed(msg)));
    }
    // 9. Reply: the list re-read once, and whether anything changed for the path.
    let after = git
        .read(
            &top,
            &["status", "--porcelain=v1", "-z", "--untracked-files=all"],
        )
        .map_err(StageError::Git)?;
    let after_entry = parse_status(&after.stdout)
        .into_iter()
        .find(|e| e.path == rel);
    let noop = after_entry == entry;
    drop(_guard);
    let changes = read_changes(git, folder, roots).map_err(StageError::Git)?;
    Ok(StageResult { changes, noop })
}

/// The empty hooks folder under the profile.
pub fn hooks_dir(profile_dir: &Path) -> PathBuf {
    profile_dir.join("runtime").join("no-hooks")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_records_split_into_sections_with_partial_and_renames() {
        let raw = b"M  a.txt\0 M b.txt\0MM c.txt\0R  new.txt\0old.txt\0?? u.txt\0UU conflict.txt\0!! ignored\0";
        let records = sections(&parse_status(raw));
        let find = |p: &str, s: Section| {
            records
                .iter()
                .find(|r| r.path == p && r.section == s)
                .cloned()
        };
        assert_eq!(find("a.txt", Section::Staged).unwrap().status, "M");
        assert!(find("a.txt", Section::Unstaged).is_none());
        assert!(
            find("b.txt", Section::Unstaged).is_some() && find("b.txt", Section::Staged).is_none()
        );
        assert!(
            find("c.txt", Section::Staged).unwrap().partial
                && find("c.txt", Section::Unstaged).unwrap().partial
        );
        assert_eq!(
            find("new.txt", Section::Staged).unwrap().from.as_deref(),
            Some("old.txt")
        );
        assert_eq!(find("u.txt", Section::Unstaged).unwrap().status, "U");
        assert_eq!(
            find("conflict.txt", Section::Conflicts).unwrap().status,
            "UU"
        );
        assert!(records.iter().all(|r| r.path != "ignored"));
    }

    #[test]
    fn a_worktree_rename_carries_its_old_path_and_does_not_leave_a_junk_record() {
        let e = parse_status(b" R new.txt\0old.txt\0 M other.txt\0");
        assert_eq!(e.len(), 2);
        assert_eq!(e[0].from.as_deref(), Some("old.txt"));
        assert_eq!(e[1].path, "other.txt");
    }

    #[test]
    fn numstat_reads_counts_renames_and_skips_binary() {
        let raw = b"3\t1\ta.txt\0-\t-\tbin.png\0" as &[u8];
        let mut v = raw.to_vec();
        v.extend_from_slice(b"2\t0\t\0old.txt\0new.txt\0");
        let m = parse_numstat(&v);
        assert_eq!(m.get("a.txt"), Some(&(3, 1)));
        assert_eq!(m.get("new.txt"), Some(&(2, 0)));
        assert!(!m.contains_key("bin.png"));
    }

    #[test]
    fn relative_paths_are_checked_before_anything_runs() {
        for bad in [
            "",
            "/abs",
            r"C:\x",
            "a/../b",
            "..",
            "a\0b",
            &"x".repeat(MAX_REL_PATH + 1),
        ] {
            assert!(check_rel(bad).is_err(), "{bad:?}");
        }
        assert!(check_rel("src/a.ts").is_ok());
    }

    fn git() -> Option<Git> {
        Git::locate().ok()
    }

    fn sh(g: &Git, dir: &Path, args: &[&str]) {
        let o = g.write(dir, args).unwrap();
        assert_eq!(o.status, 0, "{args:?}: {}", o.stderr);
    }

    /// Staging tests share the process-wide write lock; run them one at a time.
    static SERIAL: Mutex<()> = Mutex::new(());

    fn repo() -> Option<(tempfile::TempDir, Git)> {
        let g = git()?;
        let dir = tempfile::tempdir().unwrap();
        sh(&g, dir.path(), &["init", "-q", "-b", "main"]);
        sh(
            &g,
            dir.path(),
            &["config", "user.email", "t@example.invalid"],
        );
        sh(&g, dir.path(), &["config", "user.name", "T"]);
        Some((dir, g))
    }

    #[test]
    fn a_temp_repository_round_trip_reads_stages_and_unstages() {
        let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        let Some((dir, g)) = repo() else {
            eprintln!("git not found; skipping");
            return;
        };
        let p = dir.path();
        std::fs::write(p.join("a.txt"), "one\ntwo\n").unwrap();
        sh(&g, p, &["add", "a.txt"]);
        sh(&g, p, &["commit", "-q", "-m", "init"]);
        std::fs::write(p.join("a.txt"), "one\nTWO\nthree\n").unwrap();
        std::fs::write(p.join("new.txt"), "fresh\n").unwrap();
        let roots = PrivateRoots::default();
        let hooks = tempfile::tempdir().unwrap();

        let v = read_changes(&g, p, &roots).unwrap();
        assert_eq!(v.branch.as_deref(), Some("main"));
        let a = v.records.iter().find(|r| r.path == "a.txt").unwrap();
        assert_eq!(
            (a.section, a.added, a.removed),
            (Section::Unstaged, Some(2), Some(1))
        );
        assert!(v
            .records
            .iter()
            .any(|r| r.path == "new.txt" && r.status == "U"));

        let d = read_diff(&g, p, "a.txt", None, Side::Unstaged, &roots).unwrap();
        assert!(
            d.full_context
                && d.patch.contains("-two")
                && d.patch.contains("+TWO")
                && d.patch.contains(" one")
        );
        let u = read_diff(&g, p, "new.txt", None, Side::Unstaged, &roots).unwrap();
        assert!(u.patch.contains("+fresh"));

        // Stage, then the same click again is stale (the path is no longer unstaged).
        let r = stage(&g, p, "a.txt", None, StageOp::Stage, &roots, hooks.path()).unwrap();
        assert!(!r.noop);
        assert!(r
            .changes
            .records
            .iter()
            .any(|x| x.path == "a.txt" && x.section == Section::Staged));
        assert_eq!(
            stage(&g, p, "a.txt", None, StageOp::Stage, &roots, hooks.path()).unwrap_err(),
            StageError::Stale
        );
        let s = read_diff(&g, p, "a.txt", None, Side::Staged, &roots).unwrap();
        assert!(s.patch.contains("+three"));

        // Unstage puts it back.
        let r = stage(&g, p, "a.txt", None, StageOp::Unstage, &roots, hooks.path()).unwrap();
        assert!(r
            .changes
            .records
            .iter()
            .any(|x| x.path == "a.txt" && x.section == Section::Unstaged));
        assert!(stage(
            &g,
            p,
            "../escape",
            None,
            StageOp::Stage,
            &roots,
            hooks.path()
        )
        .is_err());
    }

    #[test]
    fn staging_runs_with_hooks_disabled() {
        let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        let Some((dir, g)) = repo() else { return };
        let p = dir.path();
        // `git add` runs the post-index-change hook; it leaves a marker if hooks ran.
        let hooks = p.join(".git").join("hooks");
        std::fs::create_dir_all(&hooks).unwrap();
        std::fs::write(
            hooks.join("post-index-change"),
            "#!/bin/sh\necho ran > hook-ran.txt\n",
        )
        .unwrap();
        std::fs::write(p.join("x.txt"), "x\n").unwrap();
        let empty = tempfile::tempdir().unwrap();
        stage(
            &g,
            p,
            "x.txt",
            None,
            StageOp::Stage,
            &PrivateRoots::default(),
            empty.path(),
        )
        .unwrap();
        assert!(
            !p.join("hook-ran.txt").exists(),
            "a repository hook ran during staging"
        );
        // Control: the same add without the empty hooks path does run the hook, so the assertion
        // above can fail.
        std::fs::write(
            p.join("z.txt"),
            "z
",
        )
        .unwrap();
        sh(&g, p, &["add", "z.txt"]);
        assert!(
            p.join("hook-ran.txt").exists(),
            "control: the hook should run without the override"
        );
    }

    #[test]
    fn a_second_staging_write_is_refused_while_one_runs() {
        let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        let Some((dir, g)) = repo() else { return };
        std::fs::write(dir.path().join("y.txt"), "y\n").unwrap();
        let held = STAGING.lock().unwrap();
        let empty = tempfile::tempdir().unwrap();
        let r = stage(
            &g,
            dir.path(),
            "y.txt",
            None,
            StageOp::Stage,
            &PrivateRoots::default(),
            empty.path(),
        );
        drop(held);
        assert_eq!(r.unwrap_err(), StageError::Busy);
    }

    #[test]
    fn a_conflicted_path_is_never_staged() {
        let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        let Some((dir, g)) = repo() else { return };
        let p = dir.path();
        std::fs::write(p.join("c.txt"), "base\n").unwrap();
        sh(&g, p, &["add", "c.txt"]);
        sh(&g, p, &["commit", "-q", "-m", "base"]);
        sh(&g, p, &["checkout", "-q", "-b", "other"]);
        std::fs::write(p.join("c.txt"), "theirs\n").unwrap();
        sh(&g, p, &["commit", "-q", "-am", "theirs"]);
        sh(&g, p, &["checkout", "-q", "main"]);
        std::fs::write(p.join("c.txt"), "ours\n").unwrap();
        sh(&g, p, &["commit", "-q", "-am", "ours"]);
        let merge = g.write(p, &["merge", "-q", "other"]).unwrap();
        assert_ne!(
            merge.status, 0,
            "the merge must conflict for this test to mean anything"
        );
        let v = read_changes(&g, p, &PrivateRoots::default()).unwrap();
        assert!(v
            .records
            .iter()
            .any(|r| r.path == "c.txt" && r.section == Section::Conflicts));
        let empty = tempfile::tempdir().unwrap();
        let r = stage(
            &g,
            p,
            "c.txt",
            None,
            StageOp::Stage,
            &PrivateRoots::default(),
            empty.path(),
        );
        assert!(matches!(r, Err(StageError::Invalid(_))), "{r:?}");
        let after = read_changes(&g, p, &PrivateRoots::default()).unwrap();
        assert!(
            after
                .records
                .iter()
                .any(|r| r.path == "c.txt" && r.section == Section::Conflicts),
            "still unresolved"
        );
    }

    #[test]
    fn a_folder_outside_any_repository_reports_no_repository() {
        let Some(g) = git() else { return };
        let dir = tempfile::tempdir().unwrap();
        // tempdir may sit inside a repository on some machines; only assert when it does not.
        if let Err(e) = read_changes(&g, dir.path(), &PrivateRoots::default()) {
            assert_eq!(e, GitError::NotARepository);
        }
    }
}
