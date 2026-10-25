//! Where an agent's worktree goes and what it may be called, so a checkout plus `node_modules`
//! stays under Windows' 260-character path limit for tools that do not support long paths.
//!
//! Helm never turns long-path support on (the OS switch and git's `core.longpaths` are machine
//! and repository settings it does not write); it keeps paths short instead.
//!
//! The default place is `C:\w\<repo-short>\<name>`; the root is configurable.

use std::path::{Component, Path, PathBuf};

/// A worktree name is at most this many characters.
pub const MAX_NAME: usize = 32;
/// The whole worktree path is at most this many characters.
pub const MAX_PATH_CHARS: usize = 60;
/// The repository's folder under the root is at most this many characters.
pub const MAX_REPO_SHORT: usize = 12;

/// Names Windows reserves for devices, whatever the extension.
const RESERVED: &[&str] = &[
    "con", "prn", "aux", "nul", "com1", "com2", "com3", "com4", "com5", "com6", "com7", "com8",
    "com9", "lpt1", "lpt2", "lpt3", "lpt4", "lpt5", "lpt6", "lpt7", "lpt8", "lpt9",
];

/// Why a name or a planned path is refused. The text is meant for the agent or the user.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Problem {
    Name(String),
    Root(String),
    TooLong { chars: usize, max: usize },
}

impl std::fmt::Display for Problem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Problem::Name(m) | Problem::Root(m) => f.write_str(m),
            Problem::TooLong { chars, max } => write!(
                f,
                "That path is {chars} characters; worktree paths stay within {max} so builds do not hit Windows' path limit. Use a shorter name or a shorter root."
            ),
        }
    }
}

/// The default root for worktrees: `C:\w` on Windows, `~/w` elsewhere.
pub fn default_root() -> PathBuf {
    if cfg!(windows) {
        PathBuf::from(r"C:\w")
    } else {
        std::env::var_os("HOME")
            .map(|h| PathBuf::from(h).join("w"))
            .unwrap_or_else(|| PathBuf::from("/tmp/w"))
    }
}

/// A worktree name: 1 to 32 characters of `a-z`, `0-9` and `-`; no leading or trailing hyphen
/// (a leading one reads as a git option) and not a reserved device name.
pub fn check_name(name: &str) -> Result<(), Problem> {
    let bad = |m: &str| Err(Problem::Name(m.into()));
    if name.is_empty() || name.chars().count() > MAX_NAME {
        return bad("A worktree name is 1 to 32 characters.");
    }
    if !name
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
    {
        return bad("A worktree name uses only lowercase letters, digits and hyphens.");
    }
    if name.starts_with('-') || name.ends_with('-') {
        return bad("A worktree name does not start or end with a hyphen.");
    }
    if RESERVED.contains(&name) {
        return bad("That name is reserved by Windows; pick another.");
    }
    Ok(())
}

/// Turn free text (the change being made) into a valid name: lowercase, hyphens between words,
/// at most 32 characters. Empty when nothing usable is left.
pub fn slug(text: &str) -> String {
    let mut out = String::new();
    for c in text.chars().flat_map(char::to_lowercase) {
        if c.is_ascii_lowercase() || c.is_ascii_digit() {
            out.push(c);
        } else if !out.ends_with('-') && !out.is_empty() {
            out.push('-');
        }
    }
    let cut: String = out.chars().take(MAX_NAME).collect();
    let name = cut.trim_matches('-').to_owned();
    if RESERVED.contains(&name.as_str()) {
        format!("{name}-wt")
    } else {
        name
    }
}

/// The short folder name for a repository under the root: its slug, at most 12 characters.
pub fn repo_short(repo: &str) -> String {
    let base = repo.rsplit(['/', '\\']).next().unwrap_or(repo);
    let s: String = slug(base).chars().take(MAX_REPO_SHORT).collect();
    let s = s.trim_matches('-').to_owned();
    if s.is_empty() {
        "repo".into()
    } else {
        s
    }
}

/// A root must be an absolute path with no `.`/`..` parts and no reserved or trailing-dot names.
pub fn check_root(root: &Path) -> Result<(), Problem> {
    let bad = |m: &str| Err(Problem::Root(m.into()));
    if !root.is_absolute() {
        return bad("The worktree root must be an absolute folder path.");
    }
    for c in root.components() {
        match c {
            Component::CurDir | Component::ParentDir => {
                return bad("The worktree root cannot contain . or ..");
            }
            Component::Normal(n) => {
                let s = n.to_string_lossy();
                let stem = s.split('.').next().unwrap_or("").to_lowercase();
                if s.ends_with('.') || s.ends_with(' ') || RESERVED.contains(&stem.as_str()) {
                    return bad("The worktree root has a folder name Windows does not allow.");
                }
            }
            _ => {}
        }
    }
    Ok(())
}

/// The characters in the path as a user counts them.
pub fn path_chars(path: &Path) -> usize {
    path.to_string_lossy().chars().count()
}

/// Refuse a path over the 60-character budget.
pub fn check_budget(path: &Path) -> Result<(), Problem> {
    let chars = path_chars(path);
    if chars > MAX_PATH_CHARS {
        Err(Problem::TooLong {
            chars,
            max: MAX_PATH_CHARS,
        })
    } else {
        Ok(())
    }
}

/// Where a new worktree of `repo` called `name` goes: `<root>/<repo-short>/<name>`. Checks the
/// root, the name and the length budget, so what it returns is safe to hand to git.
pub fn plan_path(root: &Path, repo: &str, name: &str) -> Result<PathBuf, Problem> {
    check_root(root)?;
    check_name(name)?;
    let path = root.join(repo_short(repo)).join(name);
    check_budget(&path)?;
    Ok(path)
}

/// A comparison key for a path: separators as `/`, no trailing slash, no `\\?\` prefix, and
/// lowercase on Windows (where paths are case-insensitive).
pub fn path_key(p: impl AsRef<str>) -> String {
    let s = p.as_ref().trim();
    let s = s.strip_prefix(r"\\?\").unwrap_or(s);
    let s = s.replace('\\', "/");
    let s = s.trim_end_matches('/');
    let s = if s.is_empty() { "/" } else { s };
    if cfg!(windows) {
        s.to_lowercase()
    } else {
        s.to_owned()
    }
}

/// True when `a` and `b` name the same path by [`path_key`].
pub fn same_path(a: &str, b: &str) -> bool {
    path_key(a) == path_key(b)
}

/// True when `inner` is `outer` or inside it, by [`path_key`] (no disk access).
pub fn is_within(inner: &str, outer: &str) -> bool {
    let (i, o) = (path_key(inner), path_key(outer));
    i == o
        || i.strip_prefix(&o)
            .is_some_and(|r| r.starts_with('/') || o.ends_with('/'))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Names, roots, the length budget and path comparison, one row each.
    #[test]
    fn naming_length_and_path_rules() {
        // (name, accepted)
        for (name, ok) in [
            ("fix-login", true),
            ("a", true),
            ("v04-repos", true),
            (&"a".repeat(32)[..], true),
            (&"a".repeat(33)[..], false),
            ("", false),
            ("Fix", false),
            ("fix_login", false),
            ("fix login", false),
            ("-fix", false),
            ("fix-", false),
            ("nul", false),
            ("com1", false),
            ("é", false),
            ("a/b", false),
        ] {
            assert_eq!(check_name(name).is_ok(), ok, "name {name:?}");
        }

        for (text, want) in [
            ("Fix the Login bug!", "fix-the-login-bug"),
            ("  --weird__name  ", "weird-name"),
            ("", ""),
            ("nul", "nul-wt"),
            (&"word ".repeat(20)[..], "word-word-word-word-word-word-wo"),
        ] {
            assert_eq!(slug(text), want, "slug of {text:?}");
        }
        assert_eq!(repo_short("owner/Helm Desktop App Extra"), "helm-desktop");
        assert_eq!(repo_short("///"), "repo");

        // Planned paths: default shape, budget, bad root, bad name.
        let root = if cfg!(windows) {
            PathBuf::from(r"C:\w")
        } else {
            PathBuf::from("/w")
        };
        let p = plan_path(&root, "github.com/o/helm", "v04-repos").unwrap();
        assert!(p.ends_with(Path::new("helm").join("v04-repos")), "{p:?}");
        assert!(path_chars(&p) <= MAX_PATH_CHARS);
        // The longest name under the default root still fits; a long root does not.
        assert!(plan_path(&root, &"x".repeat(40), &"a".repeat(32)).is_ok());
        let long_root = root.join("a-rather-long-folder-name-for-worktrees");
        assert!(matches!(
            plan_path(&long_root, "helm", &"a".repeat(32)),
            Err(Problem::TooLong { .. })
        ));
        assert!(matches!(
            plan_path(Path::new("relative/w"), "helm", "x"),
            Err(Problem::Root(_))
        ));
        assert!(matches!(
            plan_path(&root.join(".."), "helm", "x"),
            Err(Problem::Root(_))
        ));
        assert!(matches!(
            plan_path(&root, "helm", "Bad Name"),
            Err(Problem::Name(_))
        ));
        assert!(check_root(&root.join("con")).is_err());

        // Comparison: separators, trailing slash and the \\?\ prefix never matter; case does
        // not on Windows.
        assert!(same_path("C:\\w\\helm\\x\\", "C:/w/helm/x"));
        assert!(same_path(r"\\?\C:\w\helm", "C:/w/helm"));
        assert_eq!(same_path("C:/w/Helm", "c:/w/helm"), cfg!(windows));
        assert!(!same_path("C:/w/helm", "C:/w/helm2"));
        assert!(is_within("C:/w/helm/x/y", "C:/w/helm"));
        assert!(is_within("C:/w/helm", "C:/w/helm"));
        assert!(!is_within("C:/w/helm2", "C:/w/helm"));
    }
}
