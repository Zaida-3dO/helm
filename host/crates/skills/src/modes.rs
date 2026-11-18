//! Context modes: named bundles of guidance a chat switches on and off.
//!
//! A mode is one Markdown file, `modes/<name>.md`, in a knowledge layer. Its header (the same
//! simple `key: value` lines as a rule's, with inline lists such as `[a, b]`) says what the mode is
//! for and what it adds; the text after the header says what to know and where to look. A person's
//! layer wins over the Everyone layer for a name both have; a project carries no mode files, only
//! the list of modes its chats start with.
//!
//! The second half of the file is the per-chat state: which modes a chat has on, in two tables the
//! caller's transaction reads and writes.
//!
//! A mode file is plain text the model reads, so it is capped (40 files per layer, 16 KiB each),
//! screened for invisible and direction-changing characters, and written atomically. A file that
//! fails any of that is skipped with a log line and never fails a launch.

use crate::launch::{ChatKnowledge, ChatLookup};
use crate::layers::{valid_profile_id, Layer};
use crate::rules;
use helm_core::db::{Database, MigrationRegistry};
use helm_hosttools::McpServerHide;
use rusqlite::{params, Connection, Transaction};
use serde::Serialize;
use std::collections::BTreeMap;
use std::fs;
use std::io::{self, ErrorKind};
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// A layer lists at most this many mode files.
pub const MAX_MODES_PER_LAYER: usize = 40;
/// One mode file holds at most this many bytes.
pub const MAX_MODE_FILE: u64 = 16 * 1024;
/// A mode's one-line description is at most this many characters.
const MAX_DESCRIPTION: usize = 200;
/// A header list holds at most this many entries, each at most [`MAX_ITEM`] characters.
const MAX_LIST: usize = 40;
const MAX_ITEM: usize = 100;

/// The modes folder inside a layer's root.
pub fn modes_dir(root: &Path) -> PathBuf {
    root.join("modes")
}

/// One mode as Settings, the launch and `app_help` see it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct ModeFile {
    /// The file's name without `.md`.
    pub name: String,
    /// The layer it was read from: `everyone` or `profile:<id>`.
    pub layer: String,
    pub description: String,
    /// Words that suggest the mode.
    pub triggers: Vec<String>,
    /// Host tool groups the mode adds.
    pub tools: Vec<String>,
    /// Ids or names of Helm's MCP servers that belong to the mode.
    pub mcp: Vec<String>,
    pub skills: Vec<String>,
    pub rules: Vec<String>,
    pub builtins: Vec<String>,
    pub model: Option<String>,
    /// What to know and where to look.
    pub body: String,
    /// A hash of the file's bytes, for an edit that must not overwrite a newer one.
    pub revision: String,
}

/// What a save writes.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ModeInput {
    pub description: String,
    pub triggers: Vec<String>,
    pub tools: Vec<String>,
    pub mcp: Vec<String>,
    pub skills: Vec<String>,
    pub rules: Vec<String>,
    pub builtins: Vec<String>,
    pub model: Option<String>,
    pub body: String,
}

fn invalid(why: &str) -> io::Error {
    io::Error::new(ErrorKind::InvalidInput, why.to_owned())
}

/// `a, b` or `[a, b]` as a list: trimmed, unquoted, no empty entries, no repeats.
fn parse_list(v: &str) -> Vec<String> {
    let v = v.trim();
    let inner = v
        .strip_prefix('[')
        .and_then(|x| x.strip_suffix(']'))
        .unwrap_or(v);
    let mut out: Vec<String> = Vec::new();
    for item in inner.split(',') {
        let item: String = item
            .trim()
            .trim_matches(['"', '\''])
            .trim()
            .chars()
            .take(MAX_ITEM)
            .collect();
        if !item.is_empty() && !out.contains(&item) && out.len() < MAX_LIST {
            out.push(item);
        }
    }
    out
}

/// Whether a file starts a header (`---` on its own first line) that never closes.
fn header_unclosed(text: &str) -> bool {
    let t = text.strip_prefix('\u{feff}').unwrap_or(text);
    (t.starts_with("---\n") || t.starts_with("---\r\n")) && rules::split_front(text).1 == text
}

/// A mode's header and body from a file's text; `None` for a header that never closes.
fn parse(name: &str, layer: &str, text: &str, revision: String) -> Option<ModeFile> {
    if header_unclosed(text) {
        return None;
    }
    let body = rules::split_front(text).1;
    let head = &text[..text.len() - body.len()];
    let mut m = ModeFile {
        name: name.to_owned(),
        layer: layer.to_owned(),
        body: body.trim().to_owned(),
        revision,
        ..Default::default()
    };
    for line in head.lines().skip(1) {
        let Some((k, v)) = line.split_once(':') else {
            continue;
        };
        let v = v.trim();
        match k.trim() {
            "description" => m.description = v.chars().take(MAX_DESCRIPTION).collect(),
            "triggers" => m.triggers = parse_list(v),
            "tools" => m.tools = parse_list(v),
            "mcp" => m.mcp = parse_list(v),
            "skills" => m.skills = parse_list(v),
            "rules" => m.rules = parse_list(v),
            "builtins" => m.builtins = parse_list(v),
            "model" if !v.is_empty() => m.model = Some(v.chars().take(MAX_ITEM).collect()),
            _ => {}
        }
    }
    Some(m)
}

/// The mode named by a file in the folder, or `None` (with a log line) when the file is not a
/// usable mode.
fn read_file(root: &Path, layer_key: &str, name: &str) -> Option<ModeFile> {
    let path = modes_dir(root).join(format!("{name}.md"));
    let meta = fs::symlink_metadata(&path).ok()?;
    if helm_platform::fs::is_link_like(&meta) || !meta.is_file() {
        return None;
    }
    if meta.len() > MAX_MODE_FILE {
        log::warn!("modes: {name} in {layer_key} is over the size limit and was left out");
        return None;
    }
    let bytes = helm_core::privfs::read_capped(&path, MAX_MODE_FILE).ok()?;
    let text = String::from_utf8_lossy(&bytes).into_owned();
    if helm_core::hidden::hides(&text) {
        log::warn!("modes: {name} in {layer_key} holds hidden characters and was left out");
        return None;
    }
    let mode = parse(name, layer_key, &text, helm_core::sha256_hex(&bytes));
    if mode.is_none() {
        log::warn!("modes: {name} in {layer_key} has a header that never closes and was left out");
    }
    mode
}

/// The mode names a layer's folder holds, sorted: `<name>.md` with a valid name.
fn file_names(root: &Path) -> Vec<String> {
    let Ok(read) = fs::read_dir(modes_dir(root)) else {
        return Vec::new();
    };
    let mut names: Vec<String> = read
        .flatten()
        .filter_map(|e| {
            let file = e.file_name().to_string_lossy().into_owned();
            let stem = file.strip_suffix(".md")?;
            (crate::is_valid_name(stem) && rules::file_name_of(&file).is_ok())
                .then(|| stem.to_owned())
        })
        .collect();
    names.sort();
    names
}

/// Every usable mode of one layer, by name. A missing folder means none; the first
/// [`MAX_MODES_PER_LAYER`] usable files count and the rest are left out.
pub fn list_layer(root: &Path, layer_key: &str) -> Vec<ModeFile> {
    let mut out = Vec::new();
    for name in file_names(root) {
        let Some(mode) = read_file(root, layer_key, &name) else {
            continue;
        };
        if out.len() == MAX_MODES_PER_LAYER {
            log::warn!("modes: {layer_key} has more than {MAX_MODES_PER_LAYER} modes; {name} and later ones were left out");
            break;
        }
        out.push(mode);
    }
    out
}

/// The modes a person sees: the Everyone layer's, with the person's own layer winning on a name
/// both have. Sorted by name.
pub fn merged(profile_dir: &Path, person: Option<&str>) -> Vec<ModeFile> {
    let mut by_name: BTreeMap<String, ModeFile> = BTreeMap::new();
    let everyone = Layer::Everyone;
    for m in list_layer(&everyone.root(profile_dir), &everyone.key()) {
        by_name.insert(m.name.clone(), m);
    }
    if let Some(id) = person.filter(|p| valid_profile_id(p)) {
        let own = Layer::Profile(id.to_owned());
        for m in list_layer(&own.root(profile_dir), &own.key()) {
            by_name.insert(m.name.clone(), m);
        }
    }
    by_name.into_values().collect()
}

/// One mode by name, as [`merged`] sees it.
pub fn find(profile_dir: &Path, person: Option<&str>, name: &str) -> Option<ModeFile> {
    merged(profile_dir, person)
        .into_iter()
        .find(|m| m.name == name)
}

fn check_list(what: &str, items: &[String]) -> io::Result<Vec<String>> {
    let mut out: Vec<String> = Vec::new();
    for i in items {
        let i = i.trim();
        if i.is_empty() {
            continue;
        }
        if i.chars().count() > MAX_ITEM
            || helm_core::hidden::has_hidden(i)
            || i.contains([',', '[', ']', '"', '\''])
        {
            return Err(invalid(&format!(
                "an entry in {what} is too long or holds a comma, bracket, quote or hidden character"
            )));
        }
        if !out.iter().any(|o| o == i) {
            out.push(i.to_owned());
        }
    }
    if out.len() > MAX_LIST {
        return Err(invalid(&format!("{what} holds at most {MAX_LIST} entries")));
    }
    Ok(out)
}

/// The file text for a mode, after every field is checked.
fn render(input: &ModeInput) -> io::Result<String> {
    let description = input.description.trim();
    if description.is_empty() || description.chars().count() > MAX_DESCRIPTION {
        return Err(invalid(
            "a mode's description is one line of 1 to 200 characters",
        ));
    }
    if helm_core::hidden::has_hidden(description) {
        return Err(invalid(
            "the description cannot hold line breaks, control, invisible or direction-changing characters",
        ));
    }
    let body = input.body.trim();
    if helm_core::hidden::hides(body) {
        return Err(invalid(
            "the text cannot hold invisible or direction-changing characters",
        ));
    }
    let mut head = format!("---\ndescription: {description}\n");
    for (key, items) in [
        ("triggers", &input.triggers),
        ("tools", &input.tools),
        ("mcp", &input.mcp),
        ("skills", &input.skills),
        ("rules", &input.rules),
        ("builtins", &input.builtins),
    ] {
        let items = check_list(key, items)?;
        if !items.is_empty() {
            head.push_str(&format!("{key}: [{}]\n", items.join(", ")));
        }
    }
    if let Some(model) = input
        .model
        .as_deref()
        .map(str::trim)
        .filter(|m| !m.is_empty())
    {
        if model.chars().count() > MAX_ITEM || helm_core::hidden::has_hidden(model) {
            return Err(invalid(
                "the model name is too long or holds hidden characters",
            ));
        }
        head.push_str(&format!("model: {model}\n"));
    }
    let text = format!("{head}---\n{body}\n");
    if text.len() as u64 > MAX_MODE_FILE {
        return Err(invalid("the mode is too large"));
    }
    Ok(text)
}

/// Write one mode into a layer. With no `expected_revision` the mode must be new; with one, the
/// file must exist and still have that revision, else `AlreadyExists` says it changed since it was
/// read (or, for a new mode, that the name is taken).
pub fn write(
    root: &Path,
    layer_key: &str,
    name: &str,
    input: &ModeInput,
    expected_revision: Option<&str>,
) -> io::Result<ModeFile> {
    if !crate::is_valid_name(name) || rules::file_name_of(&format!("{name}.md")).is_err() {
        return Err(invalid(
            "a mode's name is lowercase letters, digits and hyphens, starting with a letter",
        ));
    }
    if Layer::parse(layer_key).is_none() {
        return Err(invalid("the layer is `everyone` or `profile:<id>`"));
    }
    let text = render(input)?;
    let existing = fs::symlink_metadata(modes_dir(root).join(format!("{name}.md"))).is_ok();
    match (existing, expected_revision) {
        (true, None) => {
            return Err(io::Error::new(
                ErrorKind::AlreadyExists,
                "a mode of that name already exists",
            ))
        }
        (false, Some(_)) => {
            return Err(io::Error::new(
                ErrorKind::NotFound,
                "no mode there to replace",
            ))
        }
        (true, Some(rev)) => {
            let now = read_file(root, layer_key, name)
                .ok_or_else(|| invalid("the mode file there cannot be read"))?;
            if now.revision != rev {
                return Err(io::Error::new(
                    ErrorKind::AlreadyExists,
                    "the mode changed since it was read",
                ));
            }
        }
        (false, None) => {
            if file_names(root).len() >= MAX_MODES_PER_LAYER {
                return Err(invalid("a layer holds at most 40 modes"));
            }
        }
    }
    helm_core::privfs::create_private_dir_all(&modes_dir(root))?;
    helm_core::privfs::write_atomic(&modes_dir(root).join(format!("{name}.md")), text.as_bytes())?;
    read_file(root, layer_key, name)
        .ok_or_else(|| io::Error::other("the mode was written but could not be read back"))
}

/// Remove one mode. Only a plain file in the folder can go, never through a link.
pub fn remove(root: &Path, name: &str) -> io::Result<()> {
    if !crate::is_valid_name(name) {
        return Err(invalid("that is not a mode name"));
    }
    let full = modes_dir(root).join(format!("{name}.md"));
    match fs::symlink_metadata(&full) {
        Err(e) if e.kind() == ErrorKind::NotFound => {
            Err(io::Error::new(ErrorKind::NotFound, "no such mode"))
        }
        Err(e) => Err(e),
        Ok(m) if helm_platform::fs::is_link_like(&m) || !m.is_file() => {
            Err(invalid("not a mode file"))
        }
        Ok(_) => fs::remove_file(&full),
    }
}

// ---------------------------------------------------------------------------------------------
// What a chat has on.

/// One row of a chat's modes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChatMode {
    pub name: String,
    /// The layer the mode was read from when it went on.
    pub layer: String,
    pub since: i64,
    /// Who switched it on: `project`, `user`, `agent` or `carried`.
    pub source: String,
    /// When it was switched off; `None` while it is on.
    pub removed_at: Option<i64>,
    /// Whether the model has been told about the latest change.
    pub announced: bool,
}

/// The two tables (an initializer; idempotent).
pub fn init(tx: &Transaction<'_>) -> rusqlite::Result<()> {
    tx.execute_batch(
        "CREATE TABLE IF NOT EXISTS chat_modes (
             chat_id    TEXT NOT NULL REFERENCES chats(id) ON DELETE CASCADE,
             mode       TEXT NOT NULL,
             layer      TEXT NOT NULL,
             since      INTEGER NOT NULL,
             source     TEXT NOT NULL,
             removed_at INTEGER,
             announced  INTEGER NOT NULL DEFAULT 0,
             PRIMARY KEY (chat_id, mode)
         );
         CREATE TABLE IF NOT EXISTS chat_modes_state (
             chat_id   TEXT PRIMARY KEY REFERENCES chats(id) ON DELETE CASCADE,
             seeded_at INTEGER NOT NULL
         );",
    )
}

pub fn register_migrations(reg: &mut MigrationRegistry) {
    reg.initializer("skills-chat-modes", init);
}

fn rows(conn: &Connection, chat_id: &str, only_active: bool) -> rusqlite::Result<Vec<ChatMode>> {
    let mut st = conn.prepare(&format!(
        "SELECT mode, layer, since, source, removed_at, announced FROM chat_modes
         WHERE chat_id = ?1 {} ORDER BY since, mode",
        if only_active {
            "AND removed_at IS NULL"
        } else {
            ""
        }
    ))?;
    let out = st
        .query_map([chat_id], |r| {
            Ok(ChatMode {
                name: r.get(0)?,
                layer: r.get(1)?,
                since: r.get(2)?,
                source: r.get(3)?,
                removed_at: r.get(4)?,
                announced: r.get::<_, i64>(5)? != 0,
            })
        })?
        .collect();
    out
}

/// The modes the chat has on, oldest first.
pub fn active(tx: &Connection, chat_id: &str) -> rusqlite::Result<Vec<ChatMode>> {
    rows(tx, chat_id, true)
}

/// Every row of the chat, switched-off ones included.
pub fn all_rows(tx: &Connection, chat_id: &str) -> rusqlite::Result<Vec<ChatMode>> {
    rows(tx, chat_id, false)
}

/// Switch a mode on. True when it was not on before (a switched-off row is brought back).
pub fn add(
    tx: &Connection,
    chat_id: &str,
    name: &str,
    layer: &str,
    source: &str,
    now: i64,
    announced: bool,
) -> rusqlite::Result<bool> {
    let was_on = tx
        .query_row(
            "SELECT removed_at IS NULL FROM chat_modes WHERE chat_id = ?1 AND mode = ?2",
            params![chat_id, name],
            |r| r.get::<_, bool>(0),
        )
        .ok()
        .unwrap_or(false);
    if was_on {
        return Ok(false);
    }
    tx.execute(
        "INSERT INTO chat_modes (chat_id, mode, layer, since, source, removed_at, announced)
         VALUES (?1, ?2, ?3, ?4, ?5, NULL, ?6)
         ON CONFLICT (chat_id, mode) DO UPDATE SET
             layer = ?3, since = ?4, source = ?5, removed_at = NULL, announced = ?6",
        params![chat_id, name, layer, now, source, announced],
    )?;
    Ok(true)
}

/// Switch a mode off (the row stays, marked). True when it was on.
pub fn remove_row(
    tx: &Connection,
    chat_id: &str,
    name: &str,
    now: i64,
    announced: bool,
) -> rusqlite::Result<bool> {
    Ok(tx.execute(
        "UPDATE chat_modes SET removed_at = ?3, announced = ?4
         WHERE chat_id = ?1 AND mode = ?2 AND removed_at IS NULL",
        params![chat_id, name, now, announced],
    )? > 0)
}

/// Whether the chat's modes were set from its project's defaults already.
pub fn is_seeded(tx: &Connection, chat_id: &str) -> rusqlite::Result<bool> {
    tx.query_row(
        "SELECT EXISTS (SELECT 1 FROM chat_modes_state WHERE chat_id = ?1)",
        [chat_id],
        |r| r.get(0),
    )
}

pub fn mark_seeded(tx: &Connection, chat_id: &str, now: i64) -> rusqlite::Result<()> {
    tx.execute(
        "INSERT OR IGNORE INTO chat_modes_state (chat_id, seeded_at) VALUES (?1, ?2)",
        params![chat_id, now],
    )?;
    Ok(())
}

/// Put `from`'s active modes on `to` (told to the model already) and count `to` as seeded.
pub fn copy_active(
    tx: &Connection,
    from: &str,
    to: &str,
    source: &str,
    now: i64,
) -> rusqlite::Result<usize> {
    let n = tx.execute(
        "INSERT OR IGNORE INTO chat_modes (chat_id, mode, layer, since, source, removed_at, announced)
         SELECT ?2, mode, layer, since, ?3, NULL, 1 FROM chat_modes
         WHERE chat_id = ?1 AND removed_at IS NULL",
        params![from, to, source],
    )?;
    mark_seeded(tx, to, now)?;
    Ok(n)
}

/// What changed since the model was last told: the modes switched on (`true`), then the ones
/// switched off (`false`).
pub fn unannounced(tx: &Connection, chat_id: &str) -> rusqlite::Result<Vec<(String, bool)>> {
    let mut out: Vec<(String, bool)> = Vec::new();
    for (sql, on) in [
        (
            "SELECT mode FROM chat_modes
             WHERE chat_id = ?1 AND announced = 0 AND removed_at IS NULL ORDER BY since, mode",
            true,
        ),
        (
            "SELECT mode FROM chat_modes
             WHERE chat_id = ?1 AND announced = 0 AND removed_at IS NOT NULL
             ORDER BY removed_at, mode",
            false,
        ),
    ] {
        let mut st = tx.prepare(sql)?;
        let names = st
            .query_map([chat_id], |r| r.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        out.extend(names.into_iter().map(|n| (n, on)));
    }
    Ok(out)
}

pub fn mark_announced(tx: &Connection, chat_id: &str) -> rusqlite::Result<()> {
    tx.execute(
        "UPDATE chat_modes SET announced = 1 WHERE chat_id = ?1 AND announced = 0",
        [chat_id],
    )?;
    Ok(())
}

// ---------------------------------------------------------------------------------------------
// MCP servers that belong to a mode.

/// Hides the MCP servers a mode names from the chats that do not have that mode on. A server no
/// mode names is never hidden.
pub struct ModeServerHide {
    db: Arc<Database>,
    profile_dir: PathBuf,
    lookup: Arc<dyn ChatLookup>,
}

impl ModeServerHide {
    pub fn new(db: Arc<Database>, profile_dir: PathBuf, lookup: Arc<dyn ChatLookup>) -> Self {
        Self {
            db,
            profile_dir,
            lookup,
        }
    }
}

/// The servers hidden from a chat whose person sees `modes` and who has `on` switched on.
pub fn hidden_servers(modes: &[ModeFile], on: &[String]) -> Vec<String> {
    let kept: Vec<String> = modes
        .iter()
        .filter(|m| on.contains(&m.name))
        .flat_map(|m| m.mcp.iter().map(|s| s.to_lowercase()))
        .collect();
    let mut out: Vec<String> = Vec::new();
    for s in modes.iter().flat_map(|m| m.mcp.iter()) {
        if !kept.contains(&s.to_lowercase()) && !out.iter().any(|o| o.eq_ignore_ascii_case(s)) {
            out.push(s.clone());
        }
    }
    out
}

impl McpServerHide for ModeServerHide {
    fn hidden(&self, chat_id: &str) -> Vec<String> {
        let who: ChatKnowledge = self.lookup.of_chat(chat_id);
        let person = who.profile.as_ref().map(|p| p.id.as_str());
        let modes = merged(&self.profile_dir, person);
        if modes.iter().all(|m| m.mcp.is_empty()) {
            return Vec::new();
        }
        let on: Vec<String> = self
            .db
            .snapshot(|tx| active(tx, chat_id))
            .map(|rows| rows.into_iter().map(|r| r.name).collect())
            .unwrap_or_default();
        hidden_servers(&modes, &on)
    }

    fn any(&self) -> bool {
        let people = self.profile_dir.join(crate::layers::FOLDER).join("people");
        let mut roots = vec![Layer::Everyone.root(&self.profile_dir)];
        roots.extend(
            fs::read_dir(people)
                .into_iter()
                .flatten()
                .flatten()
                .map(|e| e.path()),
        );
        roots
            .iter()
            .any(|r| list_layer(r, "").iter().any(|m| !m.mcp.is_empty()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::launch::ProfileRef;

    fn put(root: &Path, file: &str, text: &str) {
        let dir = modes_dir(root);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join(file), text).unwrap();
    }

    fn names(modes: &[ModeFile]) -> Vec<String> {
        modes
            .iter()
            .map(|m| format!("{}@{}", m.name, m.layer))
            .collect()
    }

    /// Layers merge by name with the person winning; bad files and names are skipped; unknown group
    /// and server names are kept as written; the caps and hidden characters hold.
    #[test]
    fn modes_merge_across_layers_and_bad_files_are_skipped() {
        let tmp = tempfile::tempdir().unwrap();
        let every = Layer::Everyone.root(tmp.path());
        let mine = Layer::Profile("sam".into()).root(tmp.path());
        put(
            &every,
            "writing.md",
            "---\ndescription: Drafting prose\ntriggers: [essay, \"draft\"]\ntools: [chats]\nmcp: [Notes, wiki]\nskills: unknown-skill\nrules: [tone]\nbuiltins: [web]\nmodel: fast-one\n---\n# Writing\nRead the style notes first.\n",
        );
        put(
            &every,
            "garden.md",
            "---\ndescription: Plants\n---\nWater early.\n",
        );
        put(
            &mine,
            "garden.md",
            "---\ndescription: My plants\n---\nWater late.\n",
        );
        put(&mine, "travel.md", "no header at all\n");
        // Skipped: header never closes, hidden character, too large, bad name, not markdown.
        put(&every, "broken.md", "---\ndescription: oops\nbody\n");
        put(
            &every,
            "sneaky.md",
            "---\ndescription: x\n---\nhide\u{202e}this\n",
        );
        put(
            &every,
            "huge.md",
            &format!("---\ndescription: x\n---\n{}", "a".repeat(17 * 1024)),
        );
        put(&every, "Bad_Name.md", "---\ndescription: x\n---\nx\n");
        put(&every, "notes.txt", "x");

        let all = merged(tmp.path(), Some("sam"));
        assert_eq!(
            names(&all),
            [
                "garden@profile:sam",
                "travel@profile:sam",
                "writing@everyone"
            ]
        );
        assert_eq!(all[0].description, "My plants");
        assert_eq!(all[1].description, "", "a file with no header is all body");
        assert_eq!(all[1].body, "no header at all");
        let w = &all[2];
        assert_eq!(w.description, "Drafting prose");
        assert_eq!(w.triggers, ["essay", "draft"]);
        assert_eq!(
            (w.tools.as_slice(), w.mcp.as_slice()),
            (
                &["chats".to_owned()][..],
                &["Notes".to_owned(), "wiki".to_owned()][..]
            )
        );
        assert_eq!(
            (
                w.skills.as_slice(),
                w.rules.as_slice(),
                w.builtins.as_slice()
            ),
            (
                &["unknown-skill".to_owned()][..],
                &["tone".to_owned()][..],
                &["web".to_owned()][..]
            )
        );
        assert_eq!(w.model.as_deref(), Some("fast-one"));
        assert_eq!(w.body, "# Writing\nRead the style notes first.");
        assert!(!w.revision.is_empty());
        // Without a person, or for another person, only the Everyone layer shows.
        assert_eq!(
            names(&merged(tmp.path(), None)),
            ["garden@everyone", "writing@everyone"]
        );
        assert_eq!(
            names(&merged(tmp.path(), Some("../x"))),
            names(&merged(tmp.path(), None))
        );
        assert_eq!(
            find(tmp.path(), Some("sam"), "garden").unwrap().body,
            "Water late."
        );
        assert!(find(tmp.path(), None, "travel").is_none());

        // At most 40 files count in a layer: the first 40 by name.
        let big = Layer::Profile("big".into()).root(tmp.path());
        for n in 0..45 {
            put(
                &big,
                &format!("m{n:02}.md"),
                "---\ndescription: d\n---\nb\n",
            );
        }
        let listed = list_layer(&big, "profile:big");
        assert_eq!(listed.len(), MAX_MODES_PER_LAYER);
        assert_eq!(listed.last().unwrap().name, "m39");
    }

    /// A save writes a file that reads back the same, refuses a taken name, a stale revision and
    /// bad fields, and a removal takes the file away.
    #[test]
    fn saving_and_removing_modes_is_checked() {
        let tmp = tempfile::tempdir().unwrap();
        let root = Layer::Everyone.root(tmp.path());
        let input = ModeInput {
            description: "Plants".into(),
            triggers: vec!["soil".into(), " ".into(), "soil".into()],
            mcp: vec!["Notes".into()],
            model: Some("m".into()),
            body: "  Water early.  ".into(),
            ..Default::default()
        };
        let saved = write(&root, "everyone", "garden", &input, None).unwrap();
        assert_eq!(saved.triggers, ["soil"]);
        assert_eq!(
            (saved.body.as_str(), saved.layer.as_str()),
            ("Water early.", "everyone")
        );
        let kind = |r: io::Result<ModeFile>| r.unwrap_err().kind();
        assert_eq!(
            kind(write(&root, "everyone", "garden", &input, None)),
            ErrorKind::AlreadyExists
        );
        assert_eq!(
            kind(write(&root, "everyone", "garden", &input, Some("stale"))),
            ErrorKind::AlreadyExists
        );
        assert_eq!(
            kind(write(&root, "everyone", "other", &input, Some("x"))),
            ErrorKind::NotFound
        );
        for (name, layer, bad) in [
            ("Garden", "everyone", input.clone()),
            ("con", "everyone", input.clone()),
            ("ok", "nowhere", input.clone()),
            (
                "ok",
                "everyone",
                ModeInput {
                    description: String::new(),
                    ..input.clone()
                },
            ),
            (
                "ok",
                "everyone",
                ModeInput {
                    description: "two\nlines".into(),
                    ..input.clone()
                },
            ),
            (
                "ok",
                "everyone",
                ModeInput {
                    mcp: vec!["a,b".into()],
                    ..input.clone()
                },
            ),
            (
                "ok",
                "everyone",
                ModeInput {
                    body: "x\u{200b}y".into(),
                    ..input.clone()
                },
            ),
            (
                "ok",
                "everyone",
                ModeInput {
                    body: "x".repeat(17 * 1024),
                    ..input.clone()
                },
            ),
        ] {
            assert_eq!(
                kind(write(&root, layer, name, &bad, None)),
                ErrorKind::InvalidInput,
                "{name} {layer}"
            );
        }
        let again = ModeInput {
            body: "Water late.".into(),
            ..input.clone()
        };
        let updated = write(&root, "everyone", "garden", &again, Some(&saved.revision)).unwrap();
        assert_eq!(updated.body, "Water late.");
        remove(&root, "garden").unwrap();
        assert_eq!(
            remove(&root, "garden").unwrap_err().kind(),
            ErrorKind::NotFound
        );
        assert_eq!(
            remove(&root, "../x").unwrap_err().kind(),
            ErrorKind::InvalidInput
        );
        assert!(list_layer(&root, "everyone").is_empty());
    }

    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "CREATE TABLE chats (id TEXT PRIMARY KEY); INSERT INTO chats VALUES ('a'), ('b');",
        )
        .unwrap();
        let tx = c.unchecked_transaction().unwrap();
        init(&tx).unwrap();
        init(&tx).unwrap();
        tx.commit().unwrap();
        c
    }

    /// The per-chat rows: switching on and off, announcing, seeding and carrying to another chat.
    #[test]
    fn a_chats_modes_switch_on_and_off_and_carry_over() {
        let c = db();
        assert!(!is_seeded(&c, "a").unwrap());
        assert!(add(&c, "a", "writing", "everyone", "project", 10, false).unwrap());
        assert!(
            !add(&c, "a", "writing", "everyone", "user", 11, true).unwrap(),
            "already on"
        );
        assert!(add(&c, "a", "garden", "profile:sam", "user", 12, true).unwrap());
        mark_seeded(&c, "a", 10).unwrap();
        mark_seeded(&c, "a", 99).unwrap();
        assert!(is_seeded(&c, "a").unwrap());
        let on = active(&c, "a").unwrap();
        assert_eq!(
            on.iter().map(|m| m.name.as_str()).collect::<Vec<_>>(),
            ["writing", "garden"]
        );
        assert_eq!(
            (on[0].source.as_str(), on[0].since, on[0].announced),
            ("project", 10, false)
        );
        assert_eq!(
            unannounced(&c, "a").unwrap(),
            [("writing".to_owned(), true)]
        );

        assert!(remove_row(&c, "a", "garden", 20, false).unwrap());
        assert!(
            !remove_row(&c, "a", "garden", 21, false).unwrap(),
            "already off"
        );
        assert!(!remove_row(&c, "a", "nothing", 21, false).unwrap());
        assert_eq!(active(&c, "a").unwrap().len(), 1);
        let every = all_rows(&c, "a").unwrap();
        assert_eq!((every.len(), every[1].removed_at), (2, Some(20)));
        assert_eq!(
            unannounced(&c, "a").unwrap(),
            [("writing".to_owned(), true), ("garden".to_owned(), false)]
        );
        mark_announced(&c, "a").unwrap();
        assert!(unannounced(&c, "a").unwrap().is_empty());

        // A switched-off mode comes back as new.
        assert!(add(&c, "a", "garden", "everyone", "agent", 30, false).unwrap());
        assert_eq!(active(&c, "a").unwrap()[1].source, "agent");

        // Only the active modes carry, told already, and the new chat counts as seeded.
        assert!(remove_row(&c, "a", "garden", 31, true).unwrap());
        assert_eq!(copy_active(&c, "a", "b", "carried", 40).unwrap(), 1);
        assert!(is_seeded(&c, "b").unwrap());
        let carried = active(&c, "b").unwrap();
        assert_eq!(carried.len(), 1);
        assert_eq!(
            (
                carried[0].name.as_str(),
                carried[0].source.as_str(),
                carried[0].announced
            ),
            ("writing", "carried", true)
        );
        // Deleting a chat takes its rows (foreign keys are on in the app's connections).
        c.pragma_update(None, "foreign_keys", true).unwrap();
        c.execute("DELETE FROM chats WHERE id = 'b'", []).unwrap();
        assert!(active(&c, "b").unwrap().is_empty() && !is_seeded(&c, "b").unwrap());
    }

    struct Fixed(Option<ProfileRef>);
    impl ChatLookup for Fixed {
        fn of_chat(&self, _: &str) -> ChatKnowledge {
            ChatKnowledge {
                profile: self.0.clone(),
                ..Default::default()
            }
        }
    }

    /// A server a mode names is hidden unless the chat has a mode that names it on; a server no
    /// mode names stays; the person's layer decides which files count.
    #[test]
    fn a_mode_hides_its_servers_until_it_is_on() {
        let tmp = tempfile::tempdir().unwrap();
        let every = Layer::Everyone.root(tmp.path());
        let mine = Layer::Profile("sam".into()).root(tmp.path());
        put(
            &every,
            "writing.md",
            "---\ndescription: w\nmcp: [Notes, wiki]\n---\nb\n",
        );
        put(
            &every,
            "garden.md",
            "---\ndescription: g\nmcp: [notes]\n---\nb\n",
        );
        put(
            &mine,
            "travel.md",
            "---\ndescription: t\nmcp: [Maps]\n---\nb\n",
        );
        let modes = merged(tmp.path(), Some("sam"));
        let sorted = |mut v: Vec<String>| {
            v.sort_by_key(|s| s.to_lowercase());
            v
        };
        assert_eq!(
            sorted(hidden_servers(&modes, &[])),
            ["Maps", "Notes", "wiki"]
        );
        assert_eq!(
            sorted(hidden_servers(&modes, &["garden".into()])),
            ["Maps", "wiki"]
        );
        assert_eq!(
            hidden_servers(&modes, &["writing".into(), "travel".into()]),
            Vec::<String>::new()
        );

        let d = tempfile::tempdir().unwrap();
        let database = Arc::new(
            Database::open(&d.path().join("t.db"), &{
                let mut r = MigrationRegistry::new();
                register_migrations(&mut r);
                r
            })
            .unwrap(),
        );
        database
            .write(|tx| {
                tx.execute("INSERT INTO projects (id, name, created_at) VALUES ('p', 'P', 1)", [])?;
                tx.execute(
                    "INSERT INTO chats (id, project_id, created_at, updated_at) VALUES ('c', 'p', 1, 1)",
                    [],
                )?;
                add(tx, "c", "writing", "everyone", "user", 1, true)
            })
            .unwrap();
        let sam = Some(ProfileRef {
            id: "sam".into(),
            name: "Sam".into(),
            is_home: true,
        });
        let hide = ModeServerHide::new(
            database.clone(),
            tmp.path().to_owned(),
            Arc::new(Fixed(sam)),
        );
        assert_eq!(sorted(hide.hidden("c")), ["Maps"]);
        assert!(hide.any());
        let other = ModeServerHide::new(database, tmp.path().to_owned(), Arc::new(Fixed(None)));
        assert_eq!(
            sorted(other.hidden("c")),
            Vec::<String>::new(),
            "the chat's writing mode names both"
        );
        let none = ModeServerHide::new(
            Arc::new(Database::open(&d.path().join("u.db"), &MigrationRegistry::new()).unwrap()),
            d.path().to_owned(),
            Arc::new(Fixed(None)),
        );
        assert!(!none.any() && none.hidden("c").is_empty());
    }
}
