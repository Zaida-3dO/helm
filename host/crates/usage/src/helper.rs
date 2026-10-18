//! Starting `helpers/usage`: `node <script>`, one JSON job on stdin, one JSON line back, killed
//! after a deadline. The helper environment is the worker's (registry environment on Windows) with
//! nothing added, so the claude binary it starts sees the user's own login and nothing else.

use crate::service::{parse_answer, ControlAnswer, PlanReader};
use helm_workers::launch::{test_claude, ClaudeRuntime};
use helm_workers::node::{resolve_node, NodeLookup};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// The helper's own deadline for the binary's answer, and the host's, a little longer.
const HELPER_TIMEOUT_MS: u64 = 60_000;
const HOST_TIMEOUT: Duration = Duration::from_secs(75);

pub struct ProcessReader {
    /// `<repo>/helpers/usage/src/main.ts`; `None` when the helper is not on disk.
    script: Option<PathBuf>,
    profile_dir: PathBuf,
    debug: bool,
}

impl ProcessReader {
    pub fn new(profile_dir: PathBuf, debug: bool) -> Self {
        Self {
            script: helper_script(debug),
            profile_dir,
            debug,
        }
    }
}

/// Where the helper lives. Debug builds read the repository (the way titles do); a release
/// build looks beside the staged worker tree in the app's resources.
pub fn helper_script(debug: bool) -> Option<PathBuf> {
    // The installer puts the resources beside the executable: that folder is the resource folder.
    let beside_exe = if debug {
        None
    } else {
        std::env::current_exe()
            .ok()
            .and_then(|e| e.parent().map(Path::to_path_buf))
    };
    script_beside(&helm_workers::stage::default_source(
        debug,
        beside_exe.as_deref(),
    )?)
}

/// `<resources>/helpers/usage/src/main.ts` for a worker source at `<resources>/worker/src`.
fn script_beside(worker_src: &Path) -> Option<PathBuf> {
    let script = worker_src
        .parent()?
        .parent()?
        .join("helpers/usage/src/main.ts");
    script.is_file().then_some(script)
}

fn failed(reason: &str, message: impl Into<String>) -> ControlAnswer {
    ControlAnswer::Failed {
        reason: reason.into(),
        message: message.into(),
        version: None,
    }
}

impl ProcessReader {
    /// Run one helper job (node, the script, one JSON line back).
    pub fn run_job(&self, job: &Value, timeout: Duration) -> Result<Value, String> {
        let Some(script) = &self.script else {
            return Err("Helm could not find its usage helper.".into());
        };
        let env = helm_workers::env::base_environment();
        let node = resolve_node(&NodeLookup::for_worker(&env)).map_err(|e| e.to_string())?;
        run_helper(&node.path, script, env.into_pairs(), job, timeout)
    }
}

impl PlanReader for ProcessReader {
    fn read(&self) -> ControlAnswer {
        let mut job = json!({ "kind": "plan", "timeoutMs": HELPER_TIMEOUT_MS });
        let (runtime, _) = ClaudeRuntime::load(&self.profile_dir);
        if let Some(t) = test_claude(
            self.debug,
            std::env::var("HELM_TEST_CLAUDE").ok().as_deref(),
        ) {
            job["command"] = Value::String(t.command);
            job["commandArgs"] = json!(t.args);
        } else if !runtime.executable.is_empty() {
            job["command"] = Value::String(runtime.executable);
        }
        match self.run_job(&job, HOST_TIMEOUT) {
            Ok(v) => parse_answer(&v),
            Err(e) => failed("error", e),
        }
    }
}

/// Start `node <script>`, write the job to stdin, read one JSON line back.
fn run_helper(
    node: &Path,
    script: &Path,
    env: Vec<(std::ffi::OsString, std::ffi::OsString)>,
    job: &Value,
    timeout: Duration,
) -> Result<Value, String> {
    use std::io::{Read, Write};
    let mut cmd = std::process::Command::new(node);
    cmd.arg(script)
        .env_clear()
        .envs(env)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    let mut child = cmd
        .spawn()
        .map_err(|e| format!("the usage helper did not start: {e}"))?;
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(job.to_string().as_bytes());
    }
    let mut stdout = child.stdout.take().ok_or("no helper output")?;
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut s = String::new();
        let _ = stdout.read_to_string(&mut s);
        let _ = tx.send(s);
    });
    let out = match rx.recv_timeout(timeout) {
        Ok(s) => s,
        Err(_) => {
            let _ = child.kill();
            return Err("the usage helper timed out".into());
        }
    };
    let _ = child.wait();
    serde_json::from_str(out.trim())
        .map_err(|_| "the usage helper's answer was not JSON".to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_helper_is_found_beside_the_bundled_worker_source() {
        let res = tempfile::tempdir().unwrap();
        let src = res.path().join("worker").join("src");
        std::fs::create_dir_all(&src).unwrap();
        assert_eq!(script_beside(&src), None, "no helper bundled yet");
        let main = res.path().join("helpers/usage/src/main.ts");
        std::fs::create_dir_all(main.parent().unwrap()).unwrap();
        std::fs::write(&main, "//").unwrap();
        assert_eq!(script_beside(&src), Some(main));
        // The folder the app reports is found through the same path.
        let found = helm_workers::stage::default_source(false, Some(res.path())).unwrap();
        assert!(script_beside(&found).is_some());
    }
}
