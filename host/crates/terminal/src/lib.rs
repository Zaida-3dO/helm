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
//!
//! Output rides the one event channel as `terminal {id, seq, data}` in UTF-8-safe
//! chunks of at most 64 KiB, and `terminal_exit {id, code}`. Output pauses past 1 MiB of
//! unacknowledged UTF-16 units. Each session's shell runs inside its own Job Object, so a
//! close or quit ends the shell and everything it started. App-internal environment variables are
//! not passed to the shell.

pub mod bindings;
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
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use stream::{split_chunks, utf16_units, FlowGate, Replay, Utf8Carry, CHUNK_MAX};
use ts_rs::TS;

pub const SESSIONS_MAX: usize = 16;
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

struct Session {
    meta: Mutex<TerminalSession>,
    running: AtomicBool,
    seq: AtomicU64,
    master: Mutex<Option<Box<dyn MasterPty + Send>>>,
    writer: Mutex<Option<Box<dyn Write + Send>>>,
    killer: Mutex<Box<dyn ChildKiller + Send + Sync>>,
    replay: Mutex<Replay>,
    flow: FlowGate,
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
        let pty = native_pty_system()
            .openpty(PtySize {
                rows: a.rows.clamp(2, 1000),
                cols: a.cols.clamp(2, 1000),
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|e| format!("could not open a terminal: {e}"))?;
        let mut cmd = CommandBuilder::new(&sh.program);
        for arg in &sh.args {
            cmd.arg(arg);
        }
        cmd.cwd(&cwd);
        for (k, _) in std::env::vars_os() {
            if let Some(k) = k.to_str() {
                if shell::is_internal_env(k) {
                    cmd.env_remove(k);
                }
            }
        }
        cmd.env("TERM", "xterm-256color");
        let child = pty
            .slave
            .spawn_command(cmd)
            .map_err(|e| format!("could not start {}: {e}", sh.title))?;
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
            shell: sh.title.clone(),
            cwd: cwd.display().to_string(),
            title: sh.title.clone(),
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
            #[cfg(windows)]
            job,
        });
        self.sessions
            .lock()
            .unwrap()
            .insert(id.clone(), session.clone());
        spawn_reader(session.clone(), reader, self.events.clone(), id.clone());
        spawn_waiter(session, child, self.events.clone(), id);
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
        let replay = s.replay.lock().unwrap().text();
        // A reattached renderer starts with nothing unacknowledged.
        s.flow.ack(u64::MAX);
        Ok(TerminalAttachResult {
            session: s.snapshot(),
            replay,
            seq: s.seq.load(Ordering::SeqCst),
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
        .map_err(|e| format!("resize: {e}"))
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
) {
    std::thread::spawn(move || {
        let mut buf = vec![0u8; CHUNK_MAX];
        let mut carry = Utf8Carry::default();
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
                let seq = s.seq.fetch_add(1, Ordering::SeqCst) + 1;
                s.replay.lock().unwrap().push(piece);
                s.flow.sent(utf16_units(piece));
                events.emit(
                    AppEvent::new("terminal")
                        .with("id", id.clone())
                        .with("seq", seq)
                        .with("data", piece),
                );
            }
            s.flow.wait_for_room(Duration::from_millis(250));
        }
    });
}

fn spawn_waiter(
    s: Arc<Session>,
    mut child: Box<dyn portable_pty::Child + Send + Sync>,
    events: Arc<dyn EventSink>,
    id: String,
) {
    std::thread::spawn(move || {
        let code = child.wait().map(|st| st.exit_code() as i64).unwrap_or(-1);
        s.running.store(false, Ordering::SeqCst);
        s.flow.close();
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
        .blocking("terminal_sessions", OpClass::Blocking, op_sessions);
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
