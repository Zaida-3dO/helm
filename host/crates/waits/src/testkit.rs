//! Test doubles shared by the crate's tests.

use crate::model::{ChatObs, FileObs, PrObs, ProcessObs};
use crate::probes::Probes;
use crate::service::{Deliver, Delivery};
use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;

#[derive(Default)]
pub struct Fake {
    pub chats: Mutex<HashMap<String, ChatObs>>,
    pub gone: Mutex<Vec<u32>>,
    pub polls: AtomicUsize,
    /// What a pull request read returns; `None` is a failed read.
    pub pr_ok: Mutex<Option<PrObs>>,
}

impl Probes for Fake {
    fn chat(&self, id: &str) -> Option<ChatObs> {
        self.chats.lock().unwrap().get(id).cloned()
    }
    fn process(&self, pid: u32) -> ProcessObs {
        if self.gone.lock().unwrap().contains(&pid) {
            ProcessObs::Gone(Some(0))
        } else {
            ProcessObs::Alive
        }
    }
    fn file(&self, _: &str) -> FileObs {
        FileObs {
            exists: false,
            len: 0,
            modified_ms: 0,
        }
    }
    fn pr(&self, _: &str) -> Result<PrObs, String> {
        self.polls.fetch_add(1, Ordering::SeqCst);
        self.pr_ok
            .lock()
            .unwrap()
            .clone()
            .ok_or_else(|| "offline".to_owned())
    }
}

#[derive(Default)]
pub struct Sink {
    pub sent: Mutex<Vec<(String, String, String)>>,
    pub fail_first: AtomicUsize,
}

impl Deliver for Sink {
    fn deliver(&self, chat: &str, sid: &str, text: &str) -> Delivery {
        if self.fail_first.load(Ordering::SeqCst) > 0 {
            self.fail_first.fetch_sub(1, Ordering::SeqCst);
            return Delivery::Retry;
        }
        self.sent
            .lock()
            .unwrap()
            .push((chat.into(), sid.into(), text.into()));
        Delivery::Sent
    }
}
