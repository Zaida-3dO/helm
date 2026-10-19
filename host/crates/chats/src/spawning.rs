//! Starting a worker off the actor's task.
//!
//! [`Spawner::spawn`] stages the worker, reads the registry environment, resolves Node, writes
//! `launch.json` and starts the process: tens of milliseconds of blocking work that used to run on
//! the actor's task. [`spawn_off_actor`] runs it on the blocking pool and hands the actor a
//! [`PendingLink`] and an output channel at once:
//!
//! - lines sent before the process exists are queued and written in order once it does, and a
//!   `terminate` asked for meanwhile is applied as soon as it does;
//! - the real worker's output is forwarded to the actor's channel unchanged;
//! - a start that fails arrives as [`WorkerOutput::SpawnFailed`], and nothing was written to any
//!   process.

use crate::runtime::{Shared, SpawnRequest, Spawned, WorkerLink};
use helm_workers::WorkerOutput;
use serde_json::Value;
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc;

enum State {
    Starting { queued: Vec<Value>, terminate: bool },
    Running(Arc<dyn WorkerLink>),
    Failed,
}

/// The actor's end of a worker that may still be starting.
pub(crate) struct PendingLink {
    state: Mutex<State>,
}

impl PendingLink {
    fn new() -> Self {
        Self {
            state: Mutex::new(State::Starting {
                queued: Vec::new(),
                terminate: false,
            }),
        }
    }

    /// The process is up: write what was queued, in order, then pass everything straight through.
    fn running(&self, link: Arc<dyn WorkerLink>) {
        let mut state = self.state.lock().unwrap();
        if let State::Starting { queued, terminate } = &mut *state {
            for line in queued.drain(..) {
                link.send(&line);
            }
            if *terminate {
                link.terminate();
            }
        }
        *state = State::Running(link);
    }

    fn failed(&self) {
        *self.state.lock().unwrap() = State::Failed;
    }
}

impl WorkerLink for PendingLink {
    fn send(&self, command: &Value) -> bool {
        match &mut *self.state.lock().unwrap() {
            State::Starting { queued, .. } => {
                queued.push(command.clone());
                true
            }
            State::Running(link) => link.send(command),
            State::Failed => false,
        }
    }

    fn terminate(&self) {
        match &mut *self.state.lock().unwrap() {
            State::Starting { terminate, .. } => *terminate = true,
            State::Running(link) => link.terminate(),
            State::Failed => {}
        }
    }
}

/// Start the worker on the blocking pool; the link and output channel are usable at once.
pub(crate) fn spawn_off_actor(sh: &Shared, req: SpawnRequest) -> Spawned {
    let pending = Arc::new(PendingLink::new());
    let (tx, rx) = mpsc::channel(256);
    let spawner = sh.spawner.clone();
    let link = pending.clone();
    sh.handle.spawn(async move {
        match tokio::task::spawn_blocking(move || spawner.spawn(&req)).await {
            Ok(Ok((real, mut out))) => {
                link.running(real);
                while let Some(o) = out.recv().await {
                    if tx.send(o).await.is_err() {
                        break; // the actor is gone; dropping the link closes the worker's stdin
                    }
                }
            }
            Ok(Err(e)) => {
                link.failed();
                let _ = tx.send(WorkerOutput::SpawnFailed(e)).await;
            }
            Err(e) => {
                link.failed();
                let _ = tx
                    .send(WorkerOutput::SpawnFailed(format!(
                        "Helm could not start the worker: {e}"
                    )))
                    .await;
            }
        }
    });
    (pending, rx)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[derive(Default)]
    struct Recorder {
        lines: Mutex<Vec<Value>>,
        terminated: Mutex<bool>,
    }

    impl WorkerLink for Recorder {
        fn send(&self, command: &Value) -> bool {
            self.lines.lock().unwrap().push(command.clone());
            true
        }
        fn terminate(&self) {
            *self.terminated.lock().unwrap() = true;
        }
    }

    #[test]
    fn lines_sent_while_starting_are_written_in_order_then_pass_through() {
        let p = PendingLink::new();
        assert!(p.send(&json!({"n": 1})));
        assert!(p.send(&json!({"n": 2})));
        let real = Arc::new(Recorder::default());
        p.running(real.clone());
        assert!(p.send(&json!({"n": 3})));
        let ns: Vec<i64> = real
            .lines
            .lock()
            .unwrap()
            .iter()
            .map(|v| v["n"].as_i64().unwrap())
            .collect();
        assert_eq!(ns, [1, 2, 3]);
        assert!(!*real.terminated.lock().unwrap());
    }

    #[test]
    fn a_terminate_while_starting_is_applied_once_running_and_a_failed_start_takes_nothing() {
        let p = PendingLink::new();
        p.terminate();
        let real = Arc::new(Recorder::default());
        p.running(real.clone());
        assert!(*real.terminated.lock().unwrap());
        let f = PendingLink::new();
        f.failed();
        assert!(
            !f.send(&json!({})),
            "nothing can be written after a failed start"
        );
    }
}
