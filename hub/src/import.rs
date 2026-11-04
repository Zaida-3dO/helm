//! `helm-hub import <dir>`: loads a folder of Markdown memory files.
//!
//! Each `*.md` file in the folder and its subfolders (hidden folders are
//! left out) becomes one memory keyed on its frontmatter `name` (falling
//! back to the file stem when there is no frontmatter), so running the
//! import again refreshes changed files and leaves the rest alone. Index
//! files are skipped: they list memories, they are not one. With `prune`, a
//! memory of the same source whose file is gone is removed.

use crate::store::{NewMemory, Store, StoreError, Upsert};
use serde::Serialize;
use std::path::{Path, PathBuf};

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
    /// Names (or sources) of the memories removed because their files are gone.
    pub pruned: Vec<String>,
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

/// What the command line asked for.
#[derive(Debug, PartialEq)]
pub struct ImportArgs {
    pub dir: PathBuf,
    pub source: String,
    pub prune: bool,
}

/// Reads `<dir> --source S [--prune]`. The source is required: it decides which memories a prune
/// may remove, and a default would quietly re-tag everything imported before.
pub fn parse_args(args: &[String]) -> Result<ImportArgs, String> {
    let mut dir = None;
    let mut source = None;
    let mut prune = false;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--source" => match it.next() {
                Some(s) => source = Some(s.trim().to_owned()),
                None => return Err("--source needs a value".into()),
            },
            "--prune" => prune = true,
            d if !d.starts_with("--") && dir.is_none() => dir = Some(PathBuf::from(d)),
            other => return Err(format!("unexpected argument {other:?}")),
        }
    }
    let dir = dir.ok_or("import needs a folder to read")?;
    match source {
        Some(s) if !s.is_empty() => Ok(ImportArgs {
            dir,
            source: s,
            prune,
        }),
        _ => Err(
            "import needs --source <tag>: say which source these memories come from, \
                  so a re-import refreshes the same ones (for example --source my-notes)"
                .into(),
        ),
    }
}

/// Every `*.md` file under `dir`, folders sorted, hidden folders (and symlinked ones) skipped.
fn markdown_files(dir: &Path, out: &mut Vec<PathBuf>) -> std::io::Result<()> {
    let mut entries: Vec<_> = std::fs::read_dir(dir)?.filter_map(|e| e.ok()).collect();
    entries.sort_by_key(|e| e.path());
    for e in entries {
        let path = e.path();
        let Ok(kind) = e.file_type() else { continue };
        if kind.is_dir() {
            let hidden = e.file_name().to_string_lossy().starts_with('.');
            if !hidden {
                markdown_files(&path, out)?;
            }
        } else if kind.is_file()
            && path
                .extension()
                .is_some_and(|x| x.eq_ignore_ascii_case("md"))
        {
            out.push(path);
        }
    }
    Ok(())
}

/// `source_tag` names where the files came from (it becomes a tag and the
/// `source` prefix), e.g. "my-notes". With `prune`, memories whose source
/// starts with `<source_tag>:` and that this run did not see are removed; a
/// run that finds no memory files prunes nothing (the folder is probably not
/// mounted).
pub fn import_dir(
    store: &mut Store,
    dir: &Path,
    source_tag: &str,
    prune: bool,
) -> std::io::Result<ImportReport> {
    let mut paths = Vec::new();
    markdown_files(dir, &mut paths)?;
    let mut report = ImportReport::default();
    let mut seen: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    // Sources of files that failed this run: their existing memories are kept, not pruned.
    let mut failed_sources: std::collections::HashSet<String> = std::collections::HashSet::new();
    for path in paths {
        // The path below the folder, with `/` on every system: `sub/a.md`, or `a.md` at the top.
        let file_name = path
            .strip_prefix(dir)
            .unwrap_or(&path)
            .components()
            .map(|c| c.as_os_str().to_string_lossy().into_owned())
            .collect::<Vec<_>>()
            .join("/");
        let base_name = path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();
        report.files += 1;
        let source = format!("{source_tag}:{file_name}");
        if INDEX_FILES
            .iter()
            .any(|i| i.eq_ignore_ascii_case(&base_name))
        {
            report.skipped.push(file_name);
            continue;
        }
        let bytes = match std::fs::read(&path) {
            Ok(b) => b,
            Err(e) => {
                failed_sources.insert(source.clone());
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
            failed_sources.insert(source.clone());
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
            source: Some(source.clone()),
            links: p.links,
            embedding: None,
        };
        match store.upsert_named(&m) {
            Ok(Upsert::Inserted) => report.inserted += 1,
            Ok(Upsert::Updated) => report.updated += 1,
            Ok(Upsert::Unchanged) => report.unchanged += 1,
            Err(StoreError::Invalid(e)) => {
                failed_sources.insert(source);
                report.failed.push(format!("{file_name}: {e}"));
            }
            Err(e) => return Err(std::io::Error::other(e.to_string())),
        }
    }
    if prune && report.files > 0 {
        let prefix = format!("{source_tag}:");
        let stale = store
            .with_source_prefix(&prefix)
            .map_err(|e| std::io::Error::other(e.to_string()))?;
        for (id, name, source) in stale {
            // Rows with no name did not come from an import; a failed file keeps its memory.
            let kept = name.as_ref().is_none_or(|n| seen.contains_key(n))
                || failed_sources.contains(&source);
            if kept {
                continue;
            }
            store
                .delete(id)
                .map_err(|e| std::io::Error::other(e.to_string()))?;
            report.pruned.push(name.unwrap_or(source));
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
        // A subfolder is read too; a hidden folder is not.
        std::fs::create_dir(dir.path().join("archived")).unwrap();
        std::fs::write(dir.path().join("archived").join("old.md"), "old").unwrap();
        std::fs::create_dir(dir.path().join(".git")).unwrap();
        std::fs::write(dir.path().join(".git").join("hidden.md"), "hidden").unwrap();

        let mut s = Store::open_in_memory().unwrap();
        let r1 = import_dir(&mut s, dir.path(), "default", false).unwrap();
        assert_eq!((r1.inserted, r1.updated, r1.unchanged), (3, 0, 0));
        assert_eq!(r1.skipped, vec!["MEMORY.md"]);
        assert_eq!(r1.failed.len(), 1, "{:?}", r1.failed);
        assert!(r1.failed[0].starts_with("c.md"));
        assert_eq!(s.count().unwrap(), 3);

        let r2 = import_dir(&mut s, dir.path(), "default", false).unwrap();
        assert_eq!((r2.inserted, r2.updated, r2.unchanged), (0, 0, 3));
        assert_eq!(s.count().unwrap(), 3);

        w(
            "a.md",
            "---\nname: alpha\ndescription: d\nmetadata:\n  type: lesson\n---\nalpha body v2",
        );
        let r3 = import_dir(&mut s, dir.path(), "default", false).unwrap();
        assert_eq!((r3.inserted, r3.updated, r3.unchanged), (0, 1, 2));
        assert_eq!(s.count().unwrap(), 3);

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
        let old = s.get_by_name("old").unwrap();
        assert_eq!(old.source.as_deref(), Some("default:archived/old.md"));
        assert!(s.get_by_name("hidden").is_err());
    }

    #[test]
    fn prune_removes_only_this_sources_memories_whose_files_are_gone() {
        let dir = tempfile::tempdir().unwrap();
        let w = |n: &str, t: &str| {
            let p = dir.path().join(n);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, t).unwrap();
        };
        w("keep.md", "keep body");
        w("sub/gone.md", "gone body");
        w("renamed.md", "---\nname: before\n---\nbody");
        let mut s = Store::open_in_memory().unwrap();
        import_dir(&mut s, dir.path(), "mine", false).unwrap();
        // Another source's memory and one written by an agent (no source) stay.
        let other = tempfile::tempdir().unwrap();
        std::fs::write(other.path().join("theirs.md"), "theirs").unwrap();
        import_dir(&mut s, other.path(), "other", false).unwrap();
        s.insert(NewMemory {
            name: Some("by-agent".into()),
            title: "t".into(),
            body: "b".into(),
            ..Default::default()
        })
        .unwrap();
        assert_eq!(s.count().unwrap(), 5);

        // One file is deleted and another changes its frontmatter name, so the old name's
        // memory is stale.
        std::fs::remove_file(dir.path().join("sub/gone.md")).unwrap();
        w("renamed.md", "---\nname: after\n---\nbody");
        let r = import_dir(&mut s, dir.path(), "mine", false).unwrap();
        assert!(r.pruned.is_empty(), "no prune without the flag");
        assert_eq!(s.count().unwrap(), 6);

        let r = import_dir(&mut s, dir.path(), "mine", true).unwrap();
        assert_eq!(r.pruned, vec!["before", "gone"]);
        for (name, present) in [
            ("keep", true),
            ("after", true),
            ("before", false),
            ("gone", false),
            ("theirs", true),
            ("by-agent", true),
        ] {
            assert_eq!(s.get_by_name(name).is_ok(), present, "{name}");
        }

        // An empty folder (a missing mount) prunes nothing.
        let empty = tempfile::tempdir().unwrap();
        let r = import_dir(&mut s, empty.path(), "mine", true).unwrap();
        assert!(r.pruned.is_empty());
        assert!(s.get_by_name("keep").is_ok());
    }

    #[test]
    fn the_command_line_needs_a_folder_and_an_explicit_source() {
        let a = |v: &[&str]| parse_args(&v.iter().map(|s| s.to_string()).collect::<Vec<_>>());
        let ok = |dir: &str, source: &str, prune: bool| {
            Ok(ImportArgs {
                dir: dir.into(),
                source: source.into(),
                prune,
            })
        };
        assert_eq!(a(&["/m", "--source", "notes"]), ok("/m", "notes", false));
        assert_eq!(
            a(&["--prune", "--source", "notes", "/m"]),
            ok("/m", "notes", true)
        );
        for bad in [
            &["/m"][..],
            &["/m", "--prune"],
            &["/m", "--source"],
            &["/m", "--source", "  "],
            &["--source", "notes"],
            &["/m", "/n", "--source", "notes"],
            &["/m", "--source", "notes", "--sorce"],
        ] {
            assert!(a(bad).is_err(), "{bad:?}");
        }
        let msg = a(&["/m"]).unwrap_err();
        assert!(msg.contains("--source"), "{msg}");
    }
}
