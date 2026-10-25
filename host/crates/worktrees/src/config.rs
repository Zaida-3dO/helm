//! Facts read straight from a checkout's `.git` folder, without starting git: the remote it was
//! cloned from, its default branch, and whether it is a real clone (not a linked worktree).

use std::path::Path;

/// A GitHub repository, as host/owner/name. Only github.com is recognised for now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Remote {
    pub host: String,
    pub owner: String,
    pub name: String,
}

impl Remote {
    /// `github.com/owner/name`, lowercase: the key two clones of one repository share.
    pub fn key(&self) -> String {
        format!("{}/{}/{}", self.host, self.owner, self.name).to_lowercase()
    }

    /// A clean HTTPS URL with no credentials.
    pub fn url(&self) -> String {
        format!("https://{}/{}/{}", self.host, self.owner, self.name)
    }
}

/// Parse an HTTPS, `ssh://` or `git@host:owner/name` GitHub URL. Credentials, ports, extra path
/// parts and every other host give `None`.
pub fn parse_remote(url: &str) -> Option<Remote> {
    let url = url.trim();
    if url.is_empty() || url.len() > 2048 || url.chars().any(char::is_control) {
        return None;
    }
    let (host, path) = if let Some(rest) = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))
        .or_else(|| url.strip_prefix("ssh://"))
        .or_else(|| url.strip_prefix("git://"))
    {
        let (auth_host, path) = rest.split_once('/')?;
        let host = match auth_host.rsplit_once('@') {
            Some((user, h)) if user == "git" || url.starts_with("https://") => h,
            Some(_) => return None,
            None => auth_host,
        };
        (host, path)
    } else {
        let rest = url.strip_prefix("git@")?;
        rest.split_once(':')?
    };
    if !host.eq_ignore_ascii_case("github.com") {
        return None;
    }
    let path = path.trim_end_matches('/');
    let path = path.strip_suffix(".git").unwrap_or(path);
    let mut parts = path.split('/');
    let (owner, name) = (parts.next()?, parts.next()?);
    if parts.next().is_some() {
        return None;
    }
    let ok = |s: &str| {
        !s.is_empty()
            && s.len() <= 100
            && s.chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
    };
    (ok(owner) && ok(name)).then(|| Remote {
        host: "github.com".into(),
        owner: owner.into(),
        name: name.into(),
    })
}

/// True when `dir/.git` is a folder: a real clone. A `.git` file marks a linked worktree or a
/// submodule, which are not checkouts of their own.
pub fn is_clone_root(dir: &Path) -> bool {
    std::fs::symlink_metadata(dir.join(".git")).is_ok_and(|m| m.is_dir())
}

/// The `url` of `[remote "origin"]` in `.git/config`.
pub fn origin_url(dir: &Path) -> Option<String> {
    let text = std::fs::read_to_string(dir.join(".git").join("config")).ok()?;
    let mut in_origin = false;
    for line in text.lines() {
        let l = line.trim();
        if l.starts_with('[') {
            in_origin = l
                .replace(' ', "")
                .eq_ignore_ascii_case("[remote\"origin\"]");
        } else if in_origin {
            if let Some((k, v)) = l.split_once('=') {
                if k.trim().eq_ignore_ascii_case("url") {
                    return Some(v.trim().to_owned());
                }
            }
        }
    }
    None
}

/// The branch `origin/HEAD` points at (`main`), when the clone recorded it.
pub fn default_branch(dir: &Path) -> Option<String> {
    let text = std::fs::read_to_string(dir.join(".git/refs/remotes/origin/HEAD")).ok()?;
    let b = text.trim().strip_prefix("ref: refs/remotes/origin/")?;
    (!b.is_empty() && b.len() <= 200 && !b.chars().any(char::is_control)).then(|| b.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remotes_and_clone_facts() {
        for (url, want) in [
            (
                "https://github.com/Zaida-3dO/helm.git",
                Some("github.com/zaida-3do/helm"),
            ),
            ("https://github.com/o/n", Some("github.com/o/n")),
            (
                "https://user:tok@github.com/o/n.git",
                Some("github.com/o/n"),
            ),
            ("git@github.com:o/n.git", Some("github.com/o/n")),
            ("ssh://git@github.com/o/n.git", Some("github.com/o/n")),
            ("https://github.com/o/n/extra", None),
            ("https://gitlab.com/o/n.git", None),
            ("git@github.com.evil.com:o/n.git", None),
            ("ssh://bob@github.com/o/n.git", None),
            ("C:/local/repo", None),
            ("", None),
        ] {
            assert_eq!(parse_remote(url).map(|r| r.key()).as_deref(), want, "{url}");
        }
        assert_eq!(
            parse_remote("git@github.com:o/n.git").unwrap().url(),
            "https://github.com/o/n"
        );

        let d = tempfile::tempdir().unwrap();
        assert!(!is_clone_root(d.path()));
        let git = d.path().join(".git");
        std::fs::create_dir_all(git.join("refs/remotes/origin")).unwrap();
        std::fs::write(
            git.join("config"),
            "[core]\n\turl = nope\n[remote \"upstream\"]\n\turl = https://github.com/x/y\n[remote \"origin\"]\n\turl = git@github.com:o/n.git\n\tfetch = +refs/heads/*:refs/remotes/origin/*\n",
        )
        .unwrap();
        std::fs::write(
            git.join("refs/remotes/origin/HEAD"),
            "ref: refs/remotes/origin/trunk\n",
        )
        .unwrap();
        assert!(is_clone_root(d.path()));
        assert_eq!(
            origin_url(d.path()).as_deref(),
            Some("git@github.com:o/n.git")
        );
        assert_eq!(default_branch(d.path()).as_deref(), Some("trunk"));
        // A linked worktree has a .git file: not a clone root.
        let w = tempfile::tempdir().unwrap();
        std::fs::write(w.path().join(".git"), "gitdir: x").unwrap();
        assert!(!is_clone_root(w.path()));
    }
}
