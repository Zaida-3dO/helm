//! Sleep, wake, sign-out and shutdown, as the app answers them.
//!
//! Sleep and wake go to the chats' outage watch, which notes the chats that were working and tells
//! their transcripts how long the computer was away. A sign-out or shutdown runs the same quit
//! sequence as quitting from the menu, without the question: the system has already decided.
//! Chats that were working are noted for a continuation, so they pick up on the next start.

use helm_chats::outage::OutageWatch;
use helm_core::db::Database;
use helm_core::lifecycle::{AppControl, QuitCoordinator, QuitOutcome, WorkerDrain};
use helm_ipc::EventSink;
use helm_platform::session::SystemWatcher;
use std::sync::Arc;
use std::time::Duration;

pub struct AppSystem {
    pub outages: Arc<OutageWatch>,
    pub quit: Arc<QuitCoordinator>,
    pub events: Arc<dyn EventSink>,
    pub workers: Arc<dyn WorkerDrain>,
    pub db: Arc<Database>,
    pub shell: Arc<dyn AppControl>,
}

impl SystemWatcher for AppSystem {
    fn suspending(&self) {
        self.outages.suspending();
    }

    fn resumed(&self) {
        self.outages.resumed();
    }

    fn session_ending(&self, budget: Duration) {
        log::info!("the session is ending; running the quit sequence");
        let (done, wait) = std::sync::mpsc::channel();
        let quit = self.quit.clone();
        let events = self.events.clone();
        let workers = self.workers.clone();
        let db = self.db.clone();
        let shell = self.shell.clone();
        tauri::async_runtime::spawn(async move {
            let outcome = quit.run_for_session_end(events, workers, Some(db)).await;
            log::info!("session end: {outcome:?}");
            if matches!(outcome, QuitOutcome::Quit { .. }) {
                shell.exit(0);
            }
            let _ = done.send(());
        });
        // The system stops waiting after a few seconds; what is not done by then is picked up by
        // the continuation notes already written.
        if wait.recv_timeout(budget).is_err() {
            log::warn!("the quit sequence outlasted the time the system gives a sign-out");
        }
    }
}
