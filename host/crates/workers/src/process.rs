//! One worker process: `node <staged main.ts> <launch.json>` in
//! the chat's folder, inside its own [`Job`], with the record rules enforced on stdout.
//!
//! - **stdout** is read as JSON lines. A record that is not a JSON object, or that passes
//!   [`MAX_RECORD_BYTES`] before its newline, is a fault: the job is terminated and the run fails.
//!   A trailing `\r` and blank lines are tolerated.
//! - **stderr** is diagnostics only; the last [`STDERR_KEEP`] bytes are kept for failure messages.
//! - **stdin** takes one command per line; dropping the [`WorkerProcess`] closes it.

use crate::job::{self, Job};
use serde_json::{Map, Value};
use std::ffi::OsString;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::mpsc;

/// 96 MiB both ways.
pub const MAX_RECORD_BYTES: usize = 96 * 1024 * 1024;
/// stderr kept for failure messages.
pub const STDERR_KEEP: usize = 16 * 1024;

#[derive(Debug, Clone)]
pub struct SpawnSpec {
    pub node: PathBuf,
    pub script: PathBuf,
    pub launch: PathBuf,
    pub cwd: PathBuf,
    pub env: Vec<(OsString, OsString)>,
    /// Lower in tests to exercise the oversize rule cheaply.
    pub max_record_bytes: usize,
}

/// What the worker sent, or how it ended.
#[derive(Debug, Clone, PartialEq)]
pub enum WorkerOutput {
    Record(Map<String, Value>),
    /// A broken record: `bad_json`, `too_long` or `truncated`. The job has been terminated.
    Fault(String),
    /// The process ended. `stderr` is the kept tail.
    Exited {
        code: Option<i32>,
        stderr: String,
    },
}

/// The tail of stderr, bounded.
#[derive(Default)]
struct StderrTail {
    bytes: Vec<u8>,
}

impl StderrTail {
    fn push(&mut self, chunk: &[u8]) {
        self.bytes.extend_from_slice(chunk);
        if self.bytes.len() > STDERR_KEEP {
            let cut = self.bytes.len() - STDERR_KEEP;
            self.bytes.drain(..cut);
        }
    }
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.bytes).into_owned()
    }
}

pub struct WorkerProcess {
    pub pid: u32,
    stdin: mpsc::UnboundedSender<String>,
    job: Arc<Job>,
    stderr: Arc<Mutex<StderrTail>>,
}

impl WorkerProcess {
    /// Write one command line. Returns false when the worker's stdin is gone.
    pub fn send(&self, command: &Value) -> bool {
        self.stdin.send(command.to_string()).is_ok()
    }

    /// End the whole worker tree now.
    pub fn terminate(&self) {
        self.job.terminate();
    }

    pub fn stderr_tail(&self) -> String {
        self.stderr.lock().unwrap().text()
    }

    pub fn job(&self) -> Arc<Job> {
        self.job.clone()
    }
}

/// Start the worker. Outputs arrive on the returned channel, ending with exactly one `Exited`.
pub fn spawn(spec: SpawnSpec) -> std::io::Result<(WorkerProcess, mpsc::Receiver<WorkerOutput>)> {
    let mut cmd = tokio::process::Command::new(&spec.node);
    cmd.arg(&spec.script)
        .arg(&spec.launch)
        .current_dir(&spec.cwd)
        .env_clear()
        .envs(spec.env.iter().cloned())
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    job::prepare(&mut cmd);
    let mut child = cmd.spawn()?;
    let job = Arc::new(Job::new()?);
    if let Err(e) = job.assign(&child) {
        let _ = child.start_kill();
        return Err(e);
    }
    let pid = child.id().unwrap_or(0);
    let mut stdin = child.stdin.take().expect("stdin piped");
    let mut stdout = child.stdout.take().expect("stdout piped");
    let mut stderr = child.stderr.take().expect("stderr piped");
    let (out_tx, out_rx) = mpsc::channel::<WorkerOutput>(256);
    let (in_tx, mut in_rx) = mpsc::unbounded_channel::<String>();
    let tail = Arc::new(Mutex::new(StderrTail::default()));

    // stdin writer: one line per command; closing the channel closes stdin.
    tokio::spawn(async move {
        while let Some(line) = in_rx.recv().await {
            if stdin.write_all(line.as_bytes()).await.is_err()
                || stdin.write_all(b"\n").await.is_err()
                || stdin.flush().await.is_err()
            {
                break;
            }
        }
        let _ = stdin.shutdown().await;
    });

    // stderr: keep the tail.
    let tail2 = tail.clone();
    let stderr_done = tokio::spawn(async move {
        let mut buf = vec![0u8; 8192];
        loop {
            match stderr.read(&mut buf).await {
                Ok(0) | Err(_) => break,
                Ok(n) => tail2.lock().unwrap().push(&buf[..n]),
            }
        }
    });

    // stdout: records, then the exit.
    let job2 = job.clone();
    let tail3 = tail.clone();
    let limit = spec.max_record_bytes;
    tokio::spawn(async move {
        let fault = read_records(&mut stdout, limit, &out_tx).await;
        if let Some(f) = fault {
            job2.terminate();
            let _ = out_tx.send(WorkerOutput::Fault(f)).await;
        }
        let status = child.wait().await.ok();
        let _ = stderr_done.await;
        let stderr = tail3.lock().unwrap().text();
        let _ = out_tx
            .send(WorkerOutput::Exited {
                code: status.and_then(|s| s.code()),
                stderr,
            })
            .await;
    });

    Ok((
        WorkerProcess {
            pid,
            stdin: in_tx,
            job,
            stderr: tail,
        },
        out_rx,
    ))
}

/// Read JSON-object lines until EOF or a fault. Returns the fault, if any.
async fn read_records(
    stdout: &mut (impl AsyncReadExt + Unpin),
    limit: usize,
    out: &mpsc::Sender<WorkerOutput>,
) -> Option<String> {
    let mut pending: Vec<u8> = Vec::new();
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = match stdout.read(&mut buf).await {
            Ok(0) | Err(_) => {
                return if pending.iter().all(|b| b.is_ascii_whitespace()) {
                    None
                } else {
                    Some("truncated".into())
                };
            }
            Ok(n) => n,
        };
        let mut start = 0;
        for i in 0..n {
            if buf[i] == b'\n' {
                pending.extend_from_slice(&buf[start..i]);
                start = i + 1;
                let line = std::mem::take(&mut pending);
                if line.len() > limit {
                    return Some("too_long".into());
                }
                if let Some(f) = emit_line(&line, out).await {
                    return Some(f);
                }
            }
        }
        pending.extend_from_slice(&buf[start..n]);
        if pending.len() > limit {
            return Some("too_long".into());
        }
    }
}

async fn emit_line(line: &[u8], out: &mpsc::Sender<WorkerOutput>) -> Option<String> {
    let line = line.strip_suffix(b"\r").unwrap_or(line);
    if line.iter().all(|b| b.is_ascii_whitespace()) {
        return None;
    }
    match serde_json::from_slice::<Value>(line) {
        Ok(Value::Object(m)) => {
            let _ = out.send(WorkerOutput::Record(m)).await;
            None
        }
        _ => Some("bad_json".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node::{resolve_node, NodeLookup};
    use serde_json::json;
    use std::time::Duration;

    /// Run `script` (JavaScript) as a stand-in worker and collect everything it produced.
    async fn run_fake(script: &str, max: usize, input: Option<Value>) -> Vec<WorkerOutput> {
        let node = resolve_node(&NodeLookup::from_env()).expect("node installed");
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("w.js");
        std::fs::write(&file, script).unwrap();
        let launch = dir.path().join("launch.json");
        std::fs::write(&launch, "{}").unwrap();
        let (proc_, mut rx) = spawn(SpawnSpec {
            node: node.path,
            script: file,
            launch,
            cwd: dir.path().to_path_buf(),
            env: crate::env::EnvMap::from_process().into_pairs(),
            max_record_bytes: max,
        })
        .unwrap();
        if let Some(cmd) = input {
            assert!(proc_.send(&cmd));
        }
        let mut out = Vec::new();
        let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
        while let Ok(Some(o)) = tokio::time::timeout_at(deadline, rx.recv()).await {
            let done = matches!(o, WorkerOutput::Exited { .. });
            out.push(o);
            if done {
                break;
            }
        }
        out
    }

    #[tokio::test]
    async fn records_flow_and_commands_reach_stdin() {
        let out = run_fake(
            "process.stdin.on('data', d => { const c = JSON.parse(String(d)); \
             process.stdout.write(JSON.stringify({type:'response', id:c.id})+'\\r\\n\\n'); process.exit(0); });",
            MAX_RECORD_BYTES,
            Some(json!({"type": "get_state", "id": "ready"})),
        )
        .await;
        assert_eq!(out.len(), 2, "{out:?}");
        let WorkerOutput::Record(r) = &out[0] else {
            panic!("{out:?}")
        };
        assert_eq!(r["id"], "ready");
        assert!(matches!(out[1], WorkerOutput::Exited { code: Some(0), .. }));
    }

    #[tokio::test]
    async fn a_non_json_record_fails_the_run() {
        let out = run_fake(
            "process.stdout.write('{\"type\":\"ok\"}\\nhello there\\n'); setTimeout(()=>{}, 60000);",
            MAX_RECORD_BYTES,
            None,
        )
        .await;
        assert!(matches!(out[0], WorkerOutput::Record(_)));
        assert_eq!(out[1], WorkerOutput::Fault("bad_json".into()));
        assert!(
            matches!(out[2], WorkerOutput::Exited { .. }),
            "job killed it"
        );
    }

    #[tokio::test]
    async fn an_oversize_record_fails_the_run() {
        let out = run_fake(
            "process.stdout.write('{\"x\":\"' + 'a'.repeat(5000) + '\"}\\n'); setTimeout(()=>{}, 60000);",
            1024,
            None,
        )
        .await;
        assert_eq!(out[0], WorkerOutput::Fault("too_long".into()), "{out:?}");
        assert!(matches!(out[1], WorkerOutput::Exited { .. }));
    }

    #[tokio::test]
    async fn stderr_is_kept_to_its_tail() {
        let out = run_fake(
            "process.stderr.write('x'.repeat(40000) + 'THE-END'); process.exit(3);",
            MAX_RECORD_BYTES,
            None,
        )
        .await;
        let WorkerOutput::Exited { code, stderr } = out.last().unwrap() else {
            panic!("{out:?}")
        };
        assert_eq!(*code, Some(3));
        assert_eq!(stderr.len(), STDERR_KEEP);
        assert!(stderr.ends_with("THE-END"));
    }

    #[tokio::test]
    async fn a_line_cut_off_at_exit_is_truncated() {
        let out = run_fake(
            "process.stdout.write('{\"type\":\"half'); process.exit(0);",
            MAX_RECORD_BYTES,
            None,
        )
        .await;
        assert_eq!(out[0], WorkerOutput::Fault("truncated".into()));
    }
}
