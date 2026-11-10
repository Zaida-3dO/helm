//! Rules: instruction files a chat is told at launch.
//!
//! A layer's `rules/` folder holds one Markdown file per rule. A rule's title is the file's first
//! `# ` line (the file name when it has none); the rest is the instruction. Rules are plain text
//! the model reads, so a write is screened for invisible and direction-changing characters, is
//! made atomically (a temporary file renamed into place) and is capped. Every function takes a
//! layer's root folder.

use serde::Serialize;
use std::fs;
use std::io::{self, ErrorKind};
use std::path::{Path, PathBuf};

/// One rule file holds at most this many bytes.
pub const MAX_RULE_FILE: u64 = 64 * 1024;
/// A rule written by an agent holds at most this many bytes of text.
pub const MAX_AGENT_RULE_TEXT: usize = 16 * 1024;
/// A title is at most this many characters.
pub const MAX_TITLE_CHARS: usize = 120;
/// A layer lists at most this many rule files.
const MAX_RULES: usize = 200;
/// File names this long or longer are not rules.
const MAX_FILE_NAME: usize = 100;

/// The rules folder inside a layer's root.
pub fn rules_dir(root: &Path) -> PathBuf {
    root.join("rules")
}

/// One rule as Settings and the launch see it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RuleEntry {
    /// `rules/<file>.md`: what the tools and ops take back.
    pub path: String,
    pub title: String,
    pub size: u64,
    /// A hash of the file's text, for an edit that must not overwrite a newer one. Empty when the
    /// file is too large to load.
    pub revision: String,
    /// Over the size limit: listed so it can be fixed, but never loaded into a chat.
    pub too_large: bool,
}

fn invalid(why: &str) -> io::Error {
    io::Error::new(ErrorKind::InvalidInput, why.to_owned())
}

/// A rule's file name, from `rules/<file>.md` or `<file>.md`; the one place a path from outside is
/// turned into a name. A single plain `.md` file name: no folders, no leading dot, no hidden
/// characters and no Windows device name.
pub fn file_name_of(path: &str) -> io::Result<String> {
    let name = path.trim().strip_prefix("rules/").unwrap_or(path.trim());
    let stem = name.strip_suffix(".md").unwrap_or("");
    let device = {
        let upper = stem.split('.').next().unwrap_or("").to_ascii_uppercase();
        matches!(upper.as_str(), "CON" | "PRN" | "AUX" | "NUL")
            || ((upper.starts_with("COM") || upper.starts_with("LPT"))
                && upper.len() == 4
                && upper.as_bytes()[3].is_ascii_digit())
    };
    if stem.is_empty()
        || name.len() > MAX_FILE_NAME
        || name.starts_with('.')
        || name.contains(['/', '\\', ':', '*', '?', '"', '<', '>', '|'])
        || name.ends_with(' ')
        || helm_core::hidden::has_hidden(name)
        || device
    {
        return Err(invalid("that is not a rule file name"));
    }
    Ok(name.to_owned())
}

/// The title of a rule's text: its first `# ` line, else `fallback`.
pub fn title_of(text: &str, fallback: &str) -> String {
    text.strip_prefix('\u{feff}')
        .unwrap_or(text)
        .lines()
        .find(|l| !l.trim().is_empty())
        .and_then(|l| l.strip_prefix("# "))
        .map(|t| t.trim().to_owned())
        .filter(|t| !t.is_empty())
        .unwrap_or_else(|| fallback.to_owned())
}

/// A rule's instruction: its text without the `# ` title line, trimmed.
pub fn body_of(text: &str) -> String {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let trimmed = text.trim_start();
    match trimmed.strip_prefix("# ") {
        Some(rest) => rest
            .split_once('\n')
            .map(|(_, body)| body)
            .unwrap_or("")
            .trim()
            .to_owned(),
        None => text.trim().to_owned(),
    }
}

fn entry(path: &Path, file: &str) -> Option<(RuleEntry, Option<String>)> {
    let meta = fs::symlink_metadata(path).ok()?;
    if helm_platform::fs::is_link_like(&meta) || !meta.is_file() {
        return None;
    }
    let stem = file.strip_suffix(".md").unwrap_or(file);
    if meta.len() > MAX_RULE_FILE {
        return Some((
            RuleEntry {
                path: format!("rules/{file}"),
                title: stem.to_owned(),
                size: meta.len(),
                revision: String::new(),
                too_large: true,
            },
            None,
        ));
    }
    let bytes = helm_core::privfs::read_capped(path, MAX_RULE_FILE).ok()?;
    let text = String::from_utf8_lossy(&bytes).into_owned();
    Some((
        RuleEntry {
            path: format!("rules/{file}"),
            title: title_of(&text, stem),
            size: meta.len(),
            revision: helm_core::sha256_hex(&bytes),
            too_large: false,
        },
        Some(text),
    ))
}

/// Every rule in the layer, by file name. A missing folder simply means none.
pub fn list(root: &Path) -> Vec<RuleEntry> {
    list_with_text(root).into_iter().map(|(e, _)| e).collect()
}

/// The layer's loadable rules with their text, by file name.
pub fn list_with_text(root: &Path) -> Vec<(RuleEntry, Option<String>)> {
    let Ok(read) = fs::read_dir(rules_dir(root)) else {
        return Vec::new();
    };
    let mut names: Vec<String> = read
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| file_name_of(n).is_ok())
        .collect();
    names.sort();
    names.truncate(MAX_RULES);
    names
        .into_iter()
        .filter_map(|n| entry(&rules_dir(root).join(&n), &n))
        .collect()
}

/// One rule's entry and text.
pub fn read(root: &Path, path: &str) -> io::Result<(RuleEntry, String)> {
    let file = file_name_of(path)?;
    match entry(&rules_dir(root).join(&file), &file) {
        Some((e, Some(text))) => Ok((e, text)),
        Some((_, None)) => Err(invalid("that rule file is too large to load")),
        None => Err(io::Error::new(ErrorKind::NotFound, "no such rule")),
    }
}

/// A file name for a new rule from its title: lower-case letters, digits and hyphens, free in the
/// layer (`-2`, `-3`… when taken).
fn new_file_name(root: &Path, title: &str) -> io::Result<String> {
    let mut slug = String::new();
    for c in title.chars() {
        if c.is_ascii_alphanumeric() {
            slug.push(c.to_ascii_lowercase());
        } else if !slug.ends_with('-') {
            slug.push('-');
        }
    }
    let slug = slug.trim_matches('-');
    let slug: String = slug.chars().take(60).collect();
    let slug = if slug.is_empty() {
        "rule".to_owned()
    } else {
        slug
    };
    for n in 1..100 {
        let name = if n == 1 {
            format!("{slug}.md")
        } else {
            format!("{slug}-{n}.md")
        };
        // Also screens device names ("con", "nul"…): such a slug gets a suffix and is retried.
        if file_name_of(&name).is_ok() && !rules_dir(root).join(&name).exists() {
            return Ok(name);
        }
    }
    Err(invalid("too many rules with that title"))
}

/// Write a rule: a new file named from the title (`path` absent), or the file at `path` (which
/// must exist) replaced. `expected_revision`, when given, must match the file's current revision,
/// else `AlreadyExists` says it changed since it was read. `max_text` caps the text.
pub fn write(
    root: &Path,
    path: Option<&str>,
    title: &str,
    text: &str,
    expected_revision: Option<&str>,
    max_text: usize,
) -> io::Result<RuleEntry> {
    let title = title.trim();
    if title.is_empty() || title.chars().count() > MAX_TITLE_CHARS {
        return Err(invalid("a rule's title is 1 to 120 characters"));
    }
    if helm_core::hidden::has_hidden(title) {
        return Err(invalid(
            "the title cannot hold control, invisible or direction-changing characters",
        ));
    }
    let text = text.trim();
    if text.is_empty() {
        return Err(invalid("a rule needs some text"));
    }
    if text.len() > max_text {
        return Err(invalid("the rule's text is too long"));
    }
    if helm_core::hidden::find_hidden(&serde_json::Value::String(text.to_owned())).is_some() {
        return Err(invalid(
            "the text cannot hold invisible or direction-changing characters",
        ));
    }
    let body = format!("# {title}\n\n{text}\n");
    if body.len() as u64 > MAX_RULE_FILE {
        return Err(invalid("the rule is too large"));
    }
    let file = match path {
        Some(p) => {
            let file = file_name_of(p)?;
            let now = read(root, &file)
                .map_err(|e| match e.kind() {
                    ErrorKind::NotFound => {
                        io::Error::new(ErrorKind::NotFound, "no rule there to replace")
                    }
                    _ => e,
                })?
                .0;
            if expected_revision.is_some_and(|r| r != now.revision) {
                return Err(io::Error::new(
                    ErrorKind::AlreadyExists,
                    "the rule changed since it was read",
                ));
            }
            file
        }
        None => new_file_name(root, title)?,
    };
    helm_core::privfs::create_private_dir_all(&rules_dir(root))?;
    helm_core::privfs::write_atomic(&rules_dir(root).join(&file), body.as_bytes())?;
    read(root, &file).map(|(e, _)| e)
}

/// Remove one rule. Only a plain file in the folder can go, never through a link.
pub fn remove(root: &Path, path: &str) -> io::Result<()> {
    let file = file_name_of(path)?;
    let full = rules_dir(root).join(&file);
    match fs::symlink_metadata(&full) {
        Err(e) if e.kind() == ErrorKind::NotFound => {
            Err(io::Error::new(ErrorKind::NotFound, "no such rule"))
        }
        Err(e) => Err(e),
        Ok(m) if helm_platform::fs::is_link_like(&m) || !m.is_file() => {
            Err(invalid("not a rule file"))
        }
        Ok(_) => fs::remove_file(&full),
    }
}
