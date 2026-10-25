//! Links inside a worktree, and taking them out safely.
//!
//! Folders such as `node_modules` or `target` inside a worktree are often links (symlinks, or
//! junctions on Windows) to a shared folder. A recursive delete that follows one deletes the
//! shared folder's contents, so removal never goes through a link: every link is removed as a
//! directory entry first, and only then does git remove the rest.
//!
//! Unknown means keep: a link that cannot be read, points nowhere, or cannot be removed makes
//! the caller keep the whole worktree.

use helm_platform::fs::is_link_like;
use std::io;
use std::path::{Path, PathBuf};

/// Folders that usually hold shared dependencies or build output. Links one level below the top
/// (`ui/node_modules` in a workspace) are found when they have one of these names.
const KNOWN: &[&str] = &[
    "node_modules",
    "target",
    "dist",
    "build",
    ".venv",
    "venv",
    "vendor",
    ".next",
    ".cache",
];

/// Every link entry in the worktree: anything at the top level, and the known dependency and
/// build folders one level down. Real folders are looked into; links are never followed.
pub fn link_entries(worktree: &Path) -> io::Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir(worktree)? {
        let entry = entry?;
        if entry.file_name() == ".git" {
            continue;
        }
        let path = entry.path();
        let meta = std::fs::symlink_metadata(&path)?;
        if is_link_like(&meta) {
            out.push(path);
            continue;
        }
        if !meta.is_dir() {
            continue;
        }
        let Ok(inner) = std::fs::read_dir(&path) else {
            continue;
        };
        for child in inner.flatten() {
            let name = child.file_name().to_string_lossy().to_lowercase();
            if !KNOWN.contains(&name.as_str()) {
                continue;
            }
            let p = child.path();
            if std::fs::symlink_metadata(&p).is_ok_and(|m| is_link_like(&m)) {
                out.push(p);
            }
        }
    }
    out.sort();
    Ok(out)
}

/// Where a link points, or why that cannot be known.
fn target_of(link: &Path) -> Result<PathBuf, String> {
    let t = std::fs::read_link(link)
        .map_err(|e| format!("cannot read the link {}: {e}", link.display()))?;
    Ok(if t.is_absolute() {
        t
    } else {
        link.parent().map(|p| p.join(&t)).unwrap_or(t)
    })
}

/// Remove a link itself, never what it points at: `remove_dir` takes a junction or a directory
/// link away as an entry (and fails on a real, non-empty folder), `remove_file` a file link.
fn remove_link(link: &Path) -> io::Result<()> {
    std::fs::remove_dir(link).or_else(|_| std::fs::remove_file(link))?;
    match std::fs::symlink_metadata(link) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        _ => Err(io::Error::other("the link is still there")),
    }
}

/// Take out every link in the worktree. Before touching anything, each link must be readable and
/// point at something that exists; after each removal its target must still exist. Returns the
/// links removed, or why the worktree must be kept.
pub fn unlink_all(worktree: &Path) -> Result<Vec<PathBuf>, String> {
    let links = link_entries(worktree).map_err(|e| format!("cannot list the worktree: {e}"))?;
    let mut targets = Vec::new();
    for link in &links {
        let target = target_of(link)?;
        if std::fs::metadata(&target).is_err() {
            return Err(format!(
                "the link {} points at {}, which is missing or unreadable",
                link.display(),
                target.display()
            ));
        }
        targets.push(target);
    }
    for (link, target) in links.iter().zip(&targets) {
        remove_link(link).map_err(|e| format!("cannot remove the link {}: {e}", link.display()))?;
        if std::fs::metadata(target).is_err() {
            return Err(format!(
                "the target of {} is gone after the link was removed; stopping",
                link.display()
            ));
        }
    }
    Ok(links)
}

/// Make a directory link for tests and tools: a junction on Windows (no special rights needed), a
/// symlink elsewhere.
pub fn make_dir_link(link: &Path, target: &Path) -> io::Result<()> {
    #[cfg(windows)]
    {
        let out = std::process::Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(link)
            .arg(target)
            .output()?;
        if out.status.success() {
            Ok(())
        } else {
            Err(io::Error::other(
                String::from_utf8_lossy(&out.stdout).into_owned(),
            ))
        }
    }
    #[cfg(not(windows))]
    {
        std::os::unix::fs::symlink(target, link)
    }
}
