//! `helm-hub import <dir>`: loads a folder of Markdown memory files.
//!
//! Each top-level `*.md` file becomes one memory keyed on its frontmatter
//! `name` (falling back to the file stem when there is no frontmatter), so
//! running the import again refreshes changed files and leaves the rest
//! alone. Index files are skipped: they list memories, they are not one.

use crate::store::{NewMemory, Store, StoreError, Upsert};
use serde::Serialize;
use std::path::Path;

/// File names that index a memory folder rather than being a memory.
pub const INDEX_FILES: &[&str] = &["MEMORY.md", "README.md"];
pub const IMPORT_TAG: &str = "imported";

#[derive(Debug, Default, Serialize)]
pub struct ImportReport {
    pub files: usize,
    pub inserted: usize,
    pub updated: usize,
    pub unchanged: usize,
    pub skipped: Vec<String>,
    pub failed: Vec<String>,
}

impl ImportReport {
    pub fn imported(&self) -> usize {
        self.inserted + self.updated + self.unchanged
    }
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Parsed {
    pub name: String,
    pub title: String,
    pub description: Option<String>,
    pub kind: Option<String>,
    pub body: String,
    pub links: Vec<String>,
}

/// `source_tag` names where the files came from (it becomes a tag and the
/// `source` prefix), e.g. "default".
pub fn import_dir(
    store: &mut Store,
    dir: &Path,
    source_tag: &str,
) -> std::io::Result<ImportReport> {
    let mut paths: Vec<_> = std::fs::read_dir(dir)?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.is_file() && p.extension().is_some_and(|x| x.eq_ignore_ascii_case("md")))
        .collect();
    paths.sort();
    let mut report = ImportReport::default();
    let mut seen: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    for path in paths {
        let file_name = path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();
        report.files += 1;
        if INDEX_FILES
            .iter()
            .any(|i| i.eq_ignore_ascii_case(&file_name))
        {
            report.skipped.push(file_name);
            continue;
        }
        let bytes = match std::fs::read(&path) {
            Ok(b) => b,
            Err(e) => {
                report.failed.push(format!("{file_name}: {e}"));
                continue;
            }
        };
        let stem = path
            .file_stem()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();
        let p = parse(&stem, &String::from_utf8_lossy(&bytes));
        // Two files claiming one name would silently overwrite each other.
        if let Some(first) = seen.get(&p.name) {
            report.failed.push(format!(
                "{file_name}: name {:?} is already used by {first}",
                p.name
            ));
            continue;
        }
        seen.insert(p.name.clone(), file_name.clone());
        let mut tags = vec![IMPORT_TAG.to_owned(), source_tag.to_owned()];
        if let Some(k) = &p.kind {
            tags.push(k.clone());
        }
        let m = NewMemory {
            name: Some(p.name),
            title: p.title,
            body: p.body,
            description: p.description,
            kind: p.kind,
            tags,
            source: Some(format!("{source_tag}:{file_name}")),
            links: p.links,
            embedding: None,
        };
        match store.upsert_named(&m) {
            Ok(Upsert::Inserted) => report.inserted += 1,
            Ok(Upsert::Updated) => report.updated += 1,
            Ok(Upsert::Unchanged) => report.unchanged += 1,
            Err(StoreError::Invalid(e)) => report.failed.push(format!("{file_name}: {e}")),
            Err(e) => return Err(std::io::Error::other(e.to_string())),
        }
    }
    Ok(report)
}

/// Splits a memory file into frontmatter fields and body.
pub fn parse(stem: &str, text: &str) -> Parsed {
    let text = text.replace("\r\n", "\n");
    let text = text.strip_prefix('\u{feff}').unwrap_or(&text);
    let (front, body) = split_frontmatter(text);
    let fields = front.map(read_frontmatter).unwrap_or_default();
    let body = body.trim().to_owned();
    let name = fields
        .name
        .filter(|n| !n.trim().is_empty())
        .unwrap_or_else(|| stem.to_owned());
    let title = first_heading(&body).unwrap_or_else(|| name.clone());
    Parsed {
        name: name.trim().to_owned(),
        title,
        description: fields.description.filter(|d| !d.trim().is_empty()),
        kind: fields
            .kind
            .map(|k| k.trim().to_lowercase())
            .filter(|k| !k.is_empty()),
        links: wiki_links(&body),
        body,
    }
}

fn split_frontmatter(text: &str) -> (Option<&str>, &str) {
    let Some(rest) = text.strip_prefix("---\n") else {
        return (None, text);
    };
    // The closing fence is a line holding only `---`.
    if let Some(end) = rest.find("\n---\n") {
        return (Some(&rest[..end]), &rest[end + 5..]);
    }
    if let Some(front) = rest.strip_suffix("\n---") {
        return (Some(front), "");
    }
    if let Some(after) = rest.strip_prefix("---\n") {
        return (Some(""), after);
    }
    (None, text)
}

#[derive(Default)]
struct Fields {
    name: Option<String>,
    description: Option<String>,
    kind: Option<String>,
}

fn read_frontmatter(front: &str) -> Fields {
    if let Ok(serde_yaml::Value::Mapping(map)) = serde_yaml::from_str::<serde_yaml::Value>(front) {
        let get = |m: &serde_yaml::Mapping, k: &str| -> Option<String> {
            match m.get(k)? {
                serde_yaml::Value::String(s) => Some(s.clone()),
                serde_yaml::Value::Number(n) => Some(n.to_string()),
                serde_yaml::Value::Bool(b) => Some(b.to_string()),
                _ => None,
            }
        };
        let nested_kind = match map.get("metadata") {
            Some(serde_yaml::Value::Mapping(meta)) => get(meta, "type"),
            _ => None,
        };
        return Fields {
            name: get(&map, "name"),
            description: get(&map, "description"),
            kind: nested_kind.or_else(|| get(&map, "type")),
        };
    }
    // Not valid YAML (an unquoted colon in a description is the usual cause):
    // fall back to reading `key: value` lines.
    let mut f = Fields::default();
    for line in front.lines() {
        let indented = line.starts_with(' ') || line.starts_with('\t');
        let Some((k, v)) = line.trim().split_once(':') else {
            continue;
        };
        let v = v.trim().trim_matches(|c| c == '"' || c == '\'').to_owned();
        match (k.trim(), indented) {
            ("name", false) => f.name = Some(v),
            ("description", false) => f.description = Some(v),
            ("type", _) if f.kind.is_none() || indented => f.kind = Some(v),
            _ => {}
        }
    }
    f
}

fn first_heading(body: &str) -> Option<String> {
    body.lines()
        .find_map(|l| l.strip_prefix("# "))
        .map(|h| h.trim().to_owned())
        .filter(|h| !h.is_empty())
}

/// `[[target]]`, `[[target|alias]]` and `[[target#heading]]` all yield `target`.
pub fn wiki_links(body: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut rest = body;
    while let Some(start) = rest.find("[[") {
        let after = &rest[start + 2..];
        let Some(end) = after.find("]]") else { break };
        let inner = &after[..end];
        if !inner.contains('\n') {
            let target = inner.split(['|', '#']).next().unwrap_or("").trim();
            if !target.is_empty() && !out.iter().any(|t| t == target) {
                out.push(target.to_owned());
            }
        }
        rest = &after[end + 2..];
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::SearchQuery;

    #[test]
    fn parses_nested_metadata_type_and_links() {
        let p = parse(
            "stem",
            "---\r\nname: lessons-x\r\ndescription: what we learnt\r\nmetadata:\r\n  type: Feedback\r\n---\r\n\r\n# Heading\r\n\r\nSee [[other-note|alias]] and [[third#part]] and [[other-note]].\r\n",
        );
        assert_eq!(p.name, "lessons-x");
        assert_eq!(p.description.as_deref(), Some("what we learnt"));
        assert_eq!(p.kind.as_deref(), Some("feedback"));
        assert_eq!(p.title, "Heading");
        assert_eq!(p.links, vec!["other-note", "third"]);
        assert!(p.body.starts_with("# Heading"));
        assert!(!p.body.contains('\r'));
    }

    #[test]
    fn top_level_type_and_invalid_yaml_fallback() {
        let p = parse(
            "s",
            "---\nname: n\ndescription: has: two colons: here\ntype: project\n---\nbody",
        );
        assert_eq!(p.name, "n");
        assert_eq!(p.description.as_deref(), Some("has: two colons: here"));
        assert_eq!(p.kind.as_deref(), Some("project"));
        assert_eq!(p.title, "n");
        assert_eq!(p.body, "body");
    }

    #[test]
    fn no_frontmatter_uses_the_file_stem() {
        let p = parse("2026-04-05-summary", "# Summary\n\n---\n\ntext");
        assert_eq!(p.name, "2026-04-05-summary");
        assert_eq!(p.title, "Summary");
        assert_eq!(p.kind, None);
        assert!(p.body.contains("text"));
    }

    #[test]
    fn import_is_idempotent_and_skips_index_files() {
        let dir = tempfile::tempdir().unwrap();
        let w = |n: &str, t: &str| std::fs::write(dir.path().join(n), t).unwrap();
        w(
            "a.md",
            "---\nname: alpha\ndescription: d\nmetadata:\n  type: lesson\n---\nalpha body",
        );
        w("b.md", "plain beta body");
        w("MEMORY.md", "# index\n- [a](a.md)");
        w("notes.txt", "not markdown");
        w(
            "c.md",
            "---
name: alpha
---
same name as a.md",
        );
        std::fs::create_dir(dir.path().join("archived")).unwrap();
        std::fs::write(dir.path().join("archived").join("old.md"), "old").unwrap();

        let mut s = Store::open_in_memory().unwrap();
        let r1 = import_dir(&mut s, dir.path(), "default").unwrap();
        assert_eq!((r1.inserted, r1.updated, r1.unchanged), (2, 0, 0));
        assert_eq!(r1.skipped, vec!["MEMORY.md"]);
        assert_eq!(r1.failed.len(), 1, "{:?}", r1.failed);
        assert!(r1.failed[0].starts_with("c.md"));
        assert_eq!(s.count().unwrap(), 2);

        let r2 = import_dir(&mut s, dir.path(), "default").unwrap();
        assert_eq!((r2.inserted, r2.updated, r2.unchanged), (0, 0, 2));
        assert_eq!(s.count().unwrap(), 2);

        w(
            "a.md",
            "---\nname: alpha\ndescription: d\nmetadata:\n  type: lesson\n---\nalpha body v2",
        );
        let r3 = import_dir(&mut s, dir.path(), "default").unwrap();
        assert_eq!((r3.inserted, r3.updated, r3.unchanged), (0, 1, 1));
        assert_eq!(s.count().unwrap(), 2);

        let a = s.get_by_name("alpha").unwrap();
        assert_eq!(a.body, "alpha body v2");
        assert_eq!(a.kind.as_deref(), Some("lesson"));
        assert_eq!(a.tags, vec!["imported", "default", "lesson"]);
        assert_eq!(a.source.as_deref(), Some("default:a.md"));
        let hits = s
            .search(&SearchQuery {
                query: "beta".into(),
                k: 5,
                ..Default::default()
            })
            .unwrap();
        assert_eq!(hits[0].summary.name.as_deref(), Some("b"));
        assert_eq!(s.list(Some("lesson"), None, 10).unwrap().len(), 1);
    }
}
