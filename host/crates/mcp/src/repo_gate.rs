//! The gate for repository-defined MCP servers.
//!
//! With native Claude Code MCP on (the default), the CLI would load the servers a
//! repository defines: its `.mcp.json` files and the `mcpServers` frontmatter of project agent
//! files (`.claude/agents/**/*.md`). Each is identified by (target, defining file, name) and bound
//! to the SHA-256 of its canonical entry, so changing the entry withdraws the approval. Unapproved
//! servers go to the CLI's disabled list; anything unreadable, oversized or unparsable turns
//! native MCP off for that chat, with a notice naming the file (fails closed). A server whose
//! name normalises to `helm`, or starts with `app_`, is reserved and never approvable.
//!
//! **Seeding.** The first time Helm sees a defining file it adopts what Claude Code
//! already allows there, so today's servers keep loading: `.mcp.json` entries named in the
//! project's `.claude/settings.local.json` / `.claude/settings.json` `enabledMcpjsonServers` (or
//! all of them under `enableAllProjectMcpServers`), minus `disabledMcpjsonServers`; agent-file
//! servers, which Claude Code loads without asking, are adopted as they are. Helm deliberately
//! does not read `~/.claude.json` (it can hold an API key, CLAUDE.md hard rule 2), so an approval
//! that exists only there is not seeded and shows as held in Settings → MCP. Seeding is not
//! budget-gated: it is what keeps the default from removing servers.
//!
//! Spec gap: YAML frontmatter is read with a small block parser, not a YAML library.
//! Only `mcpServers:` followed by indented `name:` blocks is understood; any other form under
//! that key fails closed.

use crate::validate::is_local_absolute;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use ts_rs::TS;

pub const MCP_JSON_MAX: u64 = 128 * 1024;
pub const AGENT_FILE_MAX: u64 = 1024 * 1024;
pub const MAX_AGENT_FILES: usize = 256;
pub const MAX_DEPTH: usize = 16;
pub const MAX_SERVERS_PER_FILE: usize = 64;
pub const MAX_NAME: usize = 128;
pub const APPROVALS_FILE: &str = "mcp-repo-approvals.json";
/// The "Use my Claude Code MCP servers" preference (user-only; default on).
pub const NATIVE_PREF: &str = "nativeMcp";
const TARGET: &str = "local";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum RepoServerState {
    Approved,
    Held,
    /// Named like Helm's own server: never approvable.
    Reserved,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct RepoServer {
    /// The chat folder this was read for.
    pub folder: String,
    /// The defining file, absolute.
    pub file: String,
    pub name: String,
    pub hash: String,
    pub state: RepoServerState,
}

/// What a launch in one folder gets (consumed by the launch flags).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct LaunchGate {
    /// Load native servers at all. False when the user turned them off or a file failed closed.
    pub native: bool,
    /// Server names the CLI must not load (held and reserved repository servers).
    pub disabled: Vec<String>,
    /// Shown in the chat when servers were held back or native MCP was turned off.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub notice: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Found {
    pub file: PathBuf,
    pub name: String,
    pub hash: String,
    pub from_agent_file: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Scan {
    Ok(Vec<Found>),
    /// Fails closed: native MCP off for this chat.
    Closed {
        file: PathBuf,
        reason: String,
    },
}

/// The user's "Use my Claude Code MCP servers" switch: on unless set to `false`.
pub fn native_on(db: &helm_core::db::Database) -> bool {
    db.snapshot(|tx| Ok::<_, rusqlite::Error>(helm_core::prefs::read(tx, NATIVE_PREF)))
        .ok()
        .flatten()
        != Some(Value::Bool(false))
}

pub fn is_reserved(name: &str) -> bool {
    let norm: String = name
        .to_ascii_lowercase()
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .collect();
    norm == "helm" || name.to_ascii_lowercase().starts_with("app_")
}

fn closed(file: &Path, reason: &str) -> Scan {
    Scan::Closed {
        file: file.to_owned(),
        reason: reason.to_owned(),
    }
}

/// Reads a regular file (no links), refusing anything over `cap`.
fn read_regular(p: &Path, cap: u64) -> Result<Option<Vec<u8>>, &'static str> {
    let meta = match std::fs::symlink_metadata(p) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err("it cannot be read"),
    };
    if !meta.file_type().is_file() {
        return Err("it is not a regular file");
    }
    if meta.len() > cap {
        return Err("it is too large");
    }
    std::fs::read(p).map(Some).map_err(|_| "it cannot be read")
}

fn valid_name(n: &str) -> bool {
    !n.is_empty() && n.len() <= MAX_NAME && n.trim() == n
}

fn hash_value(v: &Value) -> String {
    helm_core::sha256_hex(helm_core::revisions::canonical_json(v).as_bytes())
}

/// The folders whose `.mcp.json` and `.claude/agents` count for `folder`: it and its ancestors
/// up to the git root, never at or above the home folder.
pub fn governing_dirs(folder: &Path, home: Option<&Path>) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut cur = Some(folder.to_path_buf());
    while let Some(d) = cur {
        if let Some(h) = home {
            if h.starts_with(&d) {
                break; // at or above home
            }
        }
        out.push(d.clone());
        if d.join(".git").exists() {
            break;
        }
        cur = d.parent().map(Path::to_path_buf);
        if out.len() > 64 {
            break;
        }
    }
    out
}

fn scan_mcp_json(file: &Path, found: &mut Vec<Found>) -> Result<(), Scan> {
    let Some(bytes) = read_regular(file, MCP_JSON_MAX).map_err(|r| closed(file, r))? else {
        return Ok(());
    };
    let v: Value =
        serde_json::from_slice(&bytes).map_err(|_| closed(file, "it is not valid JSON"))?;
    let Some(servers) = v.get("mcpServers") else {
        return Ok(());
    };
    let servers = servers
        .as_object()
        .ok_or_else(|| closed(file, "mcpServers is not an object"))?;
    if servers.len() > MAX_SERVERS_PER_FILE {
        return Err(closed(file, "it defines more than 64 servers"));
    }
    for (name, entry) in servers {
        if !valid_name(name) {
            return Err(closed(file, "a server name is empty, too long or padded"));
        }
        found.push(Found {
            file: file.to_owned(),
            name: name.clone(),
            hash: hash_value(entry),
            from_agent_file: false,
        });
    }
    Ok(())
}

/// The `mcpServers` blocks of an agent file's frontmatter: name → block text.
pub fn frontmatter_servers(text: &str) -> Result<BTreeMap<String, String>, &'static str> {
    let mut out = BTreeMap::new();
    let mut lines = text.lines();
    if lines.next().map(str::trim_end) != Some("---") {
        return Ok(out);
    }
    let fm: Vec<&str> = lines.take_while(|l| l.trim_end() != "---").collect();
    let Some(start) = fm.iter().position(|l| l.starts_with("mcpServers:")) else {
        return Ok(out);
    };
    if !fm[start]["mcpServers:".len()..].trim().is_empty() {
        return Err("its mcpServers frontmatter is not in block form");
    }
    let mut child_indent: Option<usize> = None;
    let mut current: Option<(String, Vec<&str>)> = None;
    for l in &fm[start + 1..] {
        if l.trim().is_empty() {
            continue;
        }
        let indent = l.len() - l.trim_start().len();
        if indent == 0 {
            break; // the next top-level key
        }
        let ci = *child_indent.get_or_insert(indent);
        if indent < ci {
            return Err("its mcpServers frontmatter is indented inconsistently");
        }
        if indent == ci {
            let t = l.trim();
            let Some(name) = t
                .strip_suffix(':')
                .or_else(|| t.split_once(':').map(|(n, _)| n))
            else {
                return Err("its mcpServers frontmatter has an entry without a name");
            };
            let name = name.trim_matches(|c| c == '"' || c == '\'');
            if let Some((n, body)) = current.take() {
                out.insert(n, body.join("\n"));
            }
            if !valid_name(name) {
                return Err("a server name is empty, too long or padded");
            }
            current = Some((name.to_owned(), vec![t]));
        } else if let Some((_, body)) = current.as_mut() {
            body.push(l.trim_end());
        }
    }
    if let Some((n, body)) = current.take() {
        out.insert(n, body.join("\n"));
    }
    if out.len() > MAX_SERVERS_PER_FILE {
        return Err("it defines more than 64 servers");
    }
    Ok(out)
}

fn collect_agent_files(dir: &Path, depth: usize, out: &mut Vec<PathBuf>) -> Result<(), Scan> {
    if depth > MAX_DEPTH {
        return Ok(());
    }
    let Ok(rd) = std::fs::read_dir(dir) else {
        return Ok(());
    };
    let mut entries: Vec<_> = rd.flatten().collect();
    entries.sort_by_key(|e| e.file_name());
    for e in entries {
        let p = e.path();
        let Ok(meta) = std::fs::symlink_metadata(&p) else {
            continue;
        };
        if meta.file_type().is_symlink() {
            continue; // links are not followed
        }
        if meta.is_dir() {
            collect_agent_files(&p, depth + 1, out)?;
        } else if p.extension().is_some_and(|x| x.eq_ignore_ascii_case("md")) {
            out.push(p.clone());
            if out.len() > MAX_AGENT_FILES {
                return Err(closed(&p, "the folder has more than 256 agent files"));
            }
        }
    }
    Ok(())
}

/// Every repository server that would load for a chat in `folder`.
pub fn scan(folder: &Path, home: Option<&Path>) -> Scan {
    let mut found = Vec::new();
    let mut agent_files = Vec::new();
    for d in governing_dirs(folder, home) {
        if let Err(c) = scan_mcp_json(&d.join(".mcp.json"), &mut found) {
            return c;
        }
        if let Err(c) = collect_agent_files(&d.join(".claude").join("agents"), 0, &mut agent_files)
        {
            return c;
        }
    }
    if agent_files.len() > MAX_AGENT_FILES {
        return closed(folder, "the folder has more than 256 agent files");
    }
    for f in agent_files {
        let bytes = match read_regular(&f, AGENT_FILE_MAX) {
            Ok(Some(b)) => b,
            Ok(None) => continue,
            Err(r) => return closed(&f, r),
        };
        let Ok(text) = String::from_utf8(bytes) else {
            return closed(&f, "it is not text");
        };
        match frontmatter_servers(&text) {
            Ok(m) => {
                for (name, body) in m {
                    found.push(Found {
                        file: f.clone(),
                        name,
                        hash: helm_core::sha256_hex(body.as_bytes()),
                        from_agent_file: true,
                    });
                }
            }
            Err(r) => return closed(&f, r),
        }
    }
    Scan::Ok(found)
}

/// What Claude Code already allows for `.mcp.json` servers in `dirs` (seeding).
fn mtime_ms(p: &Path) -> Option<i64> {
    let t = std::fs::symlink_metadata(p).ok()?.modified().ok()?;
    t.duration_since(std::time::UNIX_EPOCH)
        .ok()
        .map(|d| d.as_millis() as i64)
}

/// Settings files changed after `cutoff` are ignored: seeding adopts what Claude Code allowed
/// before Helm first ran the gate, never what appeared since.
fn claude_allowed(dirs: &[PathBuf], cutoff: i64) -> (bool, BTreeSet<String>, BTreeSet<String>) {
    let mut all = false;
    let mut enabled = BTreeSet::new();
    let mut disabled = BTreeSet::new();
    for d in dirs {
        for name in ["settings.local.json", "settings.json"] {
            let p = d.join(".claude").join(name);
            if mtime_ms(&p).is_none_or(|m| m > cutoff) {
                continue;
            }
            let Ok(Some(bytes)) = read_regular(&p, MCP_JSON_MAX) else {
                continue;
            };
            let Ok(v) = serde_json::from_slice::<Value>(&bytes) else {
                continue;
            };
            if v.get("enableAllProjectMcpServers") == Some(&Value::Bool(true)) {
                all = true;
            }
            for (key, set) in [
                ("enabledMcpjsonServers", &mut enabled),
                ("disabledMcpjsonServers", &mut disabled),
            ] {
                if let Some(a) = v.get(key).and_then(Value::as_array) {
                    set.extend(a.iter().filter_map(Value::as_str).map(str::to_owned));
                }
            }
        }
    }
    (all, enabled, disabled)
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
struct Approval {
    target: String,
    file: String,
    name: String,
    hash: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct ApprovalFile {
    version: u32,
    approvals: Vec<Approval>,
    /// Defining files already seeded once.
    seeded: Vec<String>,
    /// When the gate first ran. Only files (and Claude Code settings) last changed before it are
    /// seeded, so a repository cloned or edited later, or a file an agent writes, still needs the
    /// user's approval.
    #[serde(default)]
    seed_cutoff_ms: Option<i64>,
}

fn key_of(p: &Path) -> String {
    let s = p.to_string_lossy().replace('\\', "/");
    if cfg!(windows) {
        s.to_lowercase()
    } else {
        s
    }
}

pub struct RepoGate {
    path: PathBuf,
    home: Option<PathBuf>,
    lock: Mutex<()>,
    /// Folder key → the last scan's servers (Settings lists these; in memory only).
    seen: Mutex<BTreeMap<String, Vec<RepoServer>>>,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct GateError(pub String);

impl RepoGate {
    pub fn new(profile_dir: &Path, home: Option<PathBuf>) -> Self {
        Self {
            path: profile_dir.join(APPROVALS_FILE),
            home,
            lock: Mutex::new(()),
            seen: Mutex::new(BTreeMap::new()),
        }
    }

    fn load(&self) -> ApprovalFile {
        helm_core::privfs::read_capped(&self.path, 4 * 1024 * 1024)
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or(ApprovalFile {
                version: 1,
                ..Default::default()
            })
    }

    fn store(&self, f: &ApprovalFile) -> Result<(), GateError> {
        let bytes = serde_json::to_vec_pretty(f).expect("approvals serialise");
        helm_core::privfs::write_atomic(&self.path, &bytes)
            .map_err(|e| GateError(format!("Could not save MCP approvals: {e}")))
    }

    fn approved(f: &ApprovalFile, file: &str, name: &str, hash: &str) -> bool {
        f.approvals
            .iter()
            .any(|a| a.target == TARGET && a.file == file && a.name == name && a.hash == hash)
    }

    /// Scans `folder`, seeds first-seen files, and returns the launch decision. `native_on` is
    /// the user's "Use my Claude Code MCP servers" switch.
    pub fn for_launch(&self, folder: &Path, native_on: bool) -> LaunchGate {
        if !native_on {
            return LaunchGate {
                native: false,
                disabled: Vec::new(),
                notice: None,
            };
        }
        let found = match scan(folder, self.home.as_deref()) {
            Scan::Ok(f) => f,
            Scan::Closed { file, reason } => {
                return LaunchGate {
                    native: false,
                    disabled: Vec::new(),
                    notice: Some(format!(
                        "Claude Code's own MCP servers are off for this chat: {} could not be checked because {reason}.",
                        file.display()
                    )),
                }
            }
        };
        let _g = self.lock.lock().unwrap_or_else(|p| p.into_inner());
        let mut f = self.load();
        let mut changed = false;
        let cutoff = *f.seed_cutoff_ms.get_or_insert_with(|| {
            changed = true;
            helm_core::now_ms()
        });
        let dirs = governing_dirs(folder, self.home.as_deref());
        let (all, enabled, disabled) = claude_allowed(&dirs, cutoff);
        let files: BTreeSet<String> = found.iter().map(|x| key_of(&x.file)).collect();
        for file in &files {
            if f.seeded.contains(file) {
                continue;
            }
            for x in found.iter().filter(|x| &key_of(&x.file) == file) {
                if mtime_ms(&x.file).is_none_or(|m| m > cutoff) {
                    continue; // changed since the gate first ran: the user decides
                }
                let allowed = x.from_agent_file
                    || ((all || enabled.contains(&x.name)) && !disabled.contains(&x.name));
                if allowed && !is_reserved(&x.name) && !Self::approved(&f, file, &x.name, &x.hash) {
                    f.approvals.push(Approval {
                        target: TARGET.into(),
                        file: file.clone(),
                        name: x.name.clone(),
                        hash: x.hash.clone(),
                    });
                }
            }
            f.seeded.push(file.clone());
            changed = true;
        }
        if changed {
            let _ = self.store(&f);
        }
        let folder_s = folder.to_string_lossy().into_owned();
        let servers: Vec<RepoServer> = found
            .iter()
            .map(|x| {
                let file = key_of(&x.file);
                let state = if is_reserved(&x.name) {
                    RepoServerState::Reserved
                } else if Self::approved(&f, &file, &x.name, &x.hash) {
                    RepoServerState::Approved
                } else {
                    RepoServerState::Held
                };
                RepoServer {
                    folder: folder_s.clone(),
                    file: x.file.to_string_lossy().into_owned(),
                    name: x.name.clone(),
                    hash: x.hash.clone(),
                    state,
                }
            })
            .collect();
        let mut disabled_names: Vec<String> = servers
            .iter()
            .filter(|s| s.state != RepoServerState::Approved)
            .map(|s| s.name.clone())
            .collect();
        disabled_names.sort();
        disabled_names.dedup();
        let held = servers
            .iter()
            .filter(|s| s.state == RepoServerState::Held)
            .count();
        self.seen
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .insert(key_of(folder), servers);
        LaunchGate {
            native: true,
            disabled: disabled_names,
            notice: (held > 0).then(|| {
                format!("{held} repository MCP server(s) are held back until you approve them in Settings → MCP.")
            }),
        }
    }

    /// Every repository server seen since launch, for Settings → MCP.
    pub fn seen(&self) -> Vec<RepoServer> {
        self.seen
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .values()
            .flatten()
            .cloned()
            .collect()
    }

    /// Approves one server as it is now. Refused for a reserved name or a changed entry.
    pub fn approve(&self, file: &str, name: &str, hash: &str) -> Result<(), GateError> {
        if is_reserved(name) {
            return Err(GateError(format!(
                "{name} is named like Helm's own server, so it can never be approved."
            )));
        }
        if !is_local_absolute(file) {
            return Err(GateError(
                "The defining file must be a local absolute path.".into(),
            ));
        }
        let current = self
            .seen()
            .into_iter()
            .any(|s| s.file == file && s.name == name && s.hash == hash);
        if !current {
            return Err(GateError(
                "That server changed or is no longer defined. Start a chat in its folder and try again.".into(),
            ));
        }
        let _g = self.lock.lock().unwrap_or_else(|p| p.into_inner());
        let mut f = self.load();
        let key = key_of(Path::new(file));
        if !Self::approved(&f, &key, name, hash) {
            f.approvals.push(Approval {
                target: TARGET.into(),
                file: key,
                name: name.into(),
                hash: hash.into(),
            });
            self.store(&f)?;
        }
        self.mark(file, name, RepoServerState::Approved);
        Ok(())
    }

    pub fn revoke(&self, file: &str, name: &str) -> Result<(), GateError> {
        let _g = self.lock.lock().unwrap_or_else(|p| p.into_inner());
        let mut f = self.load();
        let key = key_of(Path::new(file));
        f.approvals.retain(|a| !(a.file == key && a.name == name));
        self.store(&f)?;
        drop(_g);
        self.mark(file, name, RepoServerState::Held);
        Ok(())
    }

    fn mark(&self, file: &str, name: &str, state: RepoServerState) {
        for list in self
            .seen
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .values_mut()
        {
            for s in list.iter_mut() {
                if s.file == file && s.name == name && s.state != RepoServerState::Reserved {
                    s.state = state;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn write(p: &Path, text: &str) {
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, text).unwrap();
    }

    fn repo() -> tempfile::TempDir {
        let d = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(d.path().join(".git")).unwrap();
        d
    }

    fn mcp_json(d: &Path, servers: Value) {
        write(
            &d.join(".mcp.json"),
            &json!({ "mcpServers": servers }).to_string(),
        );
    }

    #[test]
    fn seeding_adopts_claude_codes_approvals_once() {
        let r = repo();
        let prof = tempfile::tempdir().unwrap();
        mcp_json(
            r.path(),
            json!({"agent-standup": {"type": "http", "url": "https://a.example/mcp"},
                   "home-assistant": {"command": "x"}, "other": {"command": "y"}}),
        );
        write(
            &r.path().join(".claude/settings.local.json"),
            &json!({"enabledMcpjsonServers": ["agent-standup", "home-assistant"], "disabledMcpjsonServers": ["home-assistant"]}).to_string(),
        );
        let g = RepoGate::new(prof.path(), None);
        let l = g.for_launch(r.path(), true);
        assert!(l.native);
        assert_eq!(l.disabled, ["home-assistant", "other"]);
        assert!(l.notice.unwrap().contains("2 repository"));
        // Seeding happens once: a later Claude Code approval is not silently adopted.
        write(
            &r.path().join(".claude/settings.local.json"),
            &json!({"enableAllProjectMcpServers": true}).to_string(),
        );
        assert_eq!(
            g.for_launch(r.path(), true).disabled,
            ["home-assistant", "other"]
        );
    }

    #[test]
    fn files_that_appear_after_the_first_run_are_never_seeded() {
        let r = repo();
        let prof = tempfile::tempdir().unwrap();
        let g = RepoGate::new(prof.path(), None);
        // The first run: nothing to seed yet, but the cutoff is set now.
        assert!(g.for_launch(r.path(), true).disabled.is_empty());
        std::thread::sleep(std::time::Duration::from_millis(30));
        // A cloned repo (or an agent) adds servers and a settings file enabling them all.
        mcp_json(r.path(), json!({"s": {"command": "a"}}));
        write(
            &r.path().join(".claude/settings.json"),
            &json!({"enableAllProjectMcpServers": true}).to_string(),
        );
        write(
            &r.path().join(".claude/agents/a.md"),
            "---
mcpServers:
  t:
    command: x
---
",
        );
        assert_eq!(g.for_launch(r.path(), true).disabled, ["s", "t"]);
        // A fresh gate on the same profile keeps the original cutoff.
        let g2 = RepoGate::new(prof.path(), None);
        assert_eq!(g2.for_launch(r.path(), true).disabled, ["s", "t"]);
    }

    #[test]
    fn changing_an_entry_withdraws_its_approval() {
        let r = repo();
        let prof = tempfile::tempdir().unwrap();
        mcp_json(r.path(), json!({"s": {"command": "a"}}));
        write(
            &r.path().join(".claude/settings.json"),
            &json!({"enableAllProjectMcpServers": true}).to_string(),
        );
        let g = RepoGate::new(prof.path(), None);
        assert!(g.for_launch(r.path(), true).disabled.is_empty());
        mcp_json(r.path(), json!({"s": {"command": "b"}}));
        assert_eq!(g.for_launch(r.path(), true).disabled, ["s"]);
        // Approve the new entry from Settings.
        let s = g.seen().into_iter().find(|s| s.name == "s").unwrap();
        assert_eq!(s.state, RepoServerState::Held);
        g.approve(&s.file, "s", &s.hash).unwrap();
        assert!(g.for_launch(r.path(), true).disabled.is_empty());
        g.revoke(&s.file, "s").unwrap();
        assert_eq!(g.for_launch(r.path(), true).disabled, ["s"]);
        assert!(
            g.approve(&s.file, "s", &"0".repeat(64)).is_err(),
            "a stale hash is refused"
        );
    }

    #[test]
    fn reserved_names_are_never_approvable() {
        let r = repo();
        let prof = tempfile::tempdir().unwrap();
        mcp_json(
            r.path(),
            json!({"helm": {"command": "a"}, "app_tools": {"command": "b"}}),
        );
        write(
            &r.path().join(".claude/settings.json"),
            &json!({"enableAllProjectMcpServers": true}).to_string(),
        );
        let g = RepoGate::new(prof.path(), None);
        assert_eq!(g.for_launch(r.path(), true).disabled, ["app_tools", "helm"]);
        let s = g.seen().into_iter().find(|s| s.name == "helm").unwrap();
        assert_eq!(s.state, RepoServerState::Reserved);
        assert!(g.approve(&s.file, "helm", &s.hash).is_err());
        assert!(is_reserved("H-E-L-M") && !is_reserved("helmet"));
    }

    #[test]
    fn fails_closed_on_bad_files() {
        let r = repo();
        let prof = tempfile::tempdir().unwrap();
        let g = RepoGate::new(prof.path(), None);
        write(&r.path().join(".mcp.json"), "{not json");
        let l = g.for_launch(r.path(), true);
        assert!(!l.native);
        assert!(l.notice.unwrap().contains(".mcp.json"));
        write(
            &r.path().join(".mcp.json"),
            &" ".repeat(MCP_JSON_MAX as usize + 1),
        );
        assert!(!g.for_launch(r.path(), true).native);
        let many: serde_json::Map<String, Value> =
            (0..65).map(|i| (format!("s{i}"), json!({}))).collect();
        mcp_json(r.path(), Value::Object(many));
        assert!(!g.for_launch(r.path(), true).native);
        mcp_json(r.path(), json!({" padded": {}}));
        assert!(!g.for_launch(r.path(), true).native);
        // Off by the user's switch: no scan, nothing disabled.
        assert_eq!(
            g.for_launch(r.path(), false),
            LaunchGate {
                native: false,
                disabled: vec![],
                notice: None
            }
        );
    }

    #[test]
    fn agent_file_servers_are_adopted_on_first_sight_and_bound_to_their_block() {
        let r = repo();
        let prof = tempfile::tempdir().unwrap();
        let agent = r.path().join(".claude/agents/sub/reviewer.md");
        write(
            &agent,
            "---\nname: reviewer\nmcpServers:\n  github:\n    command: gh-mcp\n  docs:\n    url: https://d.example\ntools: Read\n---\nBody\n",
        );
        let g = RepoGate::new(prof.path(), None);
        assert!(g.for_launch(r.path(), true).disabled.is_empty());
        write(
            &agent,
            "---\nname: reviewer\nmcpServers:\n  github:\n    command: other\n  docs:\n    url: https://d.example\n---\n",
        );
        assert_eq!(g.for_launch(r.path(), true).disabled, ["github"]);
        write(&agent, "---\nmcpServers: {github: {command: x}}\n---\n");
        assert!(
            !g.for_launch(r.path(), true).native,
            "flow style fails closed"
        );
    }

    #[test]
    fn governing_dirs_stop_at_the_git_root_and_below_home() {
        let r = repo();
        let sub = r.path().join("a/b");
        std::fs::create_dir_all(&sub).unwrap();
        let dirs = governing_dirs(&sub, None);
        assert_eq!(dirs.last().unwrap(), r.path());
        assert_eq!(dirs.len(), 3);
        let dirs = governing_dirs(&sub, Some(&r.path().join("a")));
        assert_eq!(dirs, vec![sub.clone()], "never at or above home");
    }
}
