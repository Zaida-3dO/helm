//! Pausing the scheduled task that restarts a model server, while local models are unloaded.
//!
//! A model server is often kept alive by a Windows scheduled task that relaunches it whenever it
//! stops answering. Unloading would be undone a minute later, so Helm ends that task while the
//! lock is on and starts it again after. The task is named in Settings > Local models only: it
//! is not a setting agents may change, and it is empty until the user fills it in.
//!
//! `schtasks.exe` is run by its full path in the system folder, with the task name passed as one
//! argument (no shell ever sees it), no window, and a time limit. The service never runs it while
//! the test override is in effect; tests install a stand-in instead.

use std::path::PathBuf;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

/// How long one `schtasks` call may take before it is killed.
pub const TIMEOUT: Duration = Duration::from_secs(20);

/// Ending and starting the user's restart task. Each returns a sentence when it fails.
pub trait RestartTask: Send + Sync {
    fn end(&self, name: &str) -> Result<(), String>;
    fn run(&self, name: &str) -> Result<(), String>;
}

/// `schtasks.exe` in the system folder, never one found on `PATH`.
pub fn program() -> PathBuf {
    let root = std::env::var_os("SystemRoot").unwrap_or_else(|| "C:\\Windows".into());
    PathBuf::from(root).join("System32").join("schtasks.exe")
}

/// The arguments for `verb` (`/End` or `/Run`) on the task `name`, the name as one argument.
pub fn plan(verb: &str, name: &str) -> Vec<String> {
    vec![verb.to_owned(), "/TN".into(), name.to_owned()]
}

/// Wait for `child` to exit, killing it after `limit`.
pub(crate) fn wait_or_kill(mut child: Child, limit: Duration) -> Result<ExitStatus, String> {
    let until = Instant::now() + limit;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Ok(status),
            Ok(None) if Instant::now() >= until => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!(
                    "it did not finish within {} seconds",
                    limit.as_secs()
                ));
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(200)),
            Err(e) => return Err(e.to_string()),
        }
    }
}

/// A command that opens no window and whose output goes nowhere.
pub(crate) fn quiet(program: &std::path::Path) -> Command {
    let mut cmd = Command::new(program);
    cmd.stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    cmd
}

/// The real thing: `schtasks.exe /End` and `/Run`.
pub struct Schtasks;

impl Schtasks {
    fn call(&self, verb: &str, name: &str) -> Result<(), String> {
        if !cfg!(windows) {
            return Err("Scheduled tasks are only on Windows.".into());
        }
        let child = quiet(&program())
            .args(plan(verb, name))
            .spawn()
            .map_err(|e| format!("schtasks could not be run: {e}"))?;
        match wait_or_kill(child, TIMEOUT) {
            Ok(s) if s.success() => Ok(()),
            Ok(s) => Err(match s.code() {
                Some(c) => format!("schtasks exited with code {c}"),
                None => "schtasks did not finish normally".to_owned(),
            }),
            Err(e) => Err(format!("schtasks failed: {e}")),
        }
    }
}

impl RestartTask for Schtasks {
    fn end(&self, name: &str) -> Result<(), String> {
        self.call("/End", name)
    }
    fn run(&self, name: &str) -> Result<(), String> {
        self.call("/Run", name)
    }
}
