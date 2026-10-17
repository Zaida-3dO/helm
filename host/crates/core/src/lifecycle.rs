//! Quit.
//!
//! A quit request is deferred behind a native confirmation listing running work. On confirm:
//! emit `quitting` and wait up to 2 s for the UI to report drafts and settings saved; drain workers
//! for up to 8 s; checkpoint the WAL; then the app exits. Closing the main window only hides it.

use crate::db::Database;
use helm_ipc::{AppEvent, EventSink};
use helm_platform::Dialogs;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::Notify;

pub const DRAFT_FLUSH_TIMEOUT: Duration = Duration::from_secs(2);
pub const WORKER_DRAIN_TIMEOUT: Duration = Duration::from_secs(8);

/// The worker side of shutdown. [`NoWorkers`] stands in when there are none.
pub trait WorkerDrain: Send + Sync {
    /// Human-readable lines for the confirmation, one per running chat.
    fn running_work(&self) -> Vec<String>;
    /// Send every worker its shutdown line and wait for them to exit.
    fn drain(&self) -> Pin<Box<dyn Future<Output = ()> + Send + '_>>;
}

pub struct NoWorkers;
impl WorkerDrain for NoWorkers {
    fn running_work(&self) -> Vec<String> {
        Vec::new()
    }
    fn drain(&self) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
        Box::pin(async {})
    }
}

/// What the host can ask of the app shell. Implemented by the Tauri app.
pub trait AppControl: Send + Sync {
    fn show_main(&self);
    fn hide_main(&self);
    fn exit(&self, code: i32);
}

/// Service wrappers, so trait objects can sit in [`helm_ipc::Services`].
pub struct Workers(pub Arc<dyn WorkerDrain>);
pub struct Shell(pub Arc<dyn AppControl>);
pub struct PlatformDialogs(pub Arc<dyn Dialogs>);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuitOutcome {
    /// The user cancelled; the app stays as it was.
    Cancelled,
    /// Everything is saved and drained; exit now.
    Quit { ui_saved: bool, drained: bool },
    /// A quit is already in progress.
    Busy,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Phase {
    Idle,
    Confirming,
    Quitting,
}

pub struct QuitCoordinator {
    phase: Mutex<Phase>,
    saved: Mutex<Option<Arc<Notify>>>,
}

impl Default for QuitCoordinator {
    fn default() -> Self {
        Self::new()
    }
}

impl QuitCoordinator {
    pub fn new() -> Self {
        Self {
            phase: Mutex::new(Phase::Idle),
            saved: Mutex::new(None),
        }
    }

    /// Run the whole quit sequence. The caller exits the process on [`QuitOutcome::Quit`].
    pub async fn run(
        &self,
        dialogs: Arc<dyn Dialogs>,
        events: Arc<dyn EventSink>,
        workers: Arc<dyn WorkerDrain>,
        db: Option<Arc<Database>>,
    ) -> QuitOutcome {
        {
            let mut phase = self.phase.lock().unwrap();
            if *phase != Phase::Idle {
                return QuitOutcome::Busy;
            }
            *phase = Phase::Confirming;
        }
        let running = workers.running_work();
        let message = if running.is_empty() {
            "Quit Helm?".to_owned()
        } else {
            format!(
                "Quit Helm? This work is still running and will be stopped:\n\n{}",
                running.join("\n")
            )
        };
        let confirmed = tokio::task::spawn_blocking(move || dialogs.confirm("Quit Helm", &message))
            .await
            .unwrap_or(false);
        if !confirmed {
            *self.phase.lock().unwrap() = Phase::Idle;
            return QuitOutcome::Cancelled;
        }

        let notify = Arc::new(Notify::new());
        *self.saved.lock().unwrap() = Some(notify.clone());
        *self.phase.lock().unwrap() = Phase::Quitting;
        events.emit(AppEvent::new("quitting"));
        let ui_saved = tokio::time::timeout(DRAFT_FLUSH_TIMEOUT, notify.notified())
            .await
            .is_ok();
        let drained = tokio::time::timeout(WORKER_DRAIN_TIMEOUT, workers.drain())
            .await
            .is_ok();
        if let Some(db) = db {
            let _ = tokio::task::spawn_blocking(move || {
                if let Err(e) = db.checkpoint() {
                    log::error!("WAL checkpoint at quit failed: {e}");
                }
            })
            .await;
        }
        QuitOutcome::Quit { ui_saved, drained }
    }

    /// The UI reports drafts and settings saved (the `quit_ready` op).
    pub fn ui_saved(&self) {
        if let Some(n) = self.saved.lock().unwrap().as_ref() {
            n.notify_one();
        }
    }

    pub fn is_quitting(&self) -> bool {
        *self.phase.lock().unwrap() == Phase::Quitting
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use helm_ipc::RecordingSink;
    use helm_platform::dialogs::FixedDialogs;
    use std::sync::atomic::{AtomicBool, Ordering};

    struct SlowWorkers(Duration, AtomicBool);
    impl WorkerDrain for SlowWorkers {
        fn running_work(&self) -> Vec<String> {
            vec!["chat one".into()]
        }
        fn drain(&self) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
            Box::pin(async move {
                tokio::time::sleep(self.0).await;
                self.1.store(true, Ordering::SeqCst);
            })
        }
    }

    #[tokio::test(start_paused = true)]
    async fn cancel_changes_nothing() {
        let q = QuitCoordinator::new();
        let sink = Arc::new(RecordingSink::default());
        let out = q
            .run(
                Arc::new(FixedDialogs(false)),
                sink.clone(),
                Arc::new(NoWorkers),
                None,
            )
            .await;
        assert_eq!(out, QuitOutcome::Cancelled);
        assert!(sink.kinds().is_empty(), "no quitting event after cancel");
        assert!(!q.is_quitting());
    }

    #[tokio::test(start_paused = true)]
    async fn waits_for_the_ui_at_most_two_seconds_and_workers_at_most_eight() {
        let q = QuitCoordinator::new();
        let sink = Arc::new(RecordingSink::default());
        let workers = Arc::new(SlowWorkers(Duration::from_secs(60), AtomicBool::new(false)));
        let start = tokio::time::Instant::now();
        let out = q
            .run(
                Arc::new(FixedDialogs(true)),
                sink.clone(),
                workers.clone(),
                None,
            )
            .await;
        assert_eq!(
            out,
            QuitOutcome::Quit {
                ui_saved: false,
                drained: false
            }
        );
        let took = start.elapsed();
        assert!(
            took >= DRAFT_FLUSH_TIMEOUT + WORKER_DRAIN_TIMEOUT,
            "{took:?}"
        );
        assert!(took < DRAFT_FLUSH_TIMEOUT + WORKER_DRAIN_TIMEOUT + Duration::from_secs(1));
        assert_eq!(sink.kinds(), ["quitting"]);
    }

    #[tokio::test(start_paused = true)]
    async fn a_prompt_ui_and_quick_workers_quit_immediately() {
        let q = Arc::new(QuitCoordinator::new());
        let sink = Arc::new(RecordingSink::default());
        let workers = Arc::new(SlowWorkers(
            Duration::from_millis(10),
            AtomicBool::new(false),
        ));
        let q2 = q.clone();
        let acker = tokio::spawn(async move {
            while !q2.is_quitting() {
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
            q2.ui_saved();
        });
        let start = tokio::time::Instant::now();
        let out = q
            .run(Arc::new(FixedDialogs(true)), sink, workers.clone(), None)
            .await;
        acker.await.unwrap();
        assert_eq!(
            out,
            QuitOutcome::Quit {
                ui_saved: true,
                drained: true
            }
        );
        assert!(workers.1.load(Ordering::SeqCst));
        assert!(start.elapsed() < Duration::from_secs(1));
    }

    #[tokio::test]
    async fn the_wal_is_checkpointed_last() {
        let (_d, db) = crate::db::tests::temp_db(&crate::db::MigrationRegistry::new());
        db.write(|tx| tx.execute("INSERT INTO counters(key, value) VALUES ('x', 1)", []))
            .unwrap();
        let wal = std::path::PathBuf::from(format!("{}-wal", db.path().display()));
        assert!(std::fs::metadata(&wal).unwrap().len() > 0);
        // Keep a handle open so closing the last connection cannot be what empties the WAL.
        let db = Arc::new(db);
        let q = QuitCoordinator::new();
        let q_ref = &q;
        let ack = async {
            while !q_ref.is_quitting() {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
            q_ref.ui_saved();
        };
        let run = q.run(
            Arc::new(FixedDialogs(true)),
            Arc::new(RecordingSink::default()),
            Arc::new(NoWorkers),
            Some(db.clone()),
        );
        let (out, ()) = tokio::join!(run, ack);
        assert!(matches!(out, QuitOutcome::Quit { .. }));
        assert_eq!(std::fs::metadata(&wal).unwrap().len(), 0);
    }

    #[tokio::test(start_paused = true)]
    async fn a_second_request_while_quitting_is_busy() {
        let q = Arc::new(QuitCoordinator::new());
        let q2 = q.clone();
        let first = tokio::spawn(async move {
            q2.run(
                Arc::new(FixedDialogs(true)),
                Arc::new(RecordingSink::default()),
                Arc::new(NoWorkers),
                None,
            )
            .await
        });
        while !q.is_quitting() {
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
        let second = q
            .run(
                Arc::new(FixedDialogs(true)),
                Arc::new(RecordingSink::default()),
                Arc::new(NoWorkers),
                None,
            )
            .await;
        assert_eq!(second, QuitOutcome::Busy);
        q.ui_saved();
        assert!(matches!(first.await.unwrap(), QuitOutcome::Quit { .. }));
    }
}
