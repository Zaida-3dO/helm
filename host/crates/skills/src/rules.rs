//! Rules: instruction files a chat is told at launch.
//!
//! A layer's `rules/` folder holds one Markdown file per rule. A rule's title is the file's first
//! `# ` line (the file name when it has none); the rest is the instruction. Rules are plain text
//! the model reads, so a write is screened for invisible and direction-changing characters, is
//! made atomically (a temporary file renamed into place) and is capped. Every function takes a
//! layer's root folder.

use serde::{Deserialize, Serialize};
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
const MAX_RULES: usize = 1000;
/// File names this long or longer are not rules.
const MAX_FILE_NAME: usize = 100;

/// The rules folder inside a layer's root.
pub fn rules_dir(root: &Path) -> PathBuf {
    root.join("rules")
}

/// One rule as Settings and the launch see it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct RuleEntry {
    /// `rules/<file>.md`: what the tools and ops take back.
    pub path: String,
    pub title: String,
    #[ts(type = "number")]
    pub size: u64,
    /// A hash of the file's text, for an edit that must not overwrite a newer one. Empty when the
    /// file is too large to load.
    pub revision: String,
    /// Over the size limit: listed so it can be fixed, but never loaded into a chat.
    pub too_large: bool,
    /// The header says `always: true`: the launch carries the whole rule into a chat's prompt,
    /// however long it is. False for a rule too large to load, whose header is never read.
    pub always: bool,
    /// The trigger keys as the header declares them; all empty for a rule with no trigger.
    /// Empty here as well for a rule too large to load.
    pub when: RuleWhen,
    /// What the trigger says in one short line for Settings; empty when there is no trigger.
    pub when_summary: String,
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

/// A rule file's optional header: a `---` line, lines such as `summary: ...` and `always: true`,
/// trigger keys like `folders: …` or `commands: …`, and a closing `---` line. A file without one
/// is all text.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Front {
    /// One line saying what the rule is for; the launch lists a long rule by it.
    pub summary: Option<String>,
    /// The launch carries the whole rule, however long it is.
    pub always: bool,
    /// When the rule reaches a chat and what a matching call does to it. Empty means no trigger:
    /// the rule reaches every chat, as it always has.
    pub when: RuleWhen,
}

/// The header's trigger keys: when a rule applies and what happens when one applies at a call.
/// Every field is optional; an all-empty `RuleWhen` states "no trigger". Header lists are
/// comma-separated, with double quotes around a value that holds a comma. A bad date or an
/// unknown value is left out of the fields and named in `problems`; the launch ignores both.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase", default)]
pub struct RuleWhen {
    /// What the chat gets when the rule applies: `auto` (whole text up to the inline limit, else
    /// one line), `full` (the whole text) or `line`. Absent means `auto`.
    pub show: Option<String>,
    /// What the chat gets before a rule with moment conditions fires: `hidden` (nothing) or
    /// `line`. Absent means `hidden`.
    pub otherwise: Option<String>,
    /// What happens the first time a call matches: `note` (the rule rides along as a note) or
    /// `stop` (that one call is refused, with the rule as the reason). Absent means `note`.
    pub before: Option<String>,
    /// Folders whose work the rule covers: matched by the chat's folder and by paths a call
    /// touches.
    pub folders: Vec<String>,
    /// Machine names or addresses the work reaches (matched as whole words in commands and paths).
    pub hosts: Vec<String>,
    /// The PCs this rule's chat may run on, matched case-insensitively.
    pub pcs: Vec<String>,
    /// Tool names; `*` matches any run of characters. A name without `__` is read as the last
    /// part of a tool name.
    pub tools: Vec<String>,
    /// Commands to watch for, matched at the start of each command segment. May be more than one
    /// word (`git push`).
    pub commands: Vec<String>,
    /// The kinds of chat the rule reaches: `coding`, `orchestrating` or `household`.
    pub kinds: Vec<String>,
    /// Profile names of the people whose chats the rule reaches, matched case-insensitively.
    pub people: Vec<String>,
    /// First day (local `YYYY-MM-DD`) the rule applies; before it the chat sees nothing.
    pub from: Option<String>,
    /// Last day (local `YYYY-MM-DD`) the rule applies; after it the chat sees nothing, and the
    /// file stays until someone deletes it.
    pub until: Option<String>,
    /// A date (local `YYYY-MM-DD`) past which Settings asks whether to keep or delete the rule.
    pub review: Option<String>,
    /// Read-only: header lines that could not be read, each naming its key. Never written back.
    pub problems: Vec<String>,
}

/// Every trigger key, in the order a rewrite writes them and `problems` name them.
pub const TRIGGER_KEYS: [&str; 13] = [
    "show",
    "otherwise",
    "before",
    "folders",
    "hosts",
    "pcs",
    "tools",
    "commands",
    "kinds",
    "people",
    "from",
    "until",
    "review",
];

impl RuleWhen {
    /// True when the rule carries no trigger at all (`problems` aside: a header whose only
    /// trigger line was unreadable still reaches every chat).
    pub fn is_empty(&self) -> bool {
        self.show.is_none()
            && self.otherwise.is_none()
            && self.before.is_none()
            && self.folders.is_empty()
            && self.hosts.is_empty()
            && self.pcs.is_empty()
            && self.tools.is_empty()
            && self.commands.is_empty()
            && self.kinds.is_empty()
            && self.people.is_empty()
            && self.from.is_none()
            && self.until.is_none()
            && self.review.is_none()
    }

    /// True when the rule watches a moment (a folder, a machine, a tool or a command) rather
    /// than only describing the chat it applies to.
    pub fn has_moment(&self) -> bool {
        !(self.folders.is_empty()
            && self.hosts.is_empty()
            && self.tools.is_empty()
            && self.commands.is_empty())
    }

    /// The header lines this trigger asks for, in `TRIGGER_KEYS` order: a key that is unset maps
    /// to `None` so a rewrite removes its old lines. List values join with ", "; one holding a
    /// comma goes in double quotes.
    pub fn header_changes(&self) -> Vec<(&'static str, Option<String>)> {
        let word = |v: &Option<String>| v.clone();
        let list = |v: &[String]| {
            if v.is_empty() {
                return None;
            }
            Some(
                v.iter()
                    .map(|s| {
                        if s.contains(',') {
                            format!("\"{s}\"")
                        } else {
                            s.clone()
                        }
                    })
                    .collect::<Vec<_>>()
                    .join(", "),
            )
        };
        vec![
            ("show", word(&self.show)),
            ("otherwise", word(&self.otherwise)),
            ("before", word(&self.before)),
            ("folders", list(&self.folders)),
            ("hosts", list(&self.hosts)),
            ("pcs", list(&self.pcs)),
            ("tools", list(&self.tools)),
            ("commands", list(&self.commands)),
            ("kinds", list(&self.kinds)),
            ("people", list(&self.people)),
            ("from", word(&self.from)),
            ("until", word(&self.until)),
            ("review", word(&self.review)),
        ]
    }

    /// What the trigger says in one short line for Settings, empty when there is no trigger.
    /// `today` only decides whether a date part shows its year.
    pub fn summary(&self, today: chrono::NaiveDate) -> String {
        if self.is_empty() {
            return String::new();
        }
        let mut parts: Vec<String> = Vec::new();
        for list in [&self.folders, &self.hosts, &self.pcs] {
            if !list.is_empty() {
                parts.push(list.join(", "));
            }
        }
        match (self.tools.is_empty(), self.commands.is_empty()) {
            (false, false) => parts.push(format!(
                "{}: {}",
                self.tools.join(", "),
                self.commands.join(", ")
            )),
            (false, true) => parts.push(self.tools.join(", ")),
            (true, false) => parts.push(self.commands.join(", ")),
            (true, true) => {}
        }
        for list in [&self.kinds, &self.people] {
            if !list.is_empty() {
                parts.push(list.join(", "));
            }
        }
        for (label, day) in [
            ("from", &self.from),
            ("until", &self.until),
            ("review", &self.review),
        ] {
            if let Some(d) = day.as_ref().and_then(|d| parse_date(d)) {
                parts.push(if d.year() == today.year() {
                    format!("{label} {}", d.format("%d %b"))
                } else {
                    format!("{label} {}", d.format("%d %b %Y"))
                });
            }
        }
        parts.join(" · ")
    }
}

/// A `YYYY-MM-DD` date, or None when the text is not one.
pub fn parse_date(text: &str) -> Option<chrono::NaiveDate> {
    chrono::NaiveDate::parse_from_str(text.trim(), "%Y-%m-%d").ok()
}

/// Split a header list value at its commas, keeping quoted runs (a folder path may hold one).
fn split_list(value: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut item = String::new();
    let mut quoted = false;
    for c in value.chars() {
        match c {
            '"' => quoted = !quoted,
            ',' if !quoted => {
                let t = item.trim().to_owned();
                if !t.is_empty() {
                    out.push(t);
                }
                item.clear();
            }
            _ => item.push(c),
        }
    }
    let t = item.trim().to_owned();
    if !t.is_empty() {
        out.push(t);
    }
    out
}

/// Read one trigger line into `w`. A bad value is left out and named in `problems`; the first
/// line for a single-valued key wins.
fn read_trigger_line(w: &mut RuleWhen, key: &str, value: &str) {
    match key {
        "show" | "otherwise" | "before" => {
            let allowed: &[&str] = match key {
                "show" => &["auto", "full", "line"],
                "otherwise" => &["hidden", "line"],
                _ => &["note", "stop"],
            };
            if value.is_empty() {
                return;
            }
            let got = value.to_lowercase();
            if !allowed.contains(&got.as_str()) {
                w.problems
                    .push(format!("{key}: use {}", allowed.join(" or ")));
                return;
            }
            let taken = match key {
                "show" => w.show.is_some(),
                "otherwise" => w.otherwise.is_some(),
                _ => w.before.is_some(),
            };
            if taken {
                w.problems.push(format!("{key}: given twice"));
                return;
            }
            match key {
                "show" => w.show = Some(got),
                "otherwise" => w.otherwise = Some(got),
                _ => w.before = Some(got),
            }
        }
        "folders" | "hosts" | "pcs" | "tools" | "commands" | "kinds" | "people" => {
            let more = split_list(value);
            match key {
                "folders" => w.folders.extend(more),
                "hosts" => w.hosts.extend(more),
                "pcs" => w.pcs.extend(more),
                "tools" => w.tools.extend(more),
                "commands" => w.commands.extend(more),
                "people" => w.people.extend(more),
                _ => {
                    let (keep, bad): (Vec<String>, Vec<String>) = more.into_iter().partition(|k| {
                        matches!(k.as_str(), "coding" | "orchestrating" | "household")
                    });
                    for k in bad {
                        w.problems.push(format!(
                            "kinds: {k}: use coding, orchestrating or household"
                        ));
                    }
                    w.kinds.extend(keep);
                }
            }
        }
        _ => {
            // from, until, review
            if value.is_empty() {
                return;
            }
            if parse_date(value).is_none() {
                w.problems.push(format!("{key}: not a YYYY-MM-DD date"));
                return;
            }
            let taken = match key {
                "from" => w.from.is_some(),
                "until" => w.until.is_some(),
                _ => w.review.is_some(),
            };
            if taken {
                w.problems.push(format!("{key}: given twice"));
                return;
            }
            let day = value.trim().to_owned();
            match key {
                "from" => w.from = Some(day),
                "until" => w.until = Some(day),
                _ => w.review = Some(day),
            }
        }
    }
}

/// The byte length of the header (both fences and the line end after the closing one), or 0.
fn front_len(text: &str) -> usize {
    let t = text.strip_prefix('\u{feff}').unwrap_or(text);
    let bom = text.len() - t.len();
    let Some(rest) = t
        .strip_prefix("---\n")
        .or_else(|| t.strip_prefix("---\r\n"))
    else {
        return 0;
    };
    let open = t.len() - rest.len();
    let mut at = 0;
    for line in rest.split_inclusive('\n') {
        if line.trim_end() == "---" {
            return bom + open + at + line.len();
        }
        at += line.len();
    }
    0
}

/// A file's header and the text after it.
pub fn split_front(text: &str) -> (Front, &str) {
    let n = front_len(text);
    if n == 0 {
        return (Front::default(), text);
    }
    let head = &text[..n];
    let mut f = Front::default();
    for line in head.lines().skip(1) {
        let Some((k, v)) = line.split_once(':') else {
            continue;
        };
        let v = v.trim();
        match k.trim() {
            "summary" if !v.is_empty() => f.summary = Some(v.chars().take(200).collect()),
            "always" => f.always = v.eq_ignore_ascii_case("true"),
            key if TRIGGER_KEYS.contains(&key) => read_trigger_line(&mut f.when, key, v),
            _ => {}
        }
    }
    (f, &text[n..])
}

/// Rewrite the named header keys and leave every other line (recognised or not) as it is. Each
/// change is a key and its new value: `Some(text)` writes one `key: text` line for that key,
/// `None` removes the key's lines. Lines for changed keys go after the kept ones, in the order
/// the changes are given; everything else keeps its place. A header left with no lines in it goes
/// away, and an empty header grows one only when a change asks for a line.
fn set_header(header: &str, changes: &[(&str, Option<String>)]) -> String {
    if changes.is_empty() {
        return header.to_owned();
    }
    let written: Vec<String> = changes
        .iter()
        .filter_map(|(k, v)| Some(format!("{k}: {}", v.as_ref()?)))
        .collect();
    if header.is_empty() {
        let mut lines = String::new();
        for line in &written {
            lines.push_str(line);
            lines.push('\n');
        }
        return if lines.is_empty() {
            String::new()
        } else {
            format!("---\n{lines}---\n")
        };
    }
    let fence_end = header.find('\n').unwrap_or(header.len()) + 1;
    let open = header[..fence_end].replace("\r\n", "\n");
    let body = &header[fence_end..];
    let inner = body[..body.rfind("---").unwrap_or(0)].replace('\r', "");
    let mut lines = String::new();
    for line in inner.lines() {
        if line
            .split_once(':')
            .is_some_and(|(k, _)| changes.iter().any(|(ck, _)| k.trim() == *ck))
        {
            continue;
        }
        lines.push_str(line);
        lines.push('\n');
    }
    for line in &written {
        lines.push_str(line);
        lines.push('\n');
    }
    if lines.is_empty() {
        return String::new();
    }
    format!("{open}{lines}---\n")
}

/// The title of a rule's text: its first `# ` line, else `fallback`.
pub fn title_of(text: &str, fallback: &str) -> String {
    let text = split_front(text).1;
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
    let text = split_front(text).1;
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
                always: false,
                when: RuleWhen::default(),
                when_summary: String::new(),
            },
            None,
        ));
    }
    let bytes = helm_core::privfs::read_capped(path, MAX_RULE_FILE).ok()?;
    let text = String::from_utf8_lossy(&bytes).into_owned();
    let front = split_front(&text).0;
    let when_summary = if front.when.is_empty() {
        String::new()
    } else {
        front.when.summary(chrono::Local::now().date_naive())
    };
    Some((
        RuleEntry {
            path: format!("rules/{file}"),
            title: title_of(&text, stem),
            size: meta.len(),
            revision: helm_core::sha256_hex(&bytes),
            too_large: false,
            when_summary,
            always: front.always,
            when: front.when,
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
/// else `AlreadyExists` says it changed since it was read. `always`, when given, sets or clears
/// the header's `always: true` line (a rewrite without it keeps the flag as it is). `when`, when
/// given, rewrites every trigger key to match it — an all-empty `RuleWhen` clears the trigger
/// keys, and unknown header lines survive either way (a rewrite without `when` leaves them alone).
/// `max_text` caps the text.
pub fn write(
    root: &Path,
    path: Option<&str>,
    title: &str,
    text: &str,
    expected_revision: Option<&str>,
    always: Option<bool>,
    when: Option<&RuleWhen>,
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
    // A rewritten rule keeps its header (its summary, whether it is always carried); `always`
    // sets or clears the flag and `when` rewrites the trigger keys, neither disturbing the rest.
    let mut changes: Vec<(&str, Option<String>)> = Vec::new();
    if let Some(a) = always {
        changes.push(("always", a.then(|| "true".to_owned())));
    }
    if let Some(w) = when {
        for (k, v) in w.header_changes() {
            changes.push((k, v));
        }
    }
    let (header, file) = match path {
        Some(p) => {
            let file = file_name_of(p)?;
            let (now, old) = read(root, &file).map_err(|e| match e.kind() {
                ErrorKind::NotFound => {
                    io::Error::new(ErrorKind::NotFound, "no rule there to replace")
                }
                _ => e,
            })?;
            if expected_revision.is_some_and(|r| r != now.revision) {
                return Err(io::Error::new(
                    ErrorKind::AlreadyExists,
                    "the rule changed since it was read",
                ));
            }
            (set_header(&old[..front_len(&old)], &changes), file)
        }
        None => (set_header("", &changes), new_file_name(root, title)?),
    };
    let body = format!("{header}{body}");
    if body.len() as u64 > MAX_RULE_FILE {
        return Err(invalid("the rule is too large"));
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    /// The whole file behind a rule's `rules/<file>` path.
    fn text_on_disk(root: &Path, path: &str) -> String {
        let name = path.trim_start_matches("rules/");
        fs::read_to_string(rules_dir(root).join(name)).unwrap()
    }

    #[test]
    fn header_trigger_keys_are_read_accumulated_and_screened() {
        let head = [
            "---",
            "summary: One line about the rule",
            "always: true",
            "# a stray comment",
            "show: LINE",
            "before: stop",
            "folders: C:\\work\\repo, \"C:\\odd, name\"",
            "folders: D:\\other",
            "hosts: nas1, 192.0.2.10",
            "pcs: pc-a",
            "tools: Bash, mcp__*browser*",
            "commands: taskkill, git push",
            "kinds: coding, household, wrong-one",
            "people: alice",
            "from: 2026-11-26",
            "until: not-a-date",
            "review: 2026-12-15",
            "unknown-key: kept as it is",
            "---",
        ]
        .join("\n");
        let (f, _) = split_front(&format!("{head}\n# T\n\nbody\n"));
        assert_eq!(f.summary.as_deref(), Some("One line about the rule"));
        assert!(f.always);
        assert_eq!(f.when.show.as_deref(), Some("line"), "values go lower-case");
        assert_eq!(f.when.before.as_deref(), Some("stop"));
        assert_eq!(f.when.otherwise, None, "absent stays absent");
        assert_eq!(
            f.when.folders,
            ["C:\\work\\repo", "C:\\odd, name", "D:\\other"],
            "a list key may repeat and quoted commas survive"
        );
        assert_eq!(f.when.hosts, ["nas1", "192.0.2.10"]);
        assert_eq!(f.when.pcs, ["pc-a"]);
        assert_eq!(f.when.tools, ["Bash", "mcp__*browser*"]);
        assert_eq!(f.when.commands, ["taskkill", "git push"]);
        assert_eq!(
            f.when.kinds,
            ["coding", "household"],
            "bad kinds are screened out"
        );
        assert_eq!(f.when.people, ["alice"]);
        assert_eq!(f.when.from.as_deref(), Some("2026-11-26"));
        assert_eq!(f.when.until, None, "a bad date is ignored");
        assert_eq!(f.when.review.as_deref(), Some("2026-12-15"));
        assert_eq!(
            f.when.problems,
            [
                "kinds: wrong-one: use coding, orchestrating or household",
                "until: not a YYYY-MM-DD date"
            ]
        );

        // The same key given twice for a single value: the first line wins and says so.
        let (f, _) =
            split_front("---\nshow: full\nshow: line\nfrom: 2026-11-26\nfrom: 2026-12-01\n---\nx");
        assert_eq!(f.when.show.as_deref(), Some("full"));
        assert_eq!(f.when.from.as_deref(), Some("2026-11-26"));
        assert_eq!(f.when.problems, ["show: given twice", "from: given twice"]);

        // No header at all: default front, whole file is text.
        let (f, body) = split_front("# T\n\nplain text\n");
        assert_eq!(f, Front::default());
        assert!(f.when.is_empty() && !f.when.has_moment());
        assert_eq!(body, "# T\n\nplain text\n");

        // Chat-only conditions say no moment; folders do.
        let mut w = RuleWhen::default();
        w.kinds = vec!["coding".into()];
        assert!(!w.has_moment());
        w.folders = vec!["C:\\work\\repo".into()];
        assert!(w.has_moment());
    }

    #[test]
    fn writing_when_sets_then_clears_it_and_keeps_unknown_lines() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("layer");

        // A new rule written with a trigger gets the trigger lines and nothing else; problems
        // never reach the header.
        let mut w = RuleWhen {
            tools: vec!["Bash".into(), "PowerShell".into()],
            commands: vec!["taskkill".into(), "git push".into()],
            folders: vec!["C:\\odd, name".into()],
            kinds: vec!["coding".into()],
            until: Some("2027-01-31".into()),
            ..Default::default()
        };
        w.problems.push("until: given twice".into());
        let e = write(
            &root,
            None,
            "Kill by PID",
            "Never kill by name.",
            None,
            None,
            Some(&w),
            4096,
        )
        .unwrap();
        let front = split_front(&text_on_disk(&root, &e.path)).0;
        assert_eq!(front.when.show, None);
        assert_eq!(front.when.tools, ["Bash", "PowerShell"]);
        assert_eq!(front.when.commands, ["taskkill", "git push"]);
        assert_eq!(
            front.when.folders,
            ["C:\\odd, name"],
            "the comma path comes back quoted"
        );
        assert_eq!(front.when.kinds, ["coding"]);
        assert_eq!(front.when.until.as_deref(), Some("2027-01-31"));
        assert!(front.when.problems.is_empty());

        // Rewriting with `always` keeps the trigger; rewriting with an empty When clears it but
        // leaves always (not a trigger key) alone.
        let e = write(
            &root,
            Some(&e.path),
            "Kill by PID",
            "Reworded.",
            None,
            Some(true),
            None,
            4096,
        )
        .unwrap();
        let front = split_front(&text_on_disk(&root, &e.path)).0;
        assert!(front.always);
        assert_eq!(front.when.tools, ["Bash", "PowerShell"]);
        let e = write(
            &root,
            Some(&e.path),
            "Kill by PID",
            "Reworded.",
            Some(&e.revision),
            None,
            Some(&RuleWhen::default()),
            4096,
        )
        .unwrap();
        let front = split_front(&text_on_disk(&root, &e.path)).0;
        assert!(front.always, "always is not a trigger key");
        assert!(front.when.is_empty());

        // The header disappears once its last known line goes.
        let e = write(
            &root,
            Some(&e.path),
            "Kill by PID",
            "Reworded.",
            None,
            Some(false),
            None,
            4096,
        )
        .unwrap();
        assert!(
            text_on_disk(&root, &e.path).starts_with("# Kill by PID"),
            "no lines left: the header is gone"
        );

        // An unknown line survives any rewrite.
        fs::write(
            rules_dir(&root).join(e.path.trim_start_matches("rules/")),
            "---\nmy-key: keep me\nfolders: C:\\work\\repo\n---\n# T\n\nbody\n",
        )
        .unwrap();
        let e = write(
            &root,
            Some(&e.path),
            "T",
            "new body",
            None,
            None,
            Some(&RuleWhen::default()),
            4096,
        )
        .unwrap();
        let text = text_on_disk(&root, &e.path);
        assert!(text.contains("my-key: keep me"), "unknown lines survive");
        let front = split_front(&text).0;
        assert!(front.when.folders.is_empty());
        assert!(text.contains("new body"));
    }

    #[test]
    fn the_trigger_summary_reads_as_one_line_or_stays_empty() {
        let today = chrono::NaiveDate::from_ymd_opt(2026, 11, 26).unwrap();
        assert_eq!(RuleWhen::default().summary(today), "");
        let mut w = RuleWhen::default();
        w.tools = vec!["Bash".into()];
        w.commands = vec!["taskkill".into(), "pkill".into()];
        w.kinds = vec!["coding".into()];
        w.until = Some("2027-01-31".into());
        assert_eq!(
            w.summary(today),
            "Bash: taskkill, pkill · coding · until 31 Jan 2027"
        );
        let (f, _) = split_front(
            "---\nfrom: 2026-11-01\nreview: 2026-12-15\nfolders: C:\\work\\repo\n---\nx",
        );
        assert_eq!(
            f.when.summary(today),
            "C:\\work\\repo · from 01 Nov · review 15 Dec"
        );
    }
}
