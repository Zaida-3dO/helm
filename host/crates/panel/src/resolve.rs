//! Clickable paths: turn a path an agent wrote in
//! a reply into a file the viewer can open.
//!
//! Accepted forms: `~` and `~/…` (the user's home folder), `X:\…` / `X:/…`, Git Bash's `/c/…`,
//! and paths relative to the chat's project folder (`./x`, `../x`, `dir/file.ext`). The drive
//! letter's colon is part of the path; line suffixes were split off by the renderer. `.` and `..`
//! are resolved lexically, then the result goes through the full read gate (lexical check, no
//! links along the path, private roots) before its existence is checked, so a missing file is
//! explained rather than silently ignored.

use crate::private::PrivateRoots;
use crate::viewer::{admit, ReadError};
use std::path::{Path, PathBuf};

pub const MAX_INPUT: usize = 8 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Resolved {
    File(PathBuf),
    Dir(PathBuf),
}

/// Lexically joins `rel` onto `base`, resolving `.` and `..` (never above the drive or root).
fn join_lexically(base: &Path, rel: &str) -> PathBuf {
    let base_s = base.to_string_lossy().replace('/', "\\");
    let mut parts: Vec<String> = base_s.split('\\').map(str::to_owned).collect();
    let root = parts.remove(0);
    parts.retain(|p| !p.is_empty());
    for seg in rel.replace('/', "\\").split('\\') {
        match seg {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            s => parts.push(s.to_owned()),
        }
    }
    let mut out = format!("{root}\\");
    out.push_str(&parts.join("\\"));
    PathBuf::from(out)
}

/// Turns the written form into an absolute path, without touching the disk. `home` is the user's
/// profile folder and `folder` the chat's project folder (either may be unknown).
pub fn to_absolute(
    raw: &str,
    home: Option<&Path>,
    folder: Option<&Path>,
) -> Result<PathBuf, String> {
    let s = raw.trim();
    if s.is_empty() || s.len() > MAX_INPUT || s.chars().any(|c| c.is_control()) {
        return Err("This is not a file path Helm can open.".into());
    }
    let n = s.replace('/', "\\");
    if n.starts_with("\\\\") || n.starts_with("\\??\\") {
        return Err(crate::pathgate::PathRefusal::Share.message().into());
    }
    let b = n.as_bytes();
    // `X:\…`: already absolute; normalise `.`/`..` lexically.
    if b.len() >= 3 && b[0].is_ascii_alphabetic() && b[1] == b':' && b[2] == b'\\' {
        return Ok(join_lexically(Path::new(&n[..3]), &n[3..]));
    }
    if b.len() >= 2 && b[0].is_ascii_alphabetic() && b[1] == b':' {
        return Err(crate::pathgate::PathRefusal::NotAbsolute.message().into());
    }
    if n == "~" || n.starts_with("~\\") {
        let home = home.ok_or("Your home folder is not known.")?;
        return Ok(join_lexically(home, n.trim_start_matches('~')));
    }
    // Git Bash: `/c/data/x` is `C:\data\x`.
    if b.len() >= 3 && b[0] == b'\\' && b[1].is_ascii_alphabetic() && b[2] == b'\\' {
        let drive = format!("{}:\\", (b[1] as char).to_ascii_uppercase());
        return Ok(join_lexically(Path::new(&drive), &n[3..]));
    }
    if n.starts_with('\\') {
        return Err("Paths starting with / name no drive, so Helm cannot open them.".into());
    }
    let folder = folder.ok_or("This chat has no project folder to resolve the path against.")?;
    Ok(join_lexically(folder, &n))
}

/// Resolves and checks a written path. Errors are sentences for the user.
pub fn resolve(
    raw: &str,
    home: Option<&Path>,
    folder: Option<&Path>,
    roots: &PrivateRoots,
) -> Result<Resolved, String> {
    let abs = to_absolute(raw, home, folder)?;
    let shown = abs.to_string_lossy().into_owned();
    let path = admit(&shown, roots).map_err(|e| match e {
        ReadError::NotFound => format!("Not found: {shown}"),
        other => other.message(),
    })?;
    match std::fs::symlink_metadata(&path) {
        Ok(m) if m.is_dir() => Ok(Resolved::Dir(path)),
        Ok(_) => Ok(Resolved::File(path)),
        Err(_) => Err(format!("Not found: {shown}")),
    }
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;

    #[test]
    fn written_forms_become_absolute_paths_without_touching_the_disk() {
        let home = Path::new(r"C:\home\someone");
        let folder = Path::new(r"D:\work\proj");
        let abs = |s: &str| to_absolute(s, Some(home), Some(folder));
        assert_eq!(abs("~").unwrap(), PathBuf::from(r"C:\home\someone"));
        assert_eq!(
            abs("~/notes/a.md").unwrap(),
            PathBuf::from(r"C:\home\someone\notes\a.md")
        );
        assert_eq!(abs("C:/x/../y/z.ts").unwrap(), PathBuf::from(r"C:\y\z.ts"));
        assert_eq!(abs(r"C:\x\y.ts").unwrap(), PathBuf::from(r"C:\x\y.ts"));
        assert_eq!(
            abs("/c/data/x.txt").unwrap(),
            PathBuf::from(r"C:\data\x.txt")
        );
        assert_eq!(
            abs("src/app.ts").unwrap(),
            PathBuf::from(r"D:\work\proj\src\app.ts")
        );
        assert_eq!(
            abs("./src/app.ts").unwrap(),
            PathBuf::from(r"D:\work\proj\src\app.ts")
        );
        assert_eq!(
            abs("../other/x.rs").unwrap(),
            PathBuf::from(r"D:\work\other\x.rs")
        );
        assert_eq!(
            abs("../../../../x").unwrap(),
            PathBuf::from(r"D:\x"),
            "never above the drive"
        );
    }

    #[test]
    fn shares_device_paths_and_incomplete_forms_are_refused_before_anything_else() {
        let home = Path::new(r"C:\home\someone");
        for s in [
            r"\\host\share\x",
            "//host/share/x",
            r"\\?\C:\x",
            r"\??\C:\x",
            "C:x",
            "/rooted",
            "",
            "a\u{7}b",
        ] {
            assert!(to_absolute(s, Some(home), Some(home)).is_err(), "{s:?}");
        }
        assert!(
            to_absolute("src/a.ts", Some(home), None).is_err(),
            "no folder for a relative path"
        );
        assert!(to_absolute("~/a", None, None).is_err(), "no home for ~");
    }

    #[test]
    fn resolution_checks_the_gate_private_roots_and_existence() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::write(dir.path().join("src").join("a.ts"), "x").unwrap();
        std::fs::create_dir_all(dir.path().join("Helm")).unwrap();
        std::fs::write(dir.path().join("Helm").join("db"), "x").unwrap();
        let roots = PrivateRoots::new(&[dir.path().join("Helm")]);
        let f = Some(dir.path());
        assert_eq!(
            resolve("src/a.ts", None, f, &roots).unwrap(),
            Resolved::File(dir.path().join("src").join("a.ts"))
        );
        assert_eq!(
            resolve("./src", None, f, &roots).unwrap(),
            Resolved::Dir(dir.path().join("src"))
        );
        let missing = resolve("src/nope.ts", None, f, &roots).unwrap_err();
        assert!(missing.starts_with("Not found: "), "{missing}");
        assert_eq!(
            resolve("Helm/db", None, f, &roots).unwrap_err(),
            crate::viewer::PRIVATE_MESSAGE
        );
        assert!(resolve("src/NUL", None, f, &roots).is_err());
    }
}
