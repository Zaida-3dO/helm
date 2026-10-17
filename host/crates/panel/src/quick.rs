//! Quick-open: `quick_open {chatId?, query}`.
//!
//! - A query that looks like a path (`~`, a drive, `/`, `./`, `../`, or anything with a slash)
//!   completes the folder it names: the entries whose name starts with the last segment (then
//!   those that contain it), folders first, at most 50.
//! - Any other query ranks the chat's project files by fuzzy subsequence match. The index is a
//!   bounded walk of the project folder: it never follows links, skips dot-folders and build noise,
//!   stops at 100,000 names or 3 seconds (the result says when it was partial), and is cached for
//!   60 s for up to 3 projects.

use crate::private::PrivateRoots;
use crate::resolve::to_absolute;
use crate::viewer::{admit, list_dir};
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

pub const MAX_RESULTS: usize = 50;
pub const INDEX_LIMIT: usize = 100_000;
pub const INDEX_DEADLINE: Duration = Duration::from_secs(3);
pub const INDEX_TTL: Duration = Duration::from_secs(60);
const CACHE_PROJECTS: usize = 3;
const SKIP_DIRS: &[&str] = &[
    "node_modules",
    "target",
    "dist",
    "build",
    "out",
    "bin",
    "obj",
    "__pycache__",
    "vendor",
];

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct QuickItem {
    pub path: String,
    /// Shown text: relative to the project in fuzzy mode, the entry name in path mode.
    pub label: String,
    pub kind: &'static str,
    pub score: i64,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct QuickResult {
    pub mode: &'static str,
    pub items: Vec<QuickItem>,
    pub partial: bool,
}

pub fn looks_like_path(q: &str) -> bool {
    let b = q.as_bytes();
    q.starts_with('~')
        || q.starts_with('.')
        || q.contains('/')
        || q.contains('\\')
        || (b.len() >= 2 && b[0].is_ascii_alphabetic() && b[1] == b':')
}

/// Path mode: complete the folder the query names.
pub fn complete(
    q: &str,
    home: Option<&Path>,
    folder: Option<&Path>,
    roots: &PrivateRoots,
) -> Result<QuickResult, String> {
    let n = q.replace('/', "\\");
    let (dir_part, prefix) = match n.rfind('\\') {
        Some(i) => (&n[..=i], &n[i + 1..]),
        None if n == "~" => ("~", ""),
        None => (".", n.as_str()),
    };
    let dir = to_absolute(dir_part, home, folder)?;
    let dir = admit(&dir.to_string_lossy(), roots).map_err(|e| e.message())?;
    let (entries, truncated) = list_dir(&dir, roots).map_err(|e| e.message())?;
    let p = prefix.to_lowercase();
    let mut items: Vec<QuickItem> = entries
        .into_iter()
        .filter_map(|e| {
            let name = e.name.to_lowercase();
            let score = if name.starts_with(&p) {
                2
            } else if !p.is_empty() && name.contains(&p) {
                1
            } else {
                return None;
            };
            Some(QuickItem {
                path: dir.join(&e.name).to_string_lossy().into_owned(),
                label: e.name,
                kind: e.kind,
                score: score * 10 + i64::from(e.kind == "dir"),
            })
        })
        .collect();
    items.sort_by(|a, b| {
        b.score
            .cmp(&a.score)
            .then_with(|| a.label.to_lowercase().cmp(&b.label.to_lowercase()))
    });
    items.truncate(MAX_RESULTS);
    Ok(QuickResult {
        mode: "path",
        items,
        partial: truncated,
    })
}

/// Fuzzy subsequence score of `q` against `cand` (both lowercase), or None when it does not match.
/// Name matches beat path matches; word starts and contiguous runs score higher; shorter wins ties.
pub fn fuzzy_score(q: &str, cand: &str) -> Option<i64> {
    if q.is_empty() {
        return None;
    }
    let name_at = cand.rfind(['\\', '/']).map(|i| i + 1).unwrap_or(0);
    let run = |hay: &str| -> Option<i64> {
        let hb: Vec<char> = hay.chars().collect();
        let mut score = 0i64;
        let mut hi = 0usize;
        let mut prev: Option<usize> = None;
        for qc in q.chars() {
            let found = (hi..hb.len()).find(|&i| hb[i] == qc)?;
            let at_word = found == 0 || matches!(hb[found - 1], '\\' | '/' | '_' | '-' | '.' | ' ');
            score += 1;
            if at_word {
                score += 8;
            }
            if prev == Some(found.wrapping_sub(1)) {
                score += 5;
            }
            prev = Some(found);
            hi = found + 1;
        }
        Some(score)
    };
    if let Some(s) = run(&cand[name_at..]) {
        return Some(1000 + s * 10 - cand.len() as i64);
    }
    run(cand).map(|s| s * 10 - cand.len() as i64)
}

struct Index {
    root: PathBuf,
    built: Instant,
    names: Arc<Vec<String>>,
    partial: bool,
}

fn cache() -> &'static Mutex<Vec<Index>> {
    static C: OnceLock<Mutex<Vec<Index>>> = OnceLock::new();
    C.get_or_init(|| Mutex::new(Vec::new()))
}

/// A bounded walk: relative file paths under `root`, never through links.
pub fn walk(root: &Path, limit: usize, deadline: Duration) -> (Vec<String>, bool) {
    let start = Instant::now();
    let mut out = Vec::new();
    let mut stack = vec![PathBuf::new()];
    while let Some(rel) = stack.pop() {
        if start.elapsed() > deadline || out.len() >= limit {
            return (out, true);
        }
        let Ok(rd) = std::fs::read_dir(root.join(&rel)) else {
            continue;
        };
        for entry in rd.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            let Ok(meta) = entry.metadata() else { continue };
            if helm_platform::fs::is_link_like(&meta) || name.chars().any(char::is_control) {
                continue;
            }
            let child = rel.join(&name);
            if meta.is_dir() {
                if name.starts_with('.') || SKIP_DIRS.contains(&name.to_lowercase().as_str()) {
                    continue;
                }
                stack.push(child);
            } else if meta.is_file() {
                out.push(child.to_string_lossy().into_owned());
                if out.len() >= limit {
                    return (out, true);
                }
            }
        }
    }
    (out, false)
}

fn index_for(root: &Path, rebuild: bool) -> (Arc<Vec<String>>, bool) {
    let mut c = cache().lock().unwrap_or_else(|e| e.into_inner());
    c.retain(|i| i.built.elapsed() < INDEX_TTL);
    if !rebuild {
        if let Some(i) = c.iter().find(|i| i.root == root) {
            return (i.names.clone(), i.partial);
        }
    }
    let (names, partial) = walk(root, INDEX_LIMIT, INDEX_DEADLINE);
    let names = Arc::new(names);
    c.retain(|i| i.root != root);
    c.push(Index {
        root: root.to_path_buf(),
        built: Instant::now(),
        names: names.clone(),
        partial,
    });
    while c.len() > CACHE_PROJECTS {
        c.remove(0);
    }
    (names, partial)
}

/// Fuzzy mode over the chat's project files.
pub fn fuzzy(
    q: &str,
    folder: &Path,
    roots: &PrivateRoots,
    rebuild: bool,
) -> Result<QuickResult, String> {
    let folder = admit(&folder.to_string_lossy(), roots).map_err(|e| e.message())?;
    let (names, partial) = index_for(&folder, rebuild);
    let ql = q.to_lowercase();
    let mut items: Vec<QuickItem> = names
        .iter()
        .filter_map(|rel| {
            fuzzy_score(&ql, &rel.to_lowercase()).map(|score| QuickItem {
                path: folder.join(rel).to_string_lossy().into_owned(),
                label: rel.clone(),
                kind: "file",
                score,
            })
        })
        .collect();
    items.sort_by(|a, b| b.score.cmp(&a.score).then_with(|| a.label.cmp(&b.label)));
    items.truncate(MAX_RESULTS);
    Ok(QuickResult {
        mode: "files",
        items,
        partial,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fuzzy_prefers_name_matches_word_starts_and_runs() {
        let a = fuzzy_score("app", r"src\app.ts").unwrap();
        let b = fuzzy_score("app", r"app\src\main.ts").unwrap();
        assert!(a > b, "a match in the file name beats one in a folder name");
        let c = fuzzy_score("mt", r"src\main_test.rs").unwrap();
        let d = fuzzy_score("mt", r"src\format.rs").unwrap();
        assert!(c > d, "word starts beat mid-word letters");
        assert!(fuzzy_score("xyz", r"src\app.ts").is_none());
        assert!(fuzzy_score("", "a").is_none());
    }

    #[test]
    fn the_walk_skips_noise_and_links_and_stops_at_its_limit() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::create_dir_all(dir.path().join("node_modules").join("x")).unwrap();
        std::fs::create_dir_all(dir.path().join(".git")).unwrap();
        std::fs::write(dir.path().join("src").join("a.ts"), "").unwrap();
        std::fs::write(dir.path().join("node_modules").join("x").join("b.js"), "").unwrap();
        std::fs::write(dir.path().join(".git").join("HEAD"), "").unwrap();
        std::fs::write(dir.path().join("README.md"), "").unwrap();
        let (mut names, partial) = walk(dir.path(), 100, INDEX_DEADLINE);
        names.sort();
        assert_eq!(
            names,
            ["README.md", r"src\a.ts"].map(|s| s.replace('\\', std::path::MAIN_SEPARATOR_STR))
        );
        assert!(!partial);
        let (names, partial) = walk(dir.path(), 1, INDEX_DEADLINE);
        assert_eq!(names.len(), 1);
        assert!(partial);
    }

    #[cfg(windows)]
    #[test]
    fn path_queries_complete_the_named_folder() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("src").join("components")).unwrap();
        std::fs::write(dir.path().join("src").join("app.ts"), "").unwrap();
        std::fs::write(dir.path().join("src").join("map.ts"), "").unwrap();
        let roots = PrivateRoots::default();
        let r = complete("src/", None, Some(dir.path()), &roots).unwrap();
        let labels: Vec<_> = r.items.iter().map(|i| i.label.as_str()).collect();
        assert_eq!(labels, ["components", "app.ts", "map.ts"]);
        let r = complete("./src/ap", None, Some(dir.path()), &roots).unwrap();
        let labels: Vec<_> = r.items.iter().map(|i| i.label.as_str()).collect();
        assert_eq!(
            labels,
            ["app.ts", "map.ts"],
            "prefix matches first, then contains"
        );
        assert!(complete(r"\\host\share\", None, Some(dir.path()), &roots).is_err());
        assert!(
            looks_like_path("~/x")
                && looks_like_path("C:")
                && looks_like_path("a/b")
                && !looks_like_path("app")
        );
    }

    #[test]
    fn fuzzy_mode_ranks_project_files() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::write(dir.path().join("src").join("app.ts"), "").unwrap();
        std::fs::write(dir.path().join("happy.txt"), "").unwrap();
        let r = fuzzy("app", dir.path(), &PrivateRoots::default(), true).unwrap();
        assert_eq!(
            r.items[0].label,
            format!("src{}app.ts", std::path::MAIN_SEPARATOR)
        );
        assert_eq!(r.items.len(), 2);
    }
}
