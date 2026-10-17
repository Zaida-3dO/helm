//! Obsidian vault docs.
//!
//! - **The vault list** is Obsidian's own registry (`%APPDATA%\obsidian\obsidian.json`, read only)
//!   plus folders the user adds in Settings → Vaults. A vault's name is its folder name. In debug
//!   builds `HELM_TEST_OBSIDIAN_CONFIG` points the registry at another file, for the end-to-end
//!   checks; release builds ignore it.
//! - **Resolution** follows Obsidian's own rule: an exact path first, then a unique file name. An
//!   `obsidian://` link names its vault; otherwise the vault containing the chat's folder, else the
//!   default vault, else the first. An ambiguous name returns the candidates.
//! - **Reads** are confined to the vault folder: the joined path passes the lexical gate, no links
//!   along it, private roots, and must stay under the vault root. Notes up to 512 KiB, images up to
//!   8 MiB (for embeds). The file-name index skips `.obsidian`, `.git`, `node_modules` and `.trash`,
//!   holds at most 50,000 files, and is rebuilt when older than 60 s.
//! - **Open in Obsidian** builds the `obsidian://open` URL here, from a known vault and a checked
//!   path, rather than widening the external-link allowlist.

use crate::private::PrivateRoots;
use crate::viewer::{self, admit};
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

pub const TEST_CONFIG_ENV: &str = "HELM_TEST_OBSIDIAN_CONFIG";
pub const INDEX_LIMIT: usize = 50_000;
pub const INDEX_TTL: Duration = Duration::from_secs(60);
const SKIP: &[&str] = &[".obsidian", ".git", "node_modules", ".trash"];

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VaultInfo {
    pub name: String,
    pub path: String,
    /// `obsidian` (from Obsidian's registry) or `user` (added in Settings).
    pub source: &'static str,
    pub is_default: bool,
}

/// Where Obsidian's registry is: the debug-only override, else `%APPDATA%\obsidian\obsidian.json`.
pub fn obsidian_config_path() -> Option<PathBuf> {
    if cfg!(debug_assertions) {
        if let Some(p) = std::env::var_os(TEST_CONFIG_ENV).filter(|v| !v.is_empty()) {
            return Some(PathBuf::from(p));
        }
    }
    std::env::var_os("APPDATA").map(|a| PathBuf::from(a).join("obsidian").join("obsidian.json"))
}

/// The vault folders named in an `obsidian.json` (`{"vaults": {"<id>": {"path": …}}}`).
pub fn parse_obsidian_config(text: &str) -> Vec<PathBuf> {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(text) else {
        return vec![];
    };
    let Some(vaults) = v.get("vaults").and_then(|x| x.as_object()) else {
        return vec![];
    };
    let mut out: Vec<PathBuf> = vaults
        .values()
        .filter_map(|e| e.get("path").and_then(|p| p.as_str()))
        .filter(|p| crate::pathgate::check(p).is_ok())
        .map(PathBuf::from)
        .collect();
    out.sort();
    out.dedup();
    out
}

pub fn vault_name(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string_lossy().into_owned())
}

/// Builds the list: Obsidian's vaults, then the user's, de-duplicated by folder.
pub fn list(obsidian: &[PathBuf], user: &[PathBuf], default_name: Option<&str>) -> Vec<VaultInfo> {
    let mut out: Vec<VaultInfo> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for (paths, source) in [(obsidian, "obsidian"), (user, "user")] {
        for p in paths {
            if !seen.insert(p.to_string_lossy().to_lowercase()) {
                continue;
            }
            out.push(VaultInfo {
                name: vault_name(p),
                path: p.to_string_lossy().into_owned(),
                source,
                is_default: false,
            });
        }
    }
    let pick = default_name
        .and_then(|d| out.iter().position(|v| v.name.eq_ignore_ascii_case(d)))
        .or(if out.is_empty() { None } else { Some(0) });
    if let Some(i) = pick {
        out[i].is_default = true;
    }
    out
}

pub fn init(tx: &rusqlite::Transaction<'_>) -> rusqlite::Result<()> {
    tx.execute_batch(
        "CREATE TABLE IF NOT EXISTS panel_vaults (path TEXT PRIMARY KEY, added_at INTEGER NOT NULL);
         CREATE TABLE IF NOT EXISTS panel_vault_default (id INTEGER PRIMARY KEY CHECK (id = 1), name TEXT NOT NULL);",
    )
}

/// The user's added vault folders and default vault name.
pub fn stored(tx: &rusqlite::Transaction<'_>) -> rusqlite::Result<(Vec<PathBuf>, Option<String>)> {
    let mut st = tx.prepare("SELECT path FROM panel_vaults ORDER BY added_at, path")?;
    let user = st
        .query_map([], |r| r.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let default = tx
        .query_row(
            "SELECT name FROM panel_vault_default WHERE id = 1",
            [],
            |r| r.get::<_, String>(0),
        )
        .ok();
    Ok((user.into_iter().map(PathBuf::from).collect(), default))
}

/// The current list from Obsidian's registry and the stored settings.
pub fn current(db: &helm_core::db::Database) -> Vec<VaultInfo> {
    let obsidian = obsidian_config_path()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .map(|t| parse_obsidian_config(&t))
        .unwrap_or_default();
    let (user, default) = db.snapshot(stored).unwrap_or_default();
    list(&obsidian, &user, default.as_deref())
}

/// Adds the vault instruction fragment to every agent launch.
pub struct VaultInstructions {
    pub db: Arc<helm_core::db::Database>,
}

impl helm_workers::contrib::LaunchContributor for VaultInstructions {
    fn contribute(
        &self,
        _ctx: &helm_workers::contrib::LaunchCtx<'_>,
    ) -> helm_workers::contrib::LaunchExtras {
        helm_workers::contrib::LaunchExtras {
            system_prompt: vec![instructions(&current(&self.db))],
            ..Default::default()
        }
    }
}

/// The vault a link means: the one it names, the one containing the chat's folder, the default.
pub fn choose<'a>(
    vaults: &'a [VaultInfo],
    named: Option<&str>,
    folder: Option<&Path>,
) -> Result<&'a VaultInfo, String> {
    if let Some(n) = named {
        return vaults
            .iter()
            .find(|v| v.name.eq_ignore_ascii_case(n))
            .ok_or_else(|| format!("No vault named \"{n}\" is known to Helm."));
    }
    if let Some(f) = folder {
        let f = f.to_string_lossy().to_lowercase();
        if let Some(v) = vaults.iter().find(|v| {
            let root = v.path.to_lowercase();
            f == root
                || f.starts_with(&format!(
                    "{}{}",
                    root.trim_end_matches(['\\', '/']),
                    std::path::MAIN_SEPARATOR
                ))
        }) {
            return Ok(v);
        }
    }
    vaults
        .iter()
        .find(|v| v.is_default)
        .or(vaults.first())
        .ok_or_else(|| "No Obsidian vault is set up. Add one in Settings → Vaults.".into())
}

/// A vault-relative note path, checked as text: no absolute part, no `..`, no control characters.
fn clean_rel(note: &str) -> Result<String, String> {
    let n = note.trim().replace('\\', "/");
    let n = n.trim_start_matches("./");
    if n.is_empty() || n.len() > 4096 || n.chars().any(char::is_control) {
        return Err("The note name is not valid.".into());
    }
    if n.starts_with('/')
        || (n.len() >= 2 && n.as_bytes()[1] == b':')
        || n.split('/').any(|s| s == "..")
    {
        return Err("Vault links must stay inside the vault.".into());
    }
    Ok(n.to_owned())
}

/// The full path of a vault-relative path, after every read check, confined to the vault root.
pub fn confine(vault: &Path, rel: &str, roots: &PrivateRoots) -> Result<PathBuf, String> {
    let rel = clean_rel(rel)?;
    let joined = vault.join(rel.replace('/', std::path::MAIN_SEPARATOR_STR));
    let path = admit(&joined.to_string_lossy(), roots).map_err(|e| e.message())?;
    let norm = |p: &Path| {
        p.to_string_lossy()
            .replace('/', "\\")
            .trim_end_matches('\\')
            .to_lowercase()
    };
    let (root, full) = (norm(vault), norm(&path));
    if full != root && !full.starts_with(&format!("{root}\\")) {
        return Err("Vault links must stay inside the vault.".into());
    }
    Ok(path)
}

struct Index {
    root: PathBuf,
    built: Instant,
    files: Arc<Vec<String>>,
}

fn cache() -> &'static Mutex<Vec<Index>> {
    static C: OnceLock<Mutex<Vec<Index>>> = OnceLock::new();
    C.get_or_init(|| Mutex::new(Vec::new()))
}

/// Every file under the vault (relative, forward slashes), skipping the noise folders and links.
pub fn index(root: &Path) -> Arc<Vec<String>> {
    let mut c = cache().lock().unwrap_or_else(|e| e.into_inner());
    c.retain(|i| i.built.elapsed() < INDEX_TTL);
    if let Some(i) = c.iter().find(|i| i.root == root) {
        return i.files.clone();
    }
    let mut out = Vec::new();
    let mut stack = vec![PathBuf::new()];
    while let Some(rel) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(root.join(&rel)) else {
            continue;
        };
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            let Ok(meta) = e.metadata() else { continue };
            if helm_platform::fs::is_link_like(&meta) {
                continue;
            }
            let child = rel.join(&name);
            if meta.is_dir() {
                if !SKIP.contains(&name.as_str()) {
                    stack.push(child);
                }
            } else if meta.is_file() && out.len() < INDEX_LIMIT {
                out.push(child.to_string_lossy().replace('\\', "/"));
            }
        }
        if out.len() >= INDEX_LIMIT {
            break;
        }
    }
    let files = Arc::new(out);
    c.push(Index {
        root: root.to_path_buf(),
        built: Instant::now(),
        files: files.clone(),
    });
    if c.len() > 4 {
        c.remove(0);
    }
    files
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase", untagged)]
pub enum Resolution {
    Found {
        vault: String,
        path: String,
    },
    Candidates {
        vault: String,
        candidates: Vec<String>,
    },
}

/// Obsidian's rule: an exact path (with or without `.md`, also beside `from`), then a unique file
/// name anywhere in the vault.
pub fn resolve_in(
    files: &[String],
    note: &str,
    from_dir: Option<&str>,
) -> Result<Vec<String>, String> {
    let rel = clean_rel(note)?;
    let lower = rel.to_lowercase();
    let wanted = if lower.ends_with(".md") {
        vec![lower.clone()]
    } else {
        vec![format!("{lower}.md"), lower.clone()]
    };
    let mut prefixes = vec![String::new()];
    if let Some(d) = from_dir.filter(|d| !d.is_empty()) {
        prefixes.insert(0, format!("{}/", d.trim_end_matches('/').to_lowercase()));
    }
    for p in &prefixes {
        for w in &wanted {
            let full = format!("{p}{w}");
            if let Some(hit) = files.iter().find(|f| f.to_lowercase() == full) {
                return Ok(vec![hit.clone()]);
            }
        }
    }
    let base = |s: &str| s.rsplit('/').next().unwrap_or(s).to_lowercase();
    let wanted_base: Vec<String> = wanted.iter().map(|w| base(w)).collect();
    let hits: Vec<String> = files
        .iter()
        .filter(|f| wanted_base.contains(&base(f)))
        .cloned()
        .collect();
    if hits.is_empty() {
        return Err(format!("No note named \"{rel}\" in this vault."));
    }
    Ok(hits)
}

pub fn resolve(
    vault: &VaultInfo,
    note: &str,
    from_path: Option<&str>,
) -> Result<Resolution, String> {
    let files = index(Path::new(&vault.path));
    let from_dir = from_path.and_then(|p| {
        p.replace('\\', "/")
            .rsplit_once('/')
            .map(|(d, _)| d.to_owned())
    });
    let hits = resolve_in(&files, note, from_dir.as_deref())?;
    if hits.len() == 1 {
        return Ok(Resolution::Found {
            vault: vault.name.clone(),
            path: hits[0].clone(),
        });
    }
    Ok(Resolution::Candidates {
        vault: vault.name.clone(),
        candidates: hits.into_iter().take(50).collect(),
    })
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VaultDoc {
    pub vault: String,
    pub path: String,
    pub title: String,
    /// The note's full path on disk (for Reveal).
    pub full_path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    /// Base64 bytes of an image (for embeds).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bytes: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mime: Option<&'static str>,
    pub truncated: bool,
}

pub fn read(vault: &VaultInfo, rel: &str, roots: &PrivateRoots) -> Result<VaultDoc, String> {
    let path = confine(Path::new(&vault.path), rel, roots)?;
    let ext = path
        .extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    if viewer::image_mime(&ext).is_none() && ext != "md" && ext != "txt" && ext != "canvas" {
        return Err("Only notes and images open from a vault.".into());
    }
    let v = viewer::read(&path.to_string_lossy(), roots).map_err(|e| e.message())?;
    let title = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    Ok(VaultDoc {
        vault: vault.name.clone(),
        path: rel.replace('\\', "/"),
        title,
        full_path: path.to_string_lossy().into_owned(),
        text: v.text,
        bytes: v.bytes,
        mime: v.mime,
        truncated: v.truncated,
    })
}

/// `obsidian://open?vault=…&file=…`, built from a known vault and a checked relative path.
pub fn obsidian_url(vault: &VaultInfo, rel: &str) -> Result<String, String> {
    let rel = clean_rel(rel)?;
    let enc = |s: &str| {
        s.bytes()
            .map(|b| match b {
                b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                    (b as char).to_string()
                }
                _ => format!("%{b:02X}"),
            })
            .collect::<String>()
    };
    let file = rel.strip_suffix(".md").unwrap_or(&rel);
    Ok(format!(
        "obsidian://open?vault={}&file={}",
        enc(&vault.name),
        enc(file)
    ))
}

/// Opens an `obsidian://` URL through the shell's protocol handler (Obsidian, when installed).
#[cfg(windows)]
pub fn open_url(url: &str) -> std::io::Result<()> {
    if !url.starts_with("obsidian://open?") {
        return Err(std::io::Error::other(
            "Only obsidian://open links are opened here.",
        ));
    }
    use windows_sys::Win32::UI::Shell::ShellExecuteW;
    let w = |s: &str| {
        s.encode_utf16()
            .chain(std::iter::once(0))
            .collect::<Vec<u16>>()
    };
    let (verb, file) = (w("open"), w(url));
    // SAFETY: NUL-terminated strings that outlive the call.
    let r = unsafe {
        ShellExecuteW(
            std::ptr::null_mut(),
            verb.as_ptr(),
            file.as_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            1,
        )
    };
    if (r as isize) <= 32 {
        return Err(std::io::Error::other(
            "Obsidian could not be opened. Is it installed?",
        ));
    }
    Ok(())
}

#[cfg(not(windows))]
pub fn open_url(_url: &str) -> std::io::Result<()> {
    Err(std::io::Error::other("Opening Obsidian is Windows-only."))
}

/// The instruction fragment for agents: the link forms the
/// panel opens, so links actually appear in replies.
pub fn instructions(vaults: &[VaultInfo]) -> String {
    let mut s = String::from(
        "Helm shows your links in a side panel. When you mention a file, write its path in inline code, \
         absolute where you can, with an optional line or range: `C:\\path\\to\\file.ts:42` or \
         `src/app.ts:10-20`.",
    );
    if !vaults.is_empty() {
        let names: Vec<String> = vaults.iter().map(|v| format!("\"{}\"", v.name)).collect();
        s.push_str(&format!(
            " The user keeps notes in Obsidian (vaults: {}). When you mention a note, link it as \
             [[Note name]] or obsidian://open?vault=<vault>&file=<path without .md>.",
            names.join(", ")
        ));
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vi(name: &str, path: &str, default: bool) -> VaultInfo {
        VaultInfo {
            name: name.into(),
            path: path.into(),
            source: "user",
            is_default: default,
        }
    }

    #[test]
    fn obsidian_registry_parsing_keeps_only_local_absolute_folders() {
        let text = r#"{"vaults":{"a":{"path":"C:\\Notes\\Main","ts":1},"b":{"path":"\\\\host\\share\\V"},"c":{"path":"relative"},"d":{}}}"#;
        let v = parse_obsidian_config(text);
        #[cfg(windows)]
        assert_eq!(v, vec![PathBuf::from(r"C:\Notes\Main")]);
        assert!(parse_obsidian_config("not json").is_empty());
    }

    #[test]
    fn the_list_merges_sources_and_marks_one_default() {
        let l = list(
            &[PathBuf::from(r"C:\Notes\Main")],
            &[
                PathBuf::from(r"C:\Notes\Work"),
                PathBuf::from(r"c:\notes\main"),
            ],
            Some("work"),
        );
        assert_eq!(
            l.iter()
                .map(|v| (v.name.as_str(), v.source, v.is_default))
                .collect::<Vec<_>>(),
            [("Main", "obsidian", false), ("Work", "user", true)]
        );
        assert!(list(&[], &[], None).is_empty());
        assert!(
            list(&[PathBuf::from(r"C:\A")], &[], None)[0].is_default,
            "the first is the default when none is set"
        );
    }

    #[test]
    fn a_link_resolves_in_the_named_vault_then_the_chats_then_the_default() {
        let vs = vec![
            vi("Main", r"C:\Notes\Main", false),
            vi("Work", r"C:\Notes\Work", true),
        ];
        assert_eq!(choose(&vs, Some("main"), None).unwrap().name, "Main");
        assert!(choose(&vs, Some("Nope"), None).is_err());
        #[cfg(windows)]
        assert_eq!(
            choose(&vs, None, Some(Path::new(r"C:\Notes\Main\project")))
                .unwrap()
                .name,
            "Main"
        );
        assert_eq!(
            choose(&vs, None, Some(Path::new(r"D:\code"))).unwrap().name,
            "Work"
        );
        assert!(choose(&[], None, None).is_err());
    }

    #[test]
    fn notes_resolve_by_exact_path_then_unique_name_and_list_ambiguity() {
        let files: Vec<String> = [
            "Daily/2026-10-17.md",
            "Projects/Helm.md",
            "Archive/Helm.md",
            "Inbox.md",
            "img/a.png",
            "Projects/Plan.md",
        ]
        .map(String::from)
        .to_vec();
        assert_eq!(resolve_in(&files, "Inbox", None).unwrap(), ["Inbox.md"]);
        assert_eq!(
            resolve_in(&files, "Projects/Helm", None).unwrap(),
            ["Projects/Helm.md"]
        );
        assert_eq!(
            resolve_in(&files, "helm", None).unwrap().len(),
            2,
            "an ambiguous name lists candidates"
        );
        assert_eq!(
            resolve_in(&files, "Helm", Some("Archive")).unwrap(),
            ["Archive/Helm.md"],
            "beside the current note first"
        );
        assert_eq!(
            resolve_in(&files, "Plan", None).unwrap(),
            ["Projects/Plan.md"]
        );
        assert_eq!(resolve_in(&files, "a.png", None).unwrap(), ["img/a.png"]);
        assert!(resolve_in(&files, "Missing", None).is_err());
        for bad in ["../x", "/abs", r"C:\x", "a/../../b", ""] {
            assert!(resolve_in(&files, bad, None).is_err(), "{bad}");
        }
    }

    #[test]
    fn reads_are_confined_to_the_vault() {
        let dir = tempfile::tempdir().unwrap();
        let vault = dir.path().join("Vault");
        std::fs::create_dir_all(vault.join("Sub")).unwrap();
        std::fs::write(vault.join("Sub").join("Note.md"), "# Title\n").unwrap();
        std::fs::write(dir.path().join("outside.md"), "secret").unwrap();
        let v = vi("Vault", &vault.to_string_lossy(), true);
        let roots = PrivateRoots::default();
        let doc = read(&v, "Sub/Note.md", &roots).unwrap();
        assert_eq!(
            (doc.title.as_str(), doc.text.as_deref()),
            ("Note", Some("# Title\n"))
        );
        for bad in [
            "../outside.md",
            "Sub/../../outside.md",
            &dir.path().join("outside.md").to_string_lossy(),
        ] {
            assert!(read(&v, bad, &roots).is_err(), "{bad}");
        }
        assert!(
            read(&v, "Sub/script.ps1", &roots).is_err(),
            "only notes and images"
        );
    }

    #[cfg(windows)]
    #[test]
    fn a_junction_inside_the_vault_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let vault = dir.path().join("Vault");
        let outside = dir.path().join("Outside");
        std::fs::create_dir_all(&vault).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("Leak.md"), "x").unwrap();
        let made = std::process::Command::new("cmd")
            .args(["/c", "mklink", "/J"])
            .arg(vault.join("link"))
            .arg(&outside)
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);
        assert!(made);
        let v = vi("Vault", &vault.to_string_lossy(), true);
        assert!(read(&v, "link/Leak.md", &PrivateRoots::default()).is_err());
        assert!(
            !index(&vault).iter().any(|f| f.contains("Leak")),
            "the index never follows links"
        );
    }

    #[test]
    fn open_in_obsidian_urls_are_built_from_a_known_vault_and_encoded() {
        let v = vi("My Vault", r"C:\N", true);
        assert_eq!(
            obsidian_url(&v, "Folder/Note One.md").unwrap(),
            "obsidian://open?vault=My%20Vault&file=Folder%2FNote%20One"
        );
        assert!(obsidian_url(&v, "../x").is_err());
    }

    #[test]
    fn the_instructions_name_the_link_forms_and_the_vaults() {
        let s = instructions(&[vi("Main", r"C:\N", true)]);
        assert!(
            s.contains("[[Note name]]")
                && s.contains("obsidian://open?vault=")
                && s.contains("\"Main\"")
                && s.contains(":42")
        );
        assert!(!instructions(&[]).contains("obsidian://"));
    }
}
