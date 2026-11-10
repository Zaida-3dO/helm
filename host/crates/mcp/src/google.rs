//! Several Google accounts through the open-source `workspace-mcp` server.
//!
//! Helm never holds a Google password or token. The server runs as a program on this computer,
//! one process for every account, with the user's own Google sign-in client. It keeps each
//! account's sign-in as `<address>.json` in a folder inside the profile; Helm only reads that
//! folder's file names (never their contents) to list the accounts, and deletes a file when the
//! user removes an account.
//!
//! | op | class | args → result |
//! |---|---|---|
//! | `mcp_google_accounts` | blocking | `{serverId}` → [`GoogleAccountList`] |
//! | `mcp_google_add` | blocking | `{serverId, email}` → [`GoogleSignIn`] (starts the sign-in; the browser opens) |
//! | `mcp_google_remove` | maintenance | `{serverId, email}` → [`GoogleAccountList`] |
//! | `mcp_google_label` | maintenance | `{serverId, email, label}` → [`GoogleAccountList`] |
//! | `mcp_google_permission` | maintenance | `{serverId, permission}` → [`McpEditorSnapshot`] |
//!
//! The sign-in client's id and secret are ordinary literals on the server (kept in the credential
//! store). The credentials folder and the callback port are not stored on the server: they are
//! added when the server's references are resolved, from this module, so a moved profile keeps
//! working. The port is chosen once and kept in a small settings file beside the folder, which
//! also holds the optional label for each address.

use crate::config::McpStore;
use crate::types::{McpEditorSnapshot, ResolvedServer, TransportKind};
use helm_core::privfs;
use helm_core::profile::Profile;
use helm_ipc::{ApiError, OpClass, OpCtx, OpRegistry};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};
use ts_rs::TS;

/// The preset id, which is also the default id of a server made from it.
pub const PRESET_ID: &str = "google_accounts";
pub const CREDENTIALS_ENV: &str = "WORKSPACE_MCP_CREDENTIALS_DIR";
pub const PORT_ENV: &str = "WORKSPACE_MCP_PORT";
pub const CLIENT_ID_ENV: &str = "GOOGLE_OAUTH_CLIENT_ID";
pub const CLIENT_SECRET_ENV: &str = "GOOGLE_OAUTH_CLIENT_SECRET";
/// The arguments a new server starts with; the last one is the permission level. The server
/// refuses `--tools` (and `--read-only`) beside `--permissions`, and `--permissions` alone loads
/// only the services it names.
pub const DEFAULT_ARGS: [&str; 3] = ["workspace-mcp", "--permissions", "gmail:drafts"];

/// At most this many accounts are listed.
pub const MAX_ACCOUNTS: usize = 50;
/// At most this many directory entries are looked at.
const MAX_SCAN: usize = 500;
const MAX_LABEL_CHARS: usize = 32;
const MAX_SETTINGS_BYTES: u64 = 64 * 1024;
/// How long a started sign-in is watched for.
const SIGN_IN_WINDOW: Duration = Duration::from_secs(180);
/// The first start of the server can download it, so the wait is long.
const STARTUP: Duration = Duration::from_secs(120);
const AUTH_PREFIX: &str = "https://accounts.google.com/";

/// What an account may do with mail. Each level includes the ones before it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum GooglePermission {
    Readonly,
    Drafts,
    Send,
}

impl GooglePermission {
    /// The `--permissions` value for Gmail.
    pub fn flag(self) -> &'static str {
        match self {
            GooglePermission::Readonly => "gmail:readonly",
            GooglePermission::Drafts => "gmail:drafts",
            GooglePermission::Send => "gmail:send",
        }
    }

    fn from_flag(flag: &str) -> Option<Self> {
        [Self::Readonly, Self::Drafts, Self::Send]
            .into_iter()
            .find(|p| p.flag() == flag)
    }
}

/// One signed-in address and its optional label.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct GoogleAccount {
    pub email: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub label: Option<String>,
}

/// What the Accounts block shows.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct GoogleAccountList {
    pub accounts: Vec<GoogleAccount>,
    /// Absent when the arguments carry a level the picker does not offer.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub permission: Option<GooglePermission>,
    /// `uvx` was found on `PATH`.
    pub uv_found: bool,
    /// A sign-in Helm started is still open.
    pub signing_in: bool,
}

/// The answer to starting a sign-in.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct GoogleSignIn {
    /// Google's sign-in address, for when the browser did not open by itself.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub auth_url: Option<String>,
}

// ---- where things live ----------------------------------------------------------------------

/// The folder the server keeps its sign-ins in.
pub fn credentials_dir(profile_dir: &Path) -> PathBuf {
    profile_dir.join("mcp").join("google")
}

fn settings_path(profile_dir: &Path) -> PathBuf {
    profile_dir.join("mcp").join("google-settings.json")
}

#[derive(Default, Serialize, Deserialize)]
struct Settings {
    #[serde(default)]
    port: u16,
    /// Lower-cased address → label.
    #[serde(default)]
    labels: BTreeMap<String, String>,
}

static SETTINGS_LOCK: Mutex<()> = Mutex::new(());

fn read_settings(profile_dir: &Path) -> Settings {
    privfs::read_capped(&settings_path(profile_dir), MAX_SETTINGS_BYTES + 1)
        .ok()
        .filter(|b| b.len() as u64 <= MAX_SETTINGS_BYTES)
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

fn write_settings(profile_dir: &Path, s: &Settings) -> Result<(), String> {
    let path = settings_path(profile_dir);
    if let Some(dir) = path.parent() {
        privfs::create_private_dir_all(dir).map_err(|e| e.to_string())?;
    }
    let bytes = serde_json::to_vec_pretty(s).map_err(|e| e.to_string())?;
    privfs::write_atomic(&path, &bytes).map_err(|e| e.to_string())
}

fn free_port() -> Option<u16> {
    let l = std::net::TcpListener::bind(("127.0.0.1", 0)).ok()?;
    Some(l.local_addr().ok()?.port())
}

/// The callback port: chosen once, then kept.
fn saved_port(profile_dir: &Path) -> u16 {
    let _g = SETTINGS_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let mut s = read_settings(profile_dir);
    if s.port >= 1024 {
        return s.port;
    }
    s.port = free_port().unwrap_or(8000);
    let _ = write_settings(profile_dir, &s);
    s.port
}

// ---- addresses and the account list ---------------------------------------------------------

/// A plain address: letters, digits and `._%+-` around one `@`, so it is always a safe file name.
pub fn valid_email(s: &str) -> bool {
    if s.len() < 3 || s.len() > 254 {
        return false;
    }
    let Some((local, domain)) = s.split_once('@') else {
        return false;
    };
    s.bytes()
        .all(|b| b.is_ascii_alphanumeric() || b"@._%+-".contains(&b))
        && !local.is_empty()
        && local.len() <= 64
        && !local.starts_with('.')
        && !domain.contains('@')
        && domain.contains('.')
        && !domain.starts_with(['.', '-'])
        && !domain.ends_with(['.', '-'])
        && !domain.contains("..")
}

fn clean_email(raw: &str) -> Result<String, String> {
    let e = raw.trim();
    if valid_email(e) {
        Ok(e.to_owned())
    } else {
        Err("That does not look like an email address.".into())
    }
}

/// The addresses that have a sign-in in `dir`, sorted, at most [`MAX_ACCOUNTS`]. Only file names
/// are read: a file's contents are never opened.
pub fn list_accounts(dir: &Path) -> Vec<String> {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut out: Vec<String> = rd
        .take(MAX_SCAN)
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_file()))
        .filter_map(|e| e.file_name().into_string().ok())
        .filter_map(|name| {
            let stem = name.strip_suffix(".json")?;
            valid_email(stem).then(|| stem.to_owned())
        })
        .collect();
    out.sort_by_key(|e| (e.to_ascii_lowercase(), e.clone()));
    out.dedup_by(|a, b| a.eq_ignore_ascii_case(b));
    out.truncate(MAX_ACCOUNTS);
    out
}

fn has_account(dir: &Path, email: &str) -> bool {
    list_accounts(dir)
        .iter()
        .any(|e| e.eq_ignore_ascii_case(email))
}

fn accounts_with_labels(profile_dir: &Path) -> Vec<GoogleAccount> {
    let labels = read_settings(profile_dir).labels;
    list_accounts(&credentials_dir(profile_dir))
        .into_iter()
        .map(|email| GoogleAccount {
            label: labels.get(&email.to_ascii_lowercase()).cloned(),
            email,
        })
        .collect()
}

/// A label is short plain text: it goes into the guidance the agent reads.
pub fn clean_label(raw: &str) -> Result<Option<String>, String> {
    let l = raw.trim();
    if l.is_empty() {
        return Ok(None);
    }
    if l.chars().count() > MAX_LABEL_CHARS
        || !l
            .chars()
            .all(|c| c.is_alphanumeric() || matches!(c, ' ' | '-' | '_'))
    {
        return Err(format!(
            "A label is at most {MAX_LABEL_CHARS} letters, digits, spaces, dashes or underscores."
        ));
    }
    Ok(Some(l.to_owned()))
}

fn set_label(profile_dir: &Path, email: &str, label: &str) -> Result<(), String> {
    let label = clean_label(label)?;
    if !has_account(&credentials_dir(profile_dir), email) {
        return Err("That account is not signed in.".into());
    }
    let _g = SETTINGS_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let mut s = read_settings(profile_dir);
    let key = email.to_ascii_lowercase();
    match label {
        Some(l) => {
            s.labels.insert(key, l);
        }
        None => {
            s.labels.remove(&key);
        }
    }
    write_settings(profile_dir, &s)
}

/// Delete one address's sign-in. The name deleted is the listed one, never the typed text.
fn remove_account(profile_dir: &Path, email: &str) -> Result<(), String> {
    let dir = credentials_dir(profile_dir);
    let Some(found) = list_accounts(&dir)
        .into_iter()
        .find(|e| e.eq_ignore_ascii_case(email))
    else {
        return Err("That account is not signed in.".into());
    };
    std::fs::remove_file(dir.join(format!("{found}.json")))
        .map_err(|_| "The sign-in could not be removed.".to_owned())?;
    let _g = SETTINGS_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let mut s = read_settings(profile_dir);
    if s.labels.remove(&found.to_ascii_lowercase()).is_some() {
        let _ = write_settings(profile_dir, &s);
    }
    Ok(())
}

/// `Accounts: work = a@x, b@y`, for the agent; none when nobody is signed in.
fn accounts_line(profile_dir: &Path) -> Option<String> {
    let accounts = accounts_with_labels(profile_dir);
    if accounts.is_empty() {
        return None;
    }
    let parts: Vec<String> = accounts
        .into_iter()
        .map(|a| match a.label {
            Some(l) => format!("{l} = {}", a.email),
            None => a.email,
        })
        .collect();
    Some(format!("Accounts: {}", parts.join(", ")))
}

/// Add what only the host knows to a resolved Google server: its folder and port (the user can
/// still override them with their own environment entries), and the account list for the agent.
pub fn prepare(profile_dir: &Path, out: &mut ResolvedServer) {
    let dir = credentials_dir(profile_dir);
    let _ = privfs::create_private_dir_all(&dir);
    out.env
        .entry(CREDENTIALS_ENV.into())
        .or_insert_with(|| dir.to_string_lossy().into_owned());
    out.env
        .entry(PORT_ENV.into())
        .or_insert_with(|| saved_port(profile_dir).to_string());
    // The server reads a plain `PORT` before its own variable; one left in the user's
    // environment would move the sign-in page's port, so it is set to the same port.
    if let Some(port) = out.env.get(PORT_ENV).cloned() {
        out.env.insert("PORT".into(), port);
    }
    // The sign-in page is plain http on this computer.
    out.env
        .entry("OAUTHLIB_INSECURE_TRANSPORT".into())
        .or_insert_with(|| "1".into());
    // A server saved before `--permissions` replaced `--tools` would not start with both.
    out.args = without_tool_flags(&out.args);
    if let Some(line) = accounts_line(profile_dir) {
        out.guidance = Some(match out.guidance.take() {
            Some(g) if !g.is_empty() => format!("{g} {line}"),
            _ => line,
        });
    }
}

// ---- the permission level -------------------------------------------------------------------

/// The slice of `args` that holds the `--permissions` values, if the flag is there.
fn permission_span(args: &[String]) -> Option<std::ops::Range<usize>> {
    let i = args.iter().position(|a| a == "--permissions")?;
    let end = args[i + 1..]
        .iter()
        .position(|a| a.starts_with("--"))
        .map_or(args.len(), |n| i + 1 + n);
    Some(i + 1..end)
}

/// The Gmail level in `args`, when it is one the picker offers.
pub fn permission_of(args: &[String]) -> Option<GooglePermission> {
    if let Some(span) = permission_span(args) {
        let found = args[span].iter().find(|a| a.starts_with("gmail:"))?;
        return GooglePermission::from_flag(found);
    }
    let joined = args.iter().find_map(|a| a.strip_prefix("--permissions="))?;
    GooglePermission::from_flag(joined)
}

/// `args` without `--tools` (and its values) and `--read-only` when `--permissions` is there:
/// the server will not start with both.
pub fn without_tool_flags(args: &[String]) -> Vec<String> {
    let has_permissions = args
        .iter()
        .any(|a| a == "--permissions" || a.starts_with("--permissions="));
    if !has_permissions {
        return args.to_vec();
    }
    let mut out = Vec::with_capacity(args.len());
    let mut in_tools = false;
    for a in args {
        if a.starts_with("--") {
            in_tools = a == "--tools";
            if in_tools || a.starts_with("--tools=") || a == "--read-only" {
                continue;
            }
        } else if in_tools {
            continue;
        }
        out.push(a.clone());
    }
    out
}

/// `args` with the Gmail level set, keeping everything else but the flags the server refuses
/// beside it ([`without_tool_flags`]).
pub fn with_permission(args: &[String], level: GooglePermission) -> Vec<String> {
    let want = level.flag().to_owned();
    let mut out = args.to_vec();
    if let Some(span) = permission_span(args) {
        match span.clone().find(|&k| args[k].starts_with("gmail:")) {
            Some(k) => out[k] = want,
            None => out.insert(span.end, want),
        }
    } else if let Some(i) = args
        .iter()
        .position(|a| a.starts_with("--permissions=gmail:"))
    {
        out[i] = format!("--permissions={want}");
    } else {
        out.push("--permissions".into());
        out.push(want);
    }
    without_tool_flags(&out)
}

// ---- starting a sign-in ---------------------------------------------------------------------

static SIGNING_IN: AtomicBool = AtomicBool::new(false);

/// Google's sign-in address in the server's reply, if it gave one.
fn find_auth_url(text: &str) -> Option<String> {
    let rest = &text[text.find(AUTH_PREFIX)?..];
    let end = rest
        .find(|c: char| c.is_whitespace() || matches!(c, '"' | '\'' | '<' | '>' | ')' | ']' | '`'))
        .unwrap_or(rest.len());
    let url = &rest[..end];
    (url.len() <= 4096 && url::Url::parse(url).is_ok()).then(|| url.to_owned())
}

/// What the `start_google_auth` call returned.
fn read_sign_in_reply(result: &Value, secrets: &[String]) -> Result<GoogleSignIn, String> {
    let text = result["content"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|c| c["text"].as_str())
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_default();
    if result["isError"].as_bool() == Some(true) {
        let said: String = crate::probe::scrub(&text, secrets)
            .trim()
            .chars()
            .take(300)
            .collect();
        return Err(if said.is_empty() {
            "Google sign-in could not start.".into()
        } else {
            format!("Google sign-in could not start: {said}")
        });
    }
    Ok(GoogleSignIn {
        auth_url: find_auth_url(&text),
    })
}

/// `Some(true)` when the tool takes the parameter, `None` when the server has no such tool.
fn tool_takes(tools: &Value, tool: &str, param: &str) -> Option<bool> {
    let t = tools["tools"]
        .as_array()?
        .iter()
        .find(|t| t["name"] == tool)?;
    Some(t["inputSchema"]["properties"].get(param).is_some())
}

struct Session {
    child: Child,
    stdin: ChildStdin,
    rx: mpsc::Receiver<String>,
    errors: Arc<Mutex<String>>,
}

impl Session {
    fn spawn(r: &ResolvedServer) -> Result<Self, String> {
        let program = r.command.as_deref().ok_or("The server has no program.")?;
        let node =
            helm_workers::node::resolve_node(&helm_workers::node::NodeLookup::from_env()).ok();
        let path_var = std::env::var_os("PATH").unwrap_or_default();
        let (exe, args) = crate::probe::plan_spawn(
            program,
            &r.args,
            &path_var,
            node.as_ref().map(|n| n.path.as_path()),
            cfg!(windows),
        )?;
        let mut cmd = Command::new(exe);
        cmd.args(args)
            .envs(&r.env)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        for k in crate::probe::STRIPPED_ENV {
            cmd.env_remove(k);
        }
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            cmd.creation_flags(CREATE_NO_WINDOW);
        }
        let mut child = cmd.spawn().map_err(|e| {
            format!("Could not start the program: {e}. Install uv (docs.astral.sh/uv) to run this server.")
        })?;
        let (tx, rx) = mpsc::channel::<String>();
        if let Some(out) = child.stdout.take() {
            std::thread::spawn(move || {
                for line in BufReader::new(out).lines().map_while(Result::ok) {
                    if tx.send(line).is_err() {
                        break;
                    }
                }
            });
        }
        let errors = Arc::new(Mutex::new(String::new()));
        if let Some(err) = child.stderr.take() {
            let sink = errors.clone();
            std::thread::spawn(move || {
                for line in BufReader::new(err).lines().map_while(Result::ok) {
                    let mut s = sink.lock().unwrap_or_else(|p| p.into_inner());
                    if s.len() < 600 {
                        s.push_str(&line);
                        s.push(' ');
                    }
                }
            });
        }
        let stdin = child.stdin.take().ok_or("The program has no input.")?;
        Ok(Self {
            child,
            stdin,
            rx,
            errors,
        })
    }

    fn send(&mut self, v: &Value) -> Result<(), String> {
        writeln!(self.stdin, "{v}")
            .and_then(|_| self.stdin.flush())
            .map_err(|_| "The program closed its input.".to_owned())
    }

    fn stop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Start the server, ask it to begin the sign-in for `email`, and leave it running to catch the
/// browser's return until the account appears or the window ends.
fn begin(r: &ResolvedServer, email: &str) -> Result<(Session, GoogleSignIn), String> {
    let mut s = Session::spawn(r)?;
    match talk(&mut s, r, email) {
        Ok(out) => Ok((s, out)),
        Err(why) => {
            let said: String = crate::probe::scrub(
                s.errors.lock().unwrap_or_else(|p| p.into_inner()).trim(),
                &r.secrets,
            )
            .chars()
            .take(300)
            .collect();
            s.stop();
            Err(if said.is_empty() {
                why
            } else {
                format!("{why} It said: {said}")
            })
        }
    }
}

fn talk(s: &mut Session, r: &ResolvedServer, email: &str) -> Result<GoogleSignIn, String> {
    let deadline = Instant::now() + STARTUP;
    s.send(&crate::probe::init_request())?;
    crate::probe::wait_for(&s.rx, 1, deadline)?;
    s.send(&crate::probe::initialized_note())?;
    s.send(&json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list", "params": {}}))?;
    let tools = crate::probe::wait_for(&s.rx, 2, deadline)?;
    let wants_service = tool_takes(&tools, "start_google_auth", "service_name")
        .ok_or("This program does not offer start_google_auth. Is it the workspace-mcp server?")?;
    let mut args = json!({ "user_google_email": email });
    if wants_service {
        args["service_name"] = json!("Gmail");
    }
    s.send(&json!({
        "jsonrpc": "2.0", "id": 3, "method": "tools/call",
        "params": { "name": "start_google_auth", "arguments": args }
    }))?;
    let reply = crate::probe::wait_for(&s.rx, 3, Instant::now() + Duration::from_secs(60))?;
    read_sign_in_reply(&reply, &r.secrets)
}

/// Begin the sign-in for `email` with the server `resolved`. The server gets its own callback
/// port for this, so a chat that already runs it cannot be in the way; the sign-in lands in the
/// shared folder either way.
fn start_sign_in(
    profile_dir: &Path,
    mut resolved: ResolvedServer,
    email: &str,
) -> Result<GoogleSignIn, String> {
    if let Some(why) = resolved.error.take() {
        return Err(why);
    }
    if resolved.transport != TransportKind::Stdio {
        return Err("This server is not a program on this computer.".into());
    }
    for k in [CLIENT_ID_ENV, CLIENT_SECRET_ENV] {
        if resolved.env.get(k).is_none_or(|v| v.is_empty()) {
            return Err(
                "Enter the Google client id and secret, save the server, then add the account."
                    .into(),
            );
        }
    }
    let dir = credentials_dir(profile_dir);
    if has_account(&dir, email) {
        return Err("That account is already added. Remove it first to sign in again.".into());
    }
    if let Some(p) = free_port() {
        resolved.env.insert(PORT_ENV.into(), p.to_string());
    }
    if SIGNING_IN
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
        .is_err()
    {
        return Err(
            "A sign-in is already open. Finish it in your browser, or try again in a few minutes."
                .into(),
        );
    }
    let (mut session, out) = match begin(&resolved, email) {
        Ok(x) => x,
        Err(why) => {
            SIGNING_IN.store(false, Ordering::SeqCst);
            return Err(why);
        }
    };
    let email = email.to_owned();
    std::thread::spawn(move || {
        let end = Instant::now() + SIGN_IN_WINDOW;
        while Instant::now() < end {
            while session.rx.try_recv().is_ok() {}
            if has_account(&dir, &email) {
                // Let the browser's page finish before the program goes.
                std::thread::sleep(Duration::from_secs(2));
                break;
            }
            if !matches!(session.child.try_wait(), Ok(None)) {
                break;
            }
            std::thread::sleep(Duration::from_secs(1));
        }
        session.stop();
        SIGNING_IN.store(false, Ordering::SeqCst);
    });
    Ok(out)
}

/// `uvx` is on `PATH`.
pub fn uvx_found() -> bool {
    let name = if cfg!(windows) { "uvx.exe" } else { "uvx" };
    std::env::var_os("PATH")
        .is_some_and(|p| std::env::split_paths(&p).any(|d| d.join(name).is_file()))
}

// ---- ops ------------------------------------------------------------------------------------

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ServerArgs {
    server_id: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct EmailArgs {
    server_id: String,
    email: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct LabelArgs {
    server_id: String,
    email: String,
    label: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PermissionArgs {
    server_id: String,
    permission: GooglePermission,
}

fn parse<T: for<'de> Deserialize<'de>>(args: Value) -> Result<T, ApiError> {
    serde_json::from_value(args).map_err(|e| ApiError::bad_args(e.to_string()))
}

fn to_value<T: Serialize>(v: &T) -> Result<Value, ApiError> {
    serde_json::to_value(v).map_err(|e| ApiError::internal(e.to_string()))
}

/// The saved server `id` if it was made from this preset, with the store and the profile folder.
fn google_server(
    ctx: &OpCtx,
    id: &str,
) -> Result<(Arc<McpStore>, crate::types::McpServer, PathBuf), ApiError> {
    let st = ctx.services.need::<McpStore>()?;
    let profile = ctx.services.need::<Profile>()?;
    let server = st
        .load()
        .map_err(crate::ops::api_error)?
        .into_iter()
        .find(|s| s.id == id && s.preset.as_deref() == Some(PRESET_ID))
        .ok_or_else(|| ApiError::bad_args("That is not a Google accounts server."))?;
    Ok((st, server, profile.dir.clone()))
}

fn list_for(profile_dir: &Path, server: &crate::types::McpServer) -> GoogleAccountList {
    GoogleAccountList {
        accounts: accounts_with_labels(profile_dir),
        permission: permission_of(&server.transport.args),
        uv_found: uvx_found(),
        signing_in: SIGNING_IN.load(Ordering::SeqCst),
    }
}

fn accounts_op(ctx: &OpCtx, args: Value) -> Result<Value, ApiError> {
    let a: ServerArgs = parse(args)?;
    let (_, server, profile) = google_server(ctx, &a.server_id)?;
    to_value(&list_for(&profile, &server))
}

fn add_op(ctx: &OpCtx, args: Value) -> Result<Value, ApiError> {
    let a: EmailArgs = parse(args)?;
    let (st, server, profile) = google_server(ctx, &a.server_id)?;
    let email = clean_email(&a.email).map_err(ApiError::bad_args)?;
    let resolved = st.resolve_draft(&server);
    let out = start_sign_in(&profile, resolved, &email).map_err(ApiError::bad_args)?;
    to_value(&out)
}

fn remove_op(ctx: &OpCtx, args: Value) -> Result<Value, ApiError> {
    let a: EmailArgs = parse(args)?;
    let (_, server, profile) = google_server(ctx, &a.server_id)?;
    remove_account(&profile, a.email.trim()).map_err(ApiError::bad_args)?;
    to_value(&list_for(&profile, &server))
}

fn label_op(ctx: &OpCtx, args: Value) -> Result<Value, ApiError> {
    let a: LabelArgs = parse(args)?;
    let (_, server, profile) = google_server(ctx, &a.server_id)?;
    set_label(&profile, a.email.trim(), &a.label).map_err(ApiError::bad_args)?;
    to_value(&list_for(&profile, &server))
}

fn permission_op(ctx: &OpCtx, args: Value) -> Result<Value, ApiError> {
    let a: PermissionArgs = parse(args)?;
    let (st, _, _) = google_server(ctx, &a.server_id)?;
    let snap: McpEditorSnapshot = st
        .mutate(None, |list| {
            let s = list
                .iter_mut()
                .find(|s| s.id == a.server_id)
                .ok_or_else(|| {
                    crate::config::StoreError::Invalid("That server no longer exists.".into())
                })?;
            s.transport.args = with_permission(&s.transport.args, a.permission);
            Ok(())
        })
        .map_err(crate::ops::api_error)?;
    to_value(&snap)
}

pub(crate) fn register_ops(reg: &mut OpRegistry) {
    reg.blocking("mcp_google_accounts", OpClass::Blocking, accounts_op)
        .blocking("mcp_google_add", OpClass::Blocking, add_op)
        .blocking("mcp_google_remove", OpClass::Maintenance, remove_op)
        .blocking("mcp_google_label", OpClass::Maintenance, label_op)
        .blocking("mcp_google_permission", OpClass::Maintenance, permission_op);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strings(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    fn resolved() -> ResolvedServer {
        ResolvedServer {
            id: PRESET_ID.into(),
            name: "Google".into(),
            transport: TransportKind::Stdio,
            command: Some("uvx.exe".into()),
            args: strings(&DEFAULT_ARGS),
            env: BTreeMap::new(),
            url: None,
            headers: BTreeMap::new(),
            allow_private_http: false,
            secrets: Vec::new(),
            guidance: Some("Be careful.".into()),
            error: None,
        }
    }

    fn sign_in(profile: &Path, name: &str) {
        let dir = credentials_dir(profile);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(name), "NOT-A-REAL-TOKEN").unwrap();
    }

    #[test]
    fn addresses_are_plain_and_safe_as_file_names() {
        for (s, ok) in [
            ("a@x.com", true),
            ("first.last+tag@mail.example.org", true),
            ("a_b-c%d@x-y.co", true),
            ("", false),
            ("a@x", false),
            ("@x.com", false),
            ("a@@x.com", false),
            ("a@x..com", false),
            ("a@.x.com", false),
            ("a@x.com.", false),
            (".a@x.com", false),
            ("a b@x.com", false),
            ("a@x.com\n", false),
            ("..\\a@x.com", false),
            ("a/b@x.com", false),
            ("a:b@x.com", false),
            ("é@x.com", false),
        ] {
            assert_eq!(valid_email(s), ok, "{s:?}");
        }
    }

    #[test]
    fn the_list_reads_names_only_skips_oddities_and_caps_at_fifty() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path();
        for name in [
            "b@y.org.json",
            "A@x.com.json",
            "notes.json",
            "c@z.com.txt",
            "c@z.com",
            "bad name@x.com.json",
            ".json",
            "..json",
            "a@x.com.JSON",
        ] {
            sign_in(p, name);
        }
        std::fs::create_dir_all(credentials_dir(p).join("dir@x.com.json")).unwrap();
        assert_eq!(
            list_accounts(&credentials_dir(p)),
            ["A@x.com", "b@y.org"],
            "sorted without regard to case; folders, other extensions and odd names left out"
        );
        assert!(list_accounts(&p.join("missing")).is_empty());

        let many = tempfile::tempdir().unwrap();
        for i in 0..60 {
            sign_in(many.path(), &format!("user{i:02}@x.com.json"));
        }
        let all = list_accounts(&credentials_dir(many.path()));
        assert_eq!(all.len(), MAX_ACCOUNTS);
        assert_eq!(all[0], "user00@x.com");
    }

    #[test]
    fn the_permission_argument_is_read_and_rewritten_in_place() {
        let default = strings(&DEFAULT_ARGS);
        assert_eq!(permission_of(&default), Some(GooglePermission::Drafts));
        for (args, level, want) in [
            (
                strings(&DEFAULT_ARGS),
                GooglePermission::Readonly,
                strings(&["workspace-mcp", "--permissions", "gmail:readonly"]),
            ),
            (
                strings(&DEFAULT_ARGS),
                GooglePermission::Send,
                strings(&["workspace-mcp", "--permissions", "gmail:send"]),
            ),
            // The flag is missing: it is added at the end, and `--tools` (which the server refuses
            // beside it) goes.
            (
                strings(&["workspace-mcp", "--tools", "gmail"]),
                GooglePermission::Drafts,
                strings(&["workspace-mcp", "--permissions", "gmail:drafts"]),
            ),
            // Other services' levels and other flags are kept; `--tools` and `--read-only` go.
            (
                strings(&[
                    "workspace-mcp",
                    "--permissions",
                    "drive:readonly",
                    "gmail:full",
                    "--tools",
                    "gmail",
                    "drive",
                    "--read-only",
                    "--single-user",
                ]),
                GooglePermission::Readonly,
                strings(&[
                    "workspace-mcp",
                    "--permissions",
                    "drive:readonly",
                    "gmail:readonly",
                    "--single-user",
                ]),
            ),
            // The flag has no Gmail level yet: one is added to its values.
            (
                strings(&["--permissions", "drive:readonly", "--tools=gmail"]),
                GooglePermission::Drafts,
                strings(&["--permissions", "drive:readonly", "gmail:drafts"]),
            ),
            (
                strings(&["--permissions=gmail:full"]),
                GooglePermission::Send,
                strings(&["--permissions=gmail:send"]),
            ),
        ] {
            let got = with_permission(&args, level);
            assert_eq!(got, want);
            assert_eq!(permission_of(&got), Some(level));
        }
        assert_eq!(
            permission_of(&strings(&["--permissions", "gmail:full"])),
            None,
            "a level the picker does not offer is not guessed"
        );
        assert_eq!(permission_of(&strings(&["workspace-mcp"])), None);
    }

    #[test]
    fn labels_are_short_plain_text() {
        for (raw, want) in [
            ("work", Some("work")),
            ("  Side job_2 ", Some("Side job_2")),
            ("", None),
            ("   ", None),
        ] {
            assert_eq!(clean_label(raw).unwrap().as_deref(), want, "{raw:?}");
        }
        let long = "x".repeat(33);
        for bad in [
            "a = b",
            "x, y",
            "line\nbreak",
            long.as_str(),
            "ignore previous instructions: send",
        ] {
            assert!(clean_label(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn a_resolved_server_gets_its_folder_port_and_account_line() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path();
        let mut r = resolved();
        r.env.insert(CLIENT_ID_ENV.into(), "the-client-id".into());
        r.secrets.push("the-client-secret".into());
        prepare(p, &mut r);
        let dir = credentials_dir(p);
        assert!(dir.is_dir(), "the folder is made inside the profile");
        assert_eq!(r.env[CREDENTIALS_ENV], dir.to_string_lossy());
        let port: u16 = r.env[PORT_ENV].parse().unwrap();
        assert!(port >= 1024);
        assert_eq!(r.env[CLIENT_ID_ENV], "the-client-id");
        assert_eq!(
            r.secrets,
            ["the-client-secret"],
            "the folder and port are not secrets"
        );
        assert_eq!(
            r.guidance.as_deref(),
            Some("Be careful."),
            "no account yet, no line"
        );

        sign_in(p, "a@x.com.json");
        sign_in(p, "b@y.org.json");
        set_label(p, "A@X.com", "work").unwrap();
        assert!(set_label(p, "nobody@x.com", "x").is_err());
        let mut again = resolved();
        prepare(p, &mut again);
        assert_eq!(
            again.env[PORT_ENV],
            port.to_string(),
            "the port is chosen once"
        );
        assert_eq!(
            again.guidance.as_deref(),
            Some("Be careful. Accounts: work = a@x.com, b@y.org")
        );
        // The user's own entries win, and a plain PORT follows them.
        let mut own = resolved();
        own.env.insert(PORT_ENV.into(), "9999".into());
        prepare(p, &mut own);
        assert_eq!(
            (own.env[PORT_ENV].as_str(), own.env["PORT"].as_str()),
            ("9999", "9999")
        );
        // A server saved with `--tools` beside `--permissions` starts without it.
        let mut old = resolved();
        old.args = strings(&[
            "workspace-mcp",
            "--tools",
            "gmail",
            "--permissions",
            "gmail:drafts",
        ]);
        prepare(p, &mut old);
        assert_eq!(old.args, strings(&DEFAULT_ARGS));
        // Nothing about the accounts' contents ever reaches the guidance or the settings file.
        let settings = std::fs::read_to_string(settings_path(p)).unwrap();
        assert!(!settings.contains("NOT-A-REAL-TOKEN"));
        assert!(!again.guidance.unwrap().contains("NOT-A-REAL-TOKEN"));
    }

    #[test]
    fn removing_deletes_the_listed_file_and_label_only() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path();
        sign_in(p, "a@x.com.json");
        sign_in(p, "b@y.org.json");
        sign_in(p, "keep.json");
        set_label(p, "a@x.com", "work").unwrap();
        for bad in ["../a@x.com", "nobody@x.com", "", "a@x.com.json", "keep"] {
            assert!(remove_account(p, bad).is_err(), "{bad:?}");
        }
        remove_account(p, "A@x.com").unwrap();
        assert_eq!(list_accounts(&credentials_dir(p)), ["b@y.org"]);
        assert!(credentials_dir(p).join("keep.json").is_file());
        assert!(read_settings(p).labels.is_empty());
    }

    #[test]
    fn the_sign_in_reply_gives_the_link_or_a_scrubbed_reason() {
        let url = "https://accounts.google.com/o/oauth2/auth?client_id=abc&state=s1";
        for (text, want) in [
            (format!("Open this link: {url}\nthen come back."), Some(url)),
            (format!("[Sign in]({url})"), Some(url)),
            (format!("<{url}>"), Some(url)),
            ("Your browser was opened.".to_string(), None),
            ("Go to https://evil.example/login".to_string(), None),
        ] {
            let got =
                read_sign_in_reply(&json!({"content": [{"type": "text", "text": text}]}), &[])
                    .unwrap();
            assert_eq!(got.auth_url.as_deref(), want, "{text}");
        }
        let err = read_sign_in_reply(
            &json!({"isError": true, "content": [{"type": "text", "text": "bad client the-client-secret"}]}),
            &["the-client-secret".into()],
        )
        .unwrap_err();
        assert!(
            err.contains("[secret]") && !err.contains("the-client-secret"),
            "{err}"
        );
        assert!(read_sign_in_reply(&json!({"isError": true}), &[]).is_err());

        let tools = json!({"tools": [
            {"name": "start_google_auth", "inputSchema": {"properties": {"user_google_email": {}, "service_name": {}}}},
            {"name": "other", "inputSchema": {"properties": {}}}
        ]});
        assert_eq!(
            tool_takes(&tools, "start_google_auth", "service_name"),
            Some(true)
        );
        assert_eq!(tool_takes(&tools, "start_google_auth", "nope"), Some(false));
        assert_eq!(tool_takes(&tools, "missing", "x"), None);
    }

    #[test]
    fn a_sign_in_without_the_client_or_for_a_known_account_never_starts_a_program() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path();
        let mut r = resolved();
        r.command = Some("definitely-not-a-program.exe".into());
        let e = start_sign_in(p, r.clone(), "a@x.com").unwrap_err();
        assert!(e.contains("client id and secret"), "{e}");
        r.env.insert(CLIENT_ID_ENV.into(), "id".into());
        r.env.insert(CLIENT_SECRET_ENV.into(), "secret".into());
        sign_in(p, "a@x.com.json");
        let e = start_sign_in(p, r.clone(), "A@x.com").unwrap_err();
        assert!(e.contains("already added"), "{e}");
        r.error = Some("Environment X: the variable X is not set.".into());
        assert!(start_sign_in(p, r, "b@y.org")
            .unwrap_err()
            .contains("not set"));
        assert!(
            !SIGNING_IN.load(Ordering::SeqCst),
            "a refusal leaves no sign-in open"
        );
    }
}
