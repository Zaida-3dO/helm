//! The list of checkouts whose repository has worktrees switched off.
//!
//! A tab-separated text file in the profile: one line per checkout, `<repo id><TAB><path>`.
//! Helm rewrites it whenever a switch or the repository list changes; the git stand-in reads it
//! afresh on every call, so a change applies to a chat that is already running. An empty or
//! missing file means no repository is off.

use crate::rules::path_key;
use std::io;
use std::path::Path;

/// Where the list lives in the profile folder.
pub const FILE_NAME: &str = "worktrees-off.tsv";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Line {
    pub repo_id: String,
    pub path: String,
}

/// Lines whose id or path would break the format (a tab or line break) are left out; paths are
/// de-duplicated.
pub fn render(lines: &[Line]) -> String {
    let mut seen = std::collections::BTreeSet::new();
    let mut out = String::new();
    for l in lines {
        let bad = |s: &str| s.contains(['\t', '\n', '\r']) || s.is_empty();
        if bad(&l.repo_id) || bad(&l.path) || !seen.insert(path_key(&l.path)) {
            continue;
        }
        out.push_str(&format!("{}\t{}\n", l.repo_id, l.path));
    }
    out
}

pub fn parse(text: &str) -> Vec<Line> {
    text.lines()
        .filter_map(|l| {
            let (id, path) = l.split_once('\t')?;
            (!id.is_empty() && !path.is_empty()).then(|| Line {
                repo_id: id.to_owned(),
                path: path.to_owned(),
            })
        })
        .collect()
}

/// Read the list; a missing or unreadable file is an empty list.
pub fn read(file: &Path) -> Vec<Line> {
    std::fs::read_to_string(file)
        .map(|t| parse(&t))
        .unwrap_or_default()
}

/// Write the list only when its bytes change (atomic: a temp file, then a rename).
pub fn write(file: &Path, lines: &[Line]) -> io::Result<bool> {
    let text = render(lines);
    if std::fs::read_to_string(file).ok().as_deref() == Some(text.as_str()) {
        return Ok(false);
    }
    if text.is_empty() && !file.exists() {
        return Ok(false);
    }
    let tmp = file.with_extension("tmp");
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, file)?;
    Ok(true)
}

/// The repo id of the first line whose path is one of `candidates`.
pub fn find<'a>(lines: &'a [Line], candidates: &[String]) -> Option<&'a Line> {
    let keys: Vec<String> = candidates.iter().map(path_key).collect();
    lines.iter().find(|l| keys.contains(&path_key(&l.path)))
}

/// A checkout path as git and the list see it: resolved on disk when possible (without the
/// `\\?\` prefix), else as given, so a clone made later still matches.
pub fn plain_canonical(p: &Path) -> String {
    let c = std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    let s = c.to_string_lossy().into_owned();
    s.strip_prefix(r"\\?\").map(str::to_owned).unwrap_or(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_list_round_trips_and_matches_by_path() {
        let lines = vec![
            Line {
                repo_id: "github.com/o/a".into(),
                path: "C:/code/a".into(),
            },
            Line {
                repo_id: "bad\tid".into(),
                path: "C:/code/b".into(),
            },
            Line {
                repo_id: "dup".into(),
                path: "c:\\code\\a\\".into(),
            },
            Line {
                repo_id: "github.com/o/c".into(),
                path: "C:/code/c".into(),
            },
        ];
        let text = render(&lines);
        assert_eq!(
            text,
            "github.com/o/a\tC:/code/a\ngithub.com/o/c\tC:/code/c\n"
        );
        let back = parse(&text);
        assert_eq!(back.len(), 2);
        let hit = find(&back, &["C:\\code\\c".into(), "C:/elsewhere".into()]);
        assert_eq!(hit.map(|l| l.repo_id.as_str()), Some("github.com/o/c"));
        assert!(find(&back, &["C:/code/other".into()]).is_none());

        let d = tempfile::tempdir().unwrap();
        let f = d.path().join(FILE_NAME);
        assert!(
            !write(&f, &[]).unwrap(),
            "nothing to write for an empty list"
        );
        assert!(write(&f, &lines).unwrap());
        assert!(
            !write(&f, &lines).unwrap(),
            "unchanged bytes are not rewritten"
        );
        assert_eq!(read(&f).len(), 2);
        assert!(write(&f, &[]).unwrap());
        assert!(read(&f).is_empty());
        assert!(read(&d.path().join("missing")).is_empty());
    }
}
