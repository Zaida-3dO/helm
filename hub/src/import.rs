//! `helm-hub import <dir>`: loads a folder of Markdown memory files.
//!
//! Each `*.md` file in the folder and its subfolders (hidden folders are
//! left out) becomes one memory keyed on its frontmatter `name` (falling
//! back to the file stem when there is no frontmatter), so running the
//! import again refreshes changed files and leaves the rest alone. Index
//! files are skipped: they list memories, they are not one. So is anything
//! under a folder named `archived` or a `.bak` folder, any `*.bak.md` file, and
//! any memory whose frontmatter `status` is `archived` or `superseded`: those
//! are retired and must not be found by a search. With `prune`, a memory of the
//! same source whose file is gone (or is now skipped for one of these reasons)
//! is removed.

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
    pub status: Option<String>,
    pub body: String,
    pub links: Vec<String>,
}

/// What the command line asked for.
#[derive(Debug, PartialEq)]
pub struct ImportArgs {
    pub dir: PathBuf,
    pub source: String,
    pub prune: bool,
    /// Allow a folder imported before under another source tag to be imported under this one.
    pub retag: bool,
}

/// True when a path below the import folder is retired by where it sits: a folder named
/// `archived` or ending `.bak` anywhere above it, or a file named `*.bak.md`.
pub fn retired_path(rel: &str) -> bool {
    let rel = rel.replace('\\', "/");
    // A doc cut into parts is keyed `file#2`: the part number does not change what the file is.
    let rel = match rel.rsplit_once('#') {
        Some((file, n)) if !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()) => file,
        _ => rel.as_str(),
    };
    let mut parts: Vec<String> = rel.split('/').map(str::to_lowercase).collect();
    let file = parts.pop().unwrap_or_default();
    file.ends_with(".bak.md") || parts.iter().any(|d| d == "archived" || d.ends_with(".bak"))
}

/// True when the frontmatter `status` retires the memory.
fn retired_status(status: Option<&str>) -> bool {
    status.is_some_and(|s| {
        let s = s.trim().to_lowercase();
        s == "archived" || s == "superseded"
    })
}

/// Reads `<dir> --source S [--prune] [--retag]`. The source is required: it decides which memories a prune
/// may remove, and a default would quietly re-tag everything imported before.
pub fn parse_args(args: &[String]) -> Result<ImportArgs, String> {
    let mut dir = None;
    let mut source = None;
    let mut prune = false;
    let mut retag = false;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--source" => match it.next() {
                Some(s) => source = Some(s.trim().to_owned()),
                None => return Err("--source needs a value".into()),
            },
            "--prune" => prune = true,
            "--retag" => retag = true,
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
            retag,
        }),
        _ => Err(
            "import needs --source <tag>: say which source these memories come from, \
                  so a re-import refreshes the same ones (for example --source my-notes)"
                .into(),
        ),
    }
}

const ROOTS: &str = "CREATE TABLE IF NOT EXISTS import_roots (
    path TEXT PRIMARY KEY,
    source_tag TEXT NOT NULL,
    updated_at INTEGER NOT NULL
)";

fn root_key(dir: &Path) -> Result<String, String> {
    Ok(std::fs::canonicalize(dir)
        .map_err(|e| format!("cannot read {}: {e}", dir.display()))?
        .to_string_lossy()
        .trim_start_matches(r"\\?\")
        .to_owned())
}

/// Refuses to import a folder under a source tag other than the one it was imported under before:
/// every memory would be written a second time under the new tag, and the old ones would sit beside
/// them. `retag` says the source really did change. Nothing is remembered here: the folder and
/// its tag are recorded by [`record_root`] once the import has succeeded, so a failed import
/// leaves the guard as it was.
pub fn check_root(store: &Store, dir: &Path, tag: &str, retag: bool) -> Result<(), String> {
    let root = root_key(dir)?;
    let conn = store.conn();
    let fail = |e: rusqlite::Error| format!("cannot read the import folders: {e}");
    conn.execute_batch(ROOTS).map_err(fail)?;
    let before: Option<String> = conn
        .query_row(
            "SELECT source_tag FROM import_roots WHERE path = ?1",
            [&root],
            |r| r.get(0),
        )
        .ok();
    if let Some(old) = before.filter(|old| old != tag && !retag) {
        return Err(format!(
            "{root} was imported as source {old:?}; importing it as {tag:?} would add every memory \
             a second time. Use --source {old:?}, or add --retag if the source really changed."
        ));
    }
    Ok(())
}

/// Remember the folder and the tag it was imported under, after the import has succeeded.
pub fn record_root(store: &Store, dir: &Path, tag: &str) -> Result<(), String> {
    let root = root_key(dir)?;
    let conn = store.conn();
    let fail = |e: rusqlite::Error| format!("cannot record the import folder: {e}");
    conn.execute_batch(ROOTS).map_err(fail)?;
    conn.execute(
        "INSERT INTO import_roots (path, source_tag, updated_at) VALUES (?1, ?2, strftime('%s','now'))
         ON CONFLICT(path) DO UPDATE SET source_tag = excluded.source_tag, updated_at = excluded.updated_at",
        rusqlite::params![root, tag],
    )
    .map_err(fail)?;
    Ok(())
}

/// Every `*.md` file under `dir`, folders sorted, hidden folders (and symlinked ones) skipped.
/// A symlinked file counts as the file it points at.
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
        } else if (kind.is_file() || (kind.is_symlink() && path.is_file()))
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
            || retired_path(&file_name)
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
        if retired_status(p.status.as_deref()) {
            report.skipped.push(file_name);
            continue;
        }
        // A memory is shared with the household once it is on the hub, so a file that holds a
        // credential stays out; a copy imported before stays as it was.
        if [&p.body, &p.title, &p.name]
            .into_iter()
            .chain(p.description.as_ref())
            .any(|t| crate::secrets::looks_like_secret(t))
        {
            failed_sources.insert(source.clone());
            report.failed.push(format!(
                "{file_name}: looks like it holds a credential, so it is not imported"
            ));
            continue;
        }
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
            ..Default::default()
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
    // Only index files found means the memories are not there either: prune nothing.
    if prune && report.files > report.skipped.len() {
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
        status: fields.status,
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
    status: Option<String>,
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
            status: get(&map, "status").or_else(|| match map.get("metadata") {
                Some(serde_yaml::Value::Mapping(meta)) => get(meta, "status"),
                _ => None,
            }),
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
            ("status", _) if f.status.is_none() || indented => f.status = Some(v),
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
        assert_eq!((r1.inserted, r1.updated, r1.unchanged), (2, 0, 0));
        assert_eq!(r1.skipped, vec!["MEMORY.md", "archived/old.md"]);
        assert_eq!(r1.failed.len(), 1, "{:?}", r1.failed);
        assert!(r1.failed[0].starts_with("c.md"));
        assert_eq!(s.count().unwrap(), 2);

        let r2 = import_dir(&mut s, dir.path(), "default", false).unwrap();
        assert_eq!((r2.inserted, r2.updated, r2.unchanged), (0, 0, 2));
        assert_eq!(s.count().unwrap(), 2);

        w(
            "a.md",
            "---\nname: alpha\ndescription: d\nmetadata:\n  type: lesson\n---\nalpha body v2",
        );
        let r3 = import_dir(&mut s, dir.path(), "default", false).unwrap();
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
        assert!(s.get_by_name("old").is_err(), "archived/ is not imported");
        assert!(s.get_by_name("hidden").is_err());
    }

    #[test]
    fn retired_memories_are_skipped_and_prune_removes_ones_imported_earlier() {
        let dir = tempfile::tempdir().unwrap();
        // (file, text, memory name, imported)
        let rows: &[(&str, &str, &str, bool)] = &[
            ("live.md", "live body", "live", true),
            ("sub/deep.md", "deep body", "deep", true),
            ("archived/old.md", "old body", "old", false),
            ("sub/Archived/older.md", "older body", "older", false),
            ("copy.bak/x.md", "x body", "x", false),
            ("draft.bak.md", "draft body", "draft.bak", false),
            (
                "a.md",
                "---\nname: s-arch\nstatus: archived\n---\nb",
                "s-arch",
                false,
            ),
            (
                "b.md",
                "---\nname: s-sup\nmetadata:\n  status: Superseded\n---\nb",
                "s-sup",
                false,
            ),
            (
                "c.md",
                "---\nname: s-active\nstatus: active\n---\nb",
                "s-active",
                true,
            ),
        ];
        for (n, t, _, _) in rows {
            let p = dir.path().join(n);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, t).unwrap();
        }
        let mut s = Store::open_in_memory().unwrap();
        // Memories imported before the rule existed, under this source and another one.
        for (name, source) in [
            ("old", "mine:archived/old.md"),
            ("s-arch", "mine:a.md"),
            ("other-old", "other:archived/x.md"),
        ] {
            s.insert(NewMemory {
                name: Some(name.into()),
                title: name.into(),
                body: "b".into(),
                source: Some(source.into()),
                ..Default::default()
            })
            .unwrap();
        }

        let r = import_dir(&mut s, dir.path(), "mine", false).unwrap();
        assert_eq!(r.imported(), 3);
        assert_eq!(r.skipped.len(), 6, "{:?}", r.skipped);
        assert!(r.pruned.is_empty(), "no prune without the flag");
        assert!(s.get_by_name("old").is_ok(), "kept until --prune is passed");

        let r = import_dir(&mut s, dir.path(), "mine", true).unwrap();
        assert_eq!(r.pruned, vec!["old", "s-arch"]);
        for (file, _, name, imported) in rows {
            assert_eq!(s.get_by_name(name).is_ok(), *imported, "{file}");
        }
        assert!(
            s.get_by_name("other-old").is_ok(),
            "another source is never touched"
        );
        // A file that holds a credential is reported and left out: the memory would be shared.
        let leaked = format!(
            "---\nname: keys\n---\nexport GITHUB_TOKEN=ghp_{}",
            "a1B2".repeat(5)
        );
        std::fs::write(dir.path().join("keys.md"), leaked).unwrap();
        let r = import_dir(&mut s, dir.path(), "mine", true).unwrap();
        assert!(
            r.failed.len() == 1
                && r.failed[0].starts_with("keys.md")
                && r.failed[0].contains("credential"),
            "{:?}",
            r.failed
        );
        assert!(s.get_by_name("keys").is_err());
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

        // An empty folder (a missing mount), or one holding only an index file, prunes nothing.
        let empty = tempfile::tempdir().unwrap();
        let r = import_dir(&mut s, empty.path(), "mine", true).unwrap();
        assert!(r.pruned.is_empty());
        std::fs::write(empty.path().join("MEMORY.md"), "- [keep](keep.md)").unwrap();
        let r = import_dir(&mut s, empty.path(), "mine", true).unwrap();
        assert!(r.pruned.is_empty());
        assert!(s.get_by_name("keep").is_ok());

        // A symlinked memory file is imported like the file it points at, so prune keeps it.
        #[cfg(unix)]
        {
            let target = other.path().join("linked-target.md");
            std::fs::write(&target, "---\nname: linked\n---\nbody").unwrap();
            std::os::unix::fs::symlink(&target, dir.path().join("linked.md")).unwrap();
            import_dir(&mut s, dir.path(), "mine", true).unwrap();
            assert!(s.get_by_name("linked").is_ok());
            let r = import_dir(&mut s, dir.path(), "mine", true).unwrap();
            assert!(r.pruned.is_empty(), "{:?}", r.pruned);
            assert!(s.get_by_name("linked").is_ok());
        }
    }

    #[test]
    fn a_folder_keeps_the_source_tag_it_was_first_imported_under() {
        let dir = tempfile::tempdir().unwrap();
        let s = Store::open_in_memory().unwrap();
        assert_eq!(check_root(&s, dir.path(), "notes", false), Ok(()));
        // A check alone remembers nothing: an import that failed leaves the guard as it was.
        assert_eq!(check_root(&s, dir.path(), "default", false), Ok(()));
        assert_eq!(record_root(&s, dir.path(), "notes"), Ok(()));
        assert_eq!(check_root(&s, dir.path(), "notes", false), Ok(()));
        let why = check_root(&s, dir.path(), "default", false).unwrap_err();
        assert!(
            why.contains("\"notes\"") && why.contains("--retag"),
            "{why}"
        );
        assert_eq!(check_root(&s, dir.path(), "default", true), Ok(()));
        assert_eq!(record_root(&s, dir.path(), "default"), Ok(()));
        assert_eq!(check_root(&s, dir.path(), "default", false), Ok(()));
        let other = tempfile::tempdir().unwrap();
        assert_eq!(check_root(&s, other.path(), "notes", false), Ok(()));
        for (rel, retired) in [
            ("a.md", false),
            ("archived/a.md", true),
            ("x/Archived/a.md", true),
            ("x.bak/a.md", true),
            ("a.bak.md", true),
            ("archive-notes.md", false),
            ("x.bak.md#2", true),
            ("archived\\x.md", true),
            ("sub\\x.bak\\a.md#10", true),
            ("a.md#2", false),
            ("a.md#x", false),
        ] {
            assert_eq!(retired_path(rel), retired, "{rel}");
        }
    }

    #[test]
    fn the_command_line_needs_a_folder_and_an_explicit_source() {
        let a = |v: &[&str]| parse_args(&v.iter().map(|s| s.to_string()).collect::<Vec<_>>());
        let ok = |dir: &str, source: &str, prune: bool| {
            Ok(ImportArgs {
                dir: dir.into(),
                source: source.into(),
                prune,
                retag: false,
            })
        };
        assert_eq!(a(&["/m", "--source", "notes"]), ok("/m", "notes", false));
        assert_eq!(
            a(&["--prune", "--source", "notes", "/m"]),
            ok("/m", "notes", true)
        );
        assert!(a(&["/m", "--source", "notes", "--retag"]).unwrap().retag);
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
