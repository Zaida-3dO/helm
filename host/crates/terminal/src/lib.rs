//! The terminal dock's host side.
//!
//! | op | class | does |
//! |---|---|---|
//! | `terminal_open` | blocking | `{cwd?, shell?, cols, rows}` → [`TerminalSession`]; at most 16 |
//! | `terminal_attach` | blocking | `{id}` → `{session, replay, seq}` (up to 512 KiB of recent output) |
//! | `terminal_write` | blocking | `{id, data}` |
//! | `terminal_ack` | blocking | `{id, units}`: UTF-16 units the renderer has written |
//! | `terminal_resize` | blocking | `{id, cols, rows}` |
//! | `terminal_close` | blocking | `{id}`: close the pseudo console, then end the Job after 2 s |
//! | `terminal_sessions` | blocking | → [`TerminalSession`]`[]` |
//! | `claude_sign_in` | blocking | `{cols?, rows?, token?}` → [`TerminalSession`]: the unmodified `claude auth login` (or, with `token`, `claude setup-token`) in a new session |
//!
//! `claude_sign_in` is the only way a session runs something other than a shell, and it takes no
//! program or arguments from the renderer: the host finds Claude Code the way a worker does and
//! starts it with the fixed arguments `auth login`, or `setup-token` when `token` is true. A
//! `setup-token` session picks the token out of its own output (see [`capture`]), seals it as the
//! chat token and blanks it in the session's replay.
//!
//! Output rides the one event channel as `terminal {id, seq, data}` in UTF-8-safe
//! chunks of at most 64 KiB, and `terminal_exit {id, code}`. Output pauses past 1 MiB of
//! unacknowledged UTF-16 units. Each session's shell runs inside its own Job Object, so a
//! close or quit ends the shell and everything it started. App-internal environment variables are
//! not passed to the shell.

pub mod bindings;
pub mod capture;
#[cfg(windows)]
mod job;
pub mod shell;
pub mod stream;

use helm_ipc::{ApiError, AppEvent, EventSink, OpClass, OpCtx, OpRegistry, Services};
use portable_pty::{native_pty_system, ChildKiller, CommandBuilder, MasterPty, PtySize};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU16, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use stream::{split_chunks, utf16_units, FlowGate, Replay, Utf8Carry, CHUNK_MAX};
use ts_rs::TS;

pub const SESSIONS_MAX: usize = 16;
/// Variables a Claude sign-in must not inherit, so the login writes the normal sign-in and not an
/// override from the environment (the names a worker strips for the claude child).
pub const SIGN_IN_STRIP_ENV: [&str; 6] = [
    "ANTHROPIC_API_KEY",
    "ANTHROPIC_AUTH_TOKEN",
    "ANTHROPIC_BASE_URL",
    "CLAUDE_CODE_OAUTH_TOKEN",
    "CLAUDECODE",
    // Where the host hands a worker the saved Claude token: never a terminal's business.
    "HELM_CLAUDE_TOKEN",
];
/// The tab title of the sign-in session.
pub const SIGN_IN_TITLE: &str = "Claude sign-in";

/// How long a closed shell gets to exit before its Job is ended.
pub const CLOSE_GRACE: Duration = Duration::from_secs(2);

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct TerminalSession {
    pub id: String,
    /// "pwsh", "powershell" or "bash": the tab title.
    pub shell: String,
    pub cwd: String,
    pub title: String,
    pub running: bool,
}

#[derive(Clone, Debug, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct TerminalOpenArgs {
    #[ts(optional)]
    pub cwd: Option<String>,
    /// "pwsh" | "powershell" | "gitbash"; defaults to the Settings → Shell choice, then pwsh.
    #[ts(optional)]
    pub shell: Option<String>,
    pub cols: u16,
    pub rows: u16,
}

#[derive(Clone, Debug, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct TerminalAttachResult {
    pub session: TerminalSession,
    pub replay: String,
    #[ts(type = "number")]
    pub seq: u64,
}

/// Called with a Claude token the sign-in session printed. It must never log it.
pub type TokenSink = Arc<dyn Fn(&str) + Send + Sync>;

/// What replaces a captured token in the session's replay.
const TOKEN_REPLACED: &str = "[Claude token saved by Helm]";
/// How much of the newest output is searched for a token.
const CAPTURE_WINDOW: usize = 16 * 1024;

/// What a session runs: a shell, or (only through [`sign_in_launch`] and [`setup_token_launch`])
/// the Claude sign-in.
struct Launch {
    program: PathBuf,
    args: Vec<String>,
    /// The tab title.
    title: String,
    /// The session's `shell` field.
    session_shell: String,
    /// Variables to remove besides the app-internal ones.
    strip: &'static [&'static str],
    /// The whole environment to start from (a worker's), instead of Helm's own; `None` keeps Helm's.
    base_env: Option<Vec<(String, String)>>,
    /// Told the exit code when the program ends.
    after_exit: Option<fn(i64)>,
    /// Given the token the program prints (the token sign-in only); the token is then blanked in
    /// the replay.
    catch_token: Option<TokenSink>,
}

/// `<claude> auth login`: the fixed arguments, and the environment a worker would strip.
fn sign_in_launch(claude: PathBuf, base_env: Vec<(String, String)>) -> Launch {
    Launch {
        program: claude,
        args: vec!["auth".into(), "login".into()],
        title: SIGN_IN_TITLE.into(),
        session_shell: "claude".into(),
        strip: &SIGN_IN_STRIP_ENV,
        base_env: Some(base_env),
        after_exit: Some(sign_in_ended),
        catch_token: None,
    }
}

/// `<claude> setup-token`: the same session, but it makes the long-lived token chats sign in with,
/// and `save` is given the token when it appears.
fn setup_token_launch(claude: PathBuf, base_env: Vec<(String, String)>, save: TokenSink) -> Launch {
    Launch {
        args: vec!["setup-token".into()],
        catch_token: Some(save),
        ..sign_in_launch(claude, base_env)
    }
}

/// A sign-in that ended well (exit code 0) may have changed the sign-in every Claude process
/// shares: the gate checks it afresh, and the chats that stopped waiting for a sign-in carry on.
/// Helm reads nothing of what the sign-in wrote.
fn sign_in_ended(code: i64) {
    if code == 0 {
        helm_core::signin_gate::global().signed_in();
    }
}

/// The complete environment of a launch that brings its own base, with the stripped names left
/// out; `None` when the launch starts from Helm's own environment.
fn launch_env(launch: &Launch) -> Option<Vec<(String, String)>> {
    launch.base_env.as_ref().map(|base| {
        base.iter()
            .filter(|(k, _)| !strips(k, launch.strip))
            .cloned()
            .collect()
    })
}

/// Whether a variable is left out of a session's environment.
fn strips(name: &str, extra: &[&str]) -> bool {
    shell::is_internal_env(name) || extra.iter().any(|e| name.eq_ignore_ascii_case(e))
}

struct Session {
    meta: Mutex<TerminalSession>,
    running: AtomicBool,
    seq: AtomicU64,
    master: Mutex<Option<Box<dyn MasterPty + Send>>>,
    writer: Mutex<Option<Box<dyn Write + Send>>>,
    killer: Mutex<Box<dyn ChildKiller + Send + Sync>>,
    replay: Mutex<Replay>,
    flow: FlowGate,
    /// The terminal's width, as the renderer last set it.
    cols: AtomicU16,
    #[cfg(windows)]
    job: Option<job::Job>,
}

impl Session {
    fn snapshot(&self) -> TerminalSession {
        let mut m = self.meta.lock().unwrap().clone();
        m.running = self.running.load(Ordering::SeqCst);
        m
    }

    /// Hang up, then end the whole tree after the grace period if it is still running.
    fn close(self: &Arc<Self>) {
        self.flow.close();
        self.writer.lock().unwrap().take();
        // Dropping the master closes the pseudo console (ClosePseudoConsole on Windows).
        self.master.lock().unwrap().take();
        let me = self.clone();
        std::thread::spawn(move || {
            let deadline = std::time::Instant::now() + CLOSE_GRACE;
            while me.running.load(Ordering::SeqCst) && std::time::Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(50));
            }
            if me.running.load(Ordering::SeqCst) {
                me.kill_now();
            }
        });
    }

    fn kill_now(&self) {
        #[cfg(windows)]
        if let Some(j) = &self.job {
            j.terminate();
        }
        let _ = self.killer.lock().unwrap().kill();
    }
}

/// Every terminal session; registered as a service.
pub struct TerminalManager {
    sessions: Mutex<HashMap<String, Arc<Session>>>,
    events: Arc<dyn EventSink>,
    default_cwd: PathBuf,
    shell_pref: Box<dyn Fn() -> String + Send + Sync>,
    /// The sign-in session, while there is one, so a second request shows it instead of starting
    /// another login.
    sign_in: Mutex<Option<(String, bool)>>,
}

impl TerminalManager {
    pub fn new(
        events: Arc<dyn EventSink>,
        default_cwd: PathBuf,
        shell_pref: Box<dyn Fn() -> String + Send + Sync>,
    ) -> Self {
        Self {
            sessions: Mutex::new(HashMap::new()),
            events,
            default_cwd,
            shell_pref,
            sign_in: Mutex::new(None),
        }
    }

    pub fn open(&self, a: TerminalOpenArgs) -> Result<TerminalSession, String> {
        if self.sessions.lock().unwrap().len() >= SESSIONS_MAX {
            return Err(format!(
                "At most {SESSIONS_MAX} terminals can be open; close one first."
            ));
        }
        let cwd = match a.cwd.as_deref().filter(|c| !c.trim().is_empty()) {
            Some(c) => PathBuf::from(c),
            None => self.default_cwd.clone(),
        };
        if !cwd.is_dir() {
            return Err(format!("{} is not a folder.", cwd.display()));
        }
        let want = a.shell.unwrap_or_else(|| (self.shell_pref)());
        let sh = shell::resolve(&want).ok_or_else(|| {
            "No shell found: install PowerShell 7, or choose another shell in Settings → Shell."
                .to_owned()
        })?;
        self.spawn(
            cwd,
            Launch {
                program: sh.program,
                args: sh.args,
                title: sh.title.clone(),
                session_shell: sh.title,
                strip: &[],
                base_env: None,
                after_exit: None,
                catch_token: None,
            },
            a.cols,
            a.rows,
        )
    }

    /// A session running the unmodified `claude auth login` in the default folder. The caller
    /// finds the program; the arguments are fixed here.
    pub fn open_sign_in(
        &self,
        claude: PathBuf,
        env: Vec<(String, String)>,
        cols: u16,
        rows: u16,
    ) -> Result<TerminalSession, String> {
        self.open_claude(claude, env, cols, rows, None)
    }

    /// A session running the unmodified `claude setup-token`; `save` is given the token it prints.
    pub fn open_setup_token(
        &self,
        claude: PathBuf,
        env: Vec<(String, String)>,
        cols: u16,
        rows: u16,
        save: TokenSink,
    ) -> Result<TerminalSession, String> {
        self.open_claude(claude, env, cols, rows, Some(save))
    }

    fn open_claude(
        &self,
        claude: PathBuf,
        env: Vec<(String, String)>,
        cols: u16,
        rows: u16,
        save: Option<TokenSink>,
    ) -> Result<TerminalSession, String> {
        let token_flow = save.is_some();
        let mut current = self.sign_in.lock().unwrap();
        // A second request shows the sign-in that is already running, if it is the same kind.
        if let Some((id, _)) = current.as_ref().filter(|(_, kind)| *kind == token_flow) {
            let running = self
                .sessions
                .lock()
                .unwrap()
                .get(id)
                .map(|s| s.snapshot())
                .filter(|m| m.running);
            if let Some(m) = running {
                return Ok(m);
            }
        }
        if self.sessions.lock().unwrap().len() >= SESSIONS_MAX {
            return Err(format!(
                "At most {SESSIONS_MAX} terminals can be open; close one first."
            ));
        }
        let cwd = self.default_cwd.clone();
        if !cwd.is_dir() {
            return Err(format!("{} is not a folder.", cwd.display()));
        }
        let launch = match save {
            Some(save) => setup_token_launch(claude, env, save),
            None => sign_in_launch(claude, env),
        };
        let s = self.spawn(cwd, launch, cols, rows)?;
        *current = Some((s.id.clone(), token_flow));
        Ok(s)
    }

    fn spawn(
        &self,
        cwd: PathBuf,
        launch: Launch,
        cols: u16,
        rows: u16,
    ) -> Result<TerminalSession, String> {
        let pty = native_pty_system()
            .openpty(PtySize {
                rows: rows.clamp(2, 1000),
                cols: cols.clamp(2, 1000),
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|e| format!("could not open a terminal: {e}"))?;
        let mut cmd = CommandBuilder::new(&launch.program);
        for arg in &launch.args {
            cmd.arg(arg);
        }
        cmd.cwd(&cwd);
        match launch_env(&launch) {
            Some(env) => {
                cmd.env_clear();
                for (k, v) in env {
                    cmd.env(k, v);
                }
            }
            None => {
                for (k, _) in std::env::vars_os() {
                    if let Some(k) = k.to_str() {
                        if strips(k, launch.strip) {
                            cmd.env_remove(k);
                        }
                    }
                }
            }
        }
        cmd.env("TERM", "xterm-256color");
        let child = pty
            .slave
            .spawn_command(cmd)
            .map_err(|e| format!("could not start {}: {e}", launch.title))?;
        drop(pty.slave);
        #[cfg(windows)]
        let job = match child.process_id() {
            Some(pid) => match job::Job::for_pid(pid) {
                Ok(j) => Some(j),
                Err(e) => {
                    log::warn!("terminal job object: {e}");
                    None
                }
            },
            None => None,
        };
        let reader = pty
            .master
            .try_clone_reader()
            .map_err(|e| format!("terminal output: {e}"))?;
        let writer = pty
            .master
            .take_writer()
            .map_err(|e| format!("terminal input: {e}"))?;
        let id = uuid::Uuid::new_v4().to_string();
        let meta = TerminalSession {
            id: id.clone(),
            shell: launch.session_shell.clone(),
            cwd: cwd.display().to_string(),
            title: launch.title.clone(),
            running: true,
        };
        let session = Arc::new(Session {
            meta: Mutex::new(meta.clone()),
            running: AtomicBool::new(true),
            seq: AtomicU64::new(0),
            master: Mutex::new(Some(pty.master)),
            writer: Mutex::new(Some(writer)),
            killer: Mutex::new(child.clone_killer()),
            replay: Mutex::new(Replay::default()),
            flow: FlowGate::default(),
            cols: AtomicU16::new(cols.clamp(2, 1000)),
            #[cfg(windows)]
            job,
        });
        self.sessions
            .lock()
            .unwrap()
            .insert(id.clone(), session.clone());
        spawn_reader(
            session.clone(),
            reader,
            self.events.clone(),
            id.clone(),
            launch.catch_token.clone(),
        );
        spawn_waiter(session, child, self.events.clone(), id, launch.after_exit);
        Ok(meta)
    }

    fn get(&self, id: &str) -> Result<Arc<Session>, String> {
        self.sessions
            .lock()
            .unwrap()
            .get(id)
            .cloned()
            .ok_or_else(|| "That terminal is closed.".to_owned())
    }

    pub fn attach(&self, id: &str) -> Result<TerminalAttachResult, String> {
        let s = self.get(id)?;
        // Read the replay and its sequence number under one lock (the reader updates both there).
        let (replay, seq) = {
            let r = s.replay.lock().unwrap();
            (r.text(), s.seq.load(Ordering::SeqCst))
        };
        // A reattached renderer starts with nothing unacknowledged.
        s.flow.ack(u64::MAX);
        Ok(TerminalAttachResult {
            session: s.snapshot(),
            replay,
            seq,
        })
    }

    pub fn write(&self, id: &str, data: &str) -> Result<(), String> {
        if data.len() > 1024 * 1024 {
            return Err("input too large".into());
        }
        let s = self.get(id)?;
        let mut w = s.writer.lock().unwrap();
        let w = w.as_mut().ok_or("That terminal is closing.")?;
        w.write_all(data.as_bytes())
            .and_then(|_| w.flush())
            .map_err(|e| format!("terminal input: {e}"))
    }

    pub fn ack(&self, id: &str, units: u64) -> Result<(), String> {
        self.get(id)?.flow.ack(units);
        Ok(())
    }

    pub fn resize(&self, id: &str, cols: u16, rows: u16) -> Result<(), String> {
        let s = self.get(id)?;
        let m = s.master.lock().unwrap();
        let m = m.as_ref().ok_or("That terminal is closing.")?;
        m.resize(PtySize {
            rows: rows.clamp(2, 1000),
            cols: cols.clamp(2, 1000),
            pixel_width: 0,
            pixel_height: 0,
        })
        .map_err(|e| format!("resize: {e}"))?;
        s.cols.store(cols.clamp(2, 1000), Ordering::SeqCst);
        Ok(())
    }

    pub fn close(&self, id: &str) -> Result<(), String> {
        let s = self
            .sessions
            .lock()
            .unwrap()
            .remove(id)
            .ok_or_else(|| "That terminal is closed.".to_owned())?;
        s.close();
        Ok(())
    }

    pub fn sessions(&self) -> Vec<TerminalSession> {
        let mut v: Vec<TerminalSession> = self
            .sessions
            .lock()
            .unwrap()
            .values()
            .map(|s| s.snapshot())
            .collect();
        v.sort_by(|a, b| a.id.cmp(&b.id));
        v
    }

    /// Quit: end every session's tree now.
    pub fn shutdown(&self) {
        let all: Vec<Arc<Session>> = self
            .sessions
            .lock()
            .unwrap()
            .drain()
            .map(|(_, s)| s)
            .collect();
        for s in all {
            s.flow.close();
            s.kill_now();
        }
    }
}

fn spawn_reader(
    s: Arc<Session>,
    mut reader: Box<dyn Read + Send>,
    events: Arc<dyn EventSink>,
    id: String,
    catch_token: Option<TokenSink>,
) {
    std::thread::spawn(move || {
        let mut buf = vec![0u8; CHUNK_MAX];
        let mut carry = Utf8Carry::default();
        let mut catch_token = catch_token;
        loop {
            let n = match reader.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => n,
            };
            let text = carry.push(&buf[..n]);
            if text.is_empty() {
                continue;
            }
            for piece in split_chunks(&text, CHUNK_MAX) {
                let seq = {
                    let mut replay = s.replay.lock().unwrap();
                    replay.push(piece);
                    s.seq.fetch_add(1, Ordering::SeqCst) + 1
                };
                s.flow.sent(utf16_units(piece));
                events.emit(
                    AppEvent::new("terminal")
                        .with("id", id.clone())
                        .with("seq", seq)
                        .with("data", piece),
                );
            }
            // A token sign-in: once the token has been printed whole, hand it over, blank it in
            // the replay and look no further.
            if let Some(save) = catch_token.as_ref() {
                if let Some(token) = take_token(&s.replay, s.cols.load(Ordering::SeqCst) as usize) {
                    save(&token);
                    catch_token = None;
                }
            }
            s.flow.wait_for_room(Duration::from_millis(250));
        }
    });
}

/// The complete token in the session's newest output, if there is one; the replay then shows
/// [`TOKEN_REPLACED`] in its place.
fn take_token(replay: &Mutex<Replay>, cols: usize) -> Option<String> {
    let mut replay = replay.lock().unwrap();
    let text = replay.text();
    let mut from = text.len().saturating_sub(CAPTURE_WINDOW);
    while !text.is_char_boundary(from) {
        from += 1;
    }
    let found = capture::find(&text[from..], cols)?;
    let mut scrubbed = String::with_capacity(text.len());
    scrubbed.push_str(&text[..from + found.span.start]);
    scrubbed.push_str(TOKEN_REPLACED);
    scrubbed.push_str(&text[from + found.span.end..]);
    replay.replace_all(scrubbed);
    Some(found.token)
}

fn spawn_waiter(
    s: Arc<Session>,
    mut child: Box<dyn portable_pty::Child + Send + Sync>,
    events: Arc<dyn EventSink>,
    id: String,
    after_exit: Option<fn(i64)>,
) {
    std::thread::spawn(move || {
        let code = child.wait().map(|st| st.exit_code() as i64).unwrap_or(-1);
        s.running.store(false, Ordering::SeqCst);
        s.flow.close();
        if let Some(f) = after_exit {
            f(code);
        }
        events.emit(
            AppEvent::new("terminal_exit")
                .with("id", id)
                .with("code", code),
        );
    });
}

pub fn register_ops(reg: &mut OpRegistry) {
    reg.blocking("terminal_open", OpClass::Blocking, op_open)
        .blocking("terminal_attach", OpClass::Blocking, op_attach)
        .blocking("terminal_write", OpClass::Blocking, op_write)
        .blocking("terminal_ack", OpClass::Blocking, op_ack)
        .blocking("terminal_resize", OpClass::Blocking, op_resize)
        .blocking("terminal_close", OpClass::Blocking, op_close)
        .blocking("terminal_sessions", OpClass::Blocking, op_sessions)
        .blocking("claude_sign_in", OpClass::Blocking, op_claude_sign_in);
}

/// Adds the [`TerminalManager`]. The shell preference is read from the database on each open.
pub fn register_services(services: &mut Services, events: &Arc<dyn EventSink>) {
    let db = services.get::<helm_core::db::Database>();
    let pref = move || -> String {
        db.as_ref()
            .and_then(|db| {
                db.snapshot(|tx| {
                    Ok::<_, rusqlite::Error>(helm_core::prefs::read(tx, "terminalShell"))
                })
                .ok()
                .flatten()
            })
            .and_then(|v| v.as_str().map(str::to_owned))
            .unwrap_or_else(|| "pwsh".to_owned())
    };
    let home = std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    services.insert(Arc::new(TerminalManager::new(
        events.clone(),
        home,
        Box::new(pref),
    )));
}

fn bad(e: impl std::fmt::Display) -> ApiError {
    ApiError::bad_args(e.to_string())
}

fn id_arg(args: &Value) -> Result<String, ApiError> {
    args.get("id")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| bad("id must be a string"))
}

fn mgr(ctx: &OpCtx) -> Result<Arc<TerminalManager>, ApiError> {
    ctx.services.need::<TerminalManager>()
}

fn op_open(ctx: &OpCtx, args: Value) -> Result<Value, ApiError> {
    let a: TerminalOpenArgs = serde_json::from_value(args).map_err(bad)?;
    let s = mgr(ctx)?.open(a).map_err(bad)?;
    serde_json::to_value(s).map_err(|e| ApiError::internal(e.to_string()))
}

fn op_attach(ctx: &OpCtx, args: Value) -> Result<Value, ApiError> {
    let r = mgr(ctx)?.attach(&id_arg(&args)?).map_err(bad)?;
    serde_json::to_value(r).map_err(|e| ApiError::internal(e.to_string()))
}

fn op_write(ctx: &OpCtx, args: Value) -> Result<Value, ApiError> {
    let data = args
        .get("data")
        .and_then(Value::as_str)
        .ok_or_else(|| bad("data must be a string"))?;
    mgr(ctx)?.write(&id_arg(&args)?, data).map_err(bad)?;
    Ok(Value::Null)
}

fn op_ack(ctx: &OpCtx, args: Value) -> Result<Value, ApiError> {
    let units = args
        .get("units")
        .and_then(Value::as_u64)
        .ok_or_else(|| bad("units must be a number"))?;
    mgr(ctx)?.ack(&id_arg(&args)?, units).map_err(bad)?;
    Ok(Value::Null)
}

fn op_resize(ctx: &OpCtx, args: Value) -> Result<Value, ApiError> {
    let n = |k: &str| {
        args.get(k)
            .and_then(Value::as_u64)
            .map(|v| v.min(1000) as u16)
            .ok_or_else(|| bad(format!("{k} must be a number")))
    };
    mgr(ctx)?
        .resize(&id_arg(&args)?, n("cols")?, n("rows")?)
        .map_err(bad)?;
    Ok(Value::Null)
}

fn op_close(ctx: &OpCtx, args: Value) -> Result<Value, ApiError> {
    mgr(ctx)?.close(&id_arg(&args)?).map_err(bad)?;
    Ok(Value::Null)
}

fn op_sessions(ctx: &OpCtx, _args: Value) -> Result<Value, ApiError> {
    Ok(json!(mgr(ctx)?.sessions()))
}

/// Start `claude auth login` in a new session, or with `token: true` `claude setup-token`, whose
/// token Helm seals as the chat token. Claude Code is found as a worker finds it; Helm never reads
/// the sign-in `auth login` writes.
fn op_claude_sign_in(ctx: &OpCtx, args: Value) -> Result<Value, ApiError> {
    let n = |k: &str, d: u16| {
        args.get(k)
            .and_then(Value::as_u64)
            .map(|v| v.min(1000) as u16)
            .unwrap_or(d)
    };
    let profile = ctx.services.need::<helm_core::profile::Profile>()?;
    let env = helm_workers::env::base_environment();
    let (runtime, _) = helm_workers::launch::ClaudeRuntime::load(&profile.dir);
    let home = env
        .get("USERPROFILE")
        .or_else(|| env.get("HOME"))
        .map(PathBuf::from);
    let (claude, _) =
        helm_workers::status::resolve_claude(&env, &runtime.executable, home.as_deref())
            .map_err(bad)?;
    // The environment a chat's claude gets, so the login is written where chats look for it.
    let pairs = env
        .into_pairs()
        .into_iter()
        .filter_map(|(k, v)| Some((k.into_string().ok()?, v.into_string().ok()?)))
        .collect();
    let (cols, rows) = (n("cols", 100), n("rows", 24));
    let m = mgr(ctx)?;
    let s = if args.get("token").and_then(Value::as_bool).unwrap_or(false) {
        let platform = ctx.services.need::<helm_platform::Platform>()?;
        let creds = Arc::new(helm_core::chat_token::credentials(&profile, &platform));
        let events = ctx.events.clone();
        // The token is sealed here, announced without its text, and never logged.
        let save: TokenSink = Arc::new(move |raw| {
            match helm_core::chat_token::save(&creds, raw, helm_core::now_ms() as u64) {
                Ok(status) => {
                    events.emit(AppEvent::new("claude_token").with("status", json!(status)))
                }
                Err(e) => log::warn!("the token from the sign-in was not saved: {e}"),
            }
        });
        m.open_setup_token(claude, pairs, cols, rows, save)
    } else {
        m.open_sign_in(claude, pairs, cols, rows)
    }
    .map_err(bad)?;
    serde_json::to_value(s).map_err(|e| ApiError::internal(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use helm_ipc::RecordingSink;
    use std::time::Instant;

    fn manager(sink: Arc<RecordingSink>) -> TerminalManager {
        TerminalManager::new(sink, std::env::temp_dir(), Box::new(|| "pwsh".to_owned()))
    }

    fn output(sink: &RecordingSink, id: &str) -> String {
        sink.events
            .lock()
            .unwrap()
            .iter()
            .filter(|e| e.kind == "terminal" && e.fields["id"] == id)
            .map(|e| e.fields["data"].as_str().unwrap_or("").to_owned())
            .collect()
    }

    /// SH8: a real shell prints and the output streams back (pwsh, else Windows PowerShell).
    #[test]
    #[cfg(windows)]
    fn a_real_shell_echoes_hi_and_exits() {
        let sink = Arc::new(RecordingSink::default());
        let m = manager(sink.clone());
        let shell = if shell::resolve("pwsh")
            .map(|s| s.title == "pwsh")
            .unwrap_or(false)
        {
            "pwsh"
        } else {
            "powershell"
        };
        let s = m
            .open(TerminalOpenArgs {
                cwd: None,
                shell: Some(shell.into()),
                cols: 80,
                rows: 24,
            })
            .expect("open a shell");
        // ConPTY asks for the cursor position (ESC[6n) before the shell draws; a terminal answers.
        let start = Instant::now();
        while !output(&sink, &s.id).contains("\u{1b}[6n")
            && start.elapsed() < Duration::from_secs(10)
        {
            std::thread::sleep(Duration::from_millis(50));
        }
        m.write(&s.id, "\u{1b}[1;1R").unwrap();
        m.write(&s.id, "echo hi-from-helm; exit\r").unwrap();
        while !output(&sink, &s.id).contains("hi-from-helm\r\n")
            && start.elapsed() < Duration::from_secs(30)
        {
            std::thread::sleep(Duration::from_millis(100));
        }
        let out = output(&sink, &s.id);
        assert!(out.contains("hi-from-helm"), "output: {out:?}");
        while !sink.kinds().contains(&"terminal_exit".to_owned())
            && start.elapsed() < Duration::from_secs(30)
        {
            std::thread::sleep(Duration::from_millis(100));
        }
        assert!(sink.kinds().contains(&"terminal_exit".to_owned()));
        let att = m.attach(&s.id).unwrap();
        assert!(
            att.replay.contains("hi-from-helm"),
            "replay keeps recent output"
        );
        assert!(!att.session.running);
        m.close(&s.id).unwrap();
        assert!(m.sessions().is_empty());
    }

    /// The sign-in runs exactly `auth login`, as a titled session, and starts from the given
    /// (worker) environment minus the variables that would make the login write an override.
    #[test]
    fn the_sign_in_launch_is_fixed_and_its_environment_is_stripped() {
        let base: Vec<(String, String)> = [
            "PATH",
            "USERPROFILE",
            "CLAUDE_CONFIG_DIR",
            "ANTHROPIC_FOO",
            "ANTHROPIC_API_KEY",
            "anthropic_auth_token",
            "ANTHROPIC_BASE_URL",
            "CLAUDE_CODE_OAUTH_TOKEN",
            "CLAUDECODE",
            "HELM_LOCAL_MODEL_KEY",
            "HELM_CLAUDE_TOKEN",
        ]
        .iter()
        .map(|k| (k.to_string(), "x".to_string()))
        .collect();
        let l = sign_in_launch(PathBuf::from("claude.exe"), base);
        assert_eq!(l.program, PathBuf::from("claude.exe"));
        assert_eq!(l.args, ["auth", "login"]);
        assert_eq!(l.title, "Claude sign-in");
        let kept: Vec<String> = launch_env(&l)
            .unwrap()
            .into_iter()
            .map(|(k, _)| k)
            .collect();
        assert_eq!(
            kept,
            ["PATH", "USERPROFILE", "CLAUDE_CONFIG_DIR", "ANTHROPIC_FOO"]
        );
        // The token sign-in is the same session with its own fixed arguments and a place to put
        // the token.
        let t = setup_token_launch(PathBuf::from("claude.exe"), Vec::new(), Arc::new(|_| {}));
        assert_eq!(t.args, ["setup-token"]);
        assert_eq!(t.title, "Claude sign-in");
        assert!(t.catch_token.is_some() && l.catch_token.is_none());
        // A shell launch brings no base and strips only the app-internal variables.
        assert!(!strips("ANTHROPIC_API_KEY", &[]));
        assert!(strips("HELM_LOCAL_MODEL_KEY", &[]));
        assert!(strips("HELM_CLAUDE_TOKEN", &[]));
        // Only a sign-in that ended well tells the sign-in gate, which then checks afresh.
        let gate = helm_core::signin_gate::global();
        let before = gate.sign_ins();
        let ended = l.after_exit.expect("the sign-in reports its end");
        ended(1);
        assert_eq!(gate.sign_ins(), before);
        ended(0);
        assert_eq!(gate.sign_ins(), before + 1);
    }

    /// The token the sign-in prints is handed over once, whole, however the output was cut into
    /// reads, and the replay shows a note where it was.
    #[test]
    fn a_printed_token_is_taken_once_and_blanked_in_the_replay() {
        let token = "sk-ant-oat01-AbC123_def-GHI456jklMNO789pqrSTUvwx-YZ0123456789abcdefghij";
        let out = format!(
            "Opening the browser...\r\n\r\nYour token:\r\n\x1b[1m{token}\x1b[0m\r\n\r\nStore it safely.\r\n"
        );
        let replay = Mutex::new(Replay::default());
        let mut taken = Vec::new();
        // Cut in the middle of the token: nothing is taken until the token is whole.
        let cut = out.find("GHI456").unwrap();
        let end = out.find("Store").unwrap();
        for piece in [&out[..cut], &out[cut..end], &out[end..]] {
            replay.lock().unwrap().push(piece);
            taken.extend(take_token(&replay, 120));
        }
        assert_eq!(taken, [token]);
        let shown = replay.lock().unwrap().text();
        assert!(
            !shown.contains("sk-ant-") && !shown.contains("GHI456"),
            "{shown:?}"
        );
        assert!(shown.contains(TOKEN_REPLACED) && shown.contains("Your token:"));
        assert!(shown.ends_with("Store it safely.\r\n"));
        // Nothing left to take on the next read.
        assert!(take_token(&replay, 120).is_none());
    }

    #[test]
    fn a_missing_folder_is_refused_and_the_cap_is_enforced() {
        let m = manager(Arc::new(RecordingSink::default()));
        let r = m.open(TerminalOpenArgs {
            cwd: Some("Z:/definitely/not/here".into()),
            shell: None,
            cols: 80,
            rows: 24,
        });
        assert!(r.unwrap_err().contains("not a folder"));
        assert!(m.close("nope").is_err());
        assert!(m.write("nope", "x").is_err());
    }

    #[test]
    #[cfg(windows)]
    fn close_ends_a_shell_that_ignores_the_hangup_within_the_grace() {
        let sink = Arc::new(RecordingSink::default());
        let m = manager(sink.clone());
        let s = m
            .open(TerminalOpenArgs {
                cwd: None,
                shell: Some("powershell".into()),
                cols: 80,
                rows: 24,
            })
            .unwrap();
        // Something long-running in the foreground.
        std::thread::sleep(Duration::from_millis(500));
        m.write(&s.id, "\u{1b}[1;1R").unwrap();
        m.write(&s.id, "Start-Sleep -Seconds 120\r").unwrap();
        std::thread::sleep(Duration::from_millis(500));
        let start = Instant::now();
        m.close(&s.id).unwrap();
        while !sink.kinds().contains(&"terminal_exit".to_owned())
            && start.elapsed() < Duration::from_secs(10)
        {
            std::thread::sleep(Duration::from_millis(50));
        }
        assert!(sink.kinds().contains(&"terminal_exit".to_owned()), "ended");
        assert!(
            start.elapsed() < Duration::from_secs(5),
            "within the grace plus slack"
        );
    }
}
