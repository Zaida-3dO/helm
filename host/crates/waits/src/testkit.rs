//! Test doubles shared by the crate's tests.

use crate::model::{ChatObs, FileObs, PrObs, ProcessObs};
use crate::probes::Probes;
use crate::service::{Deliver, Delivery};
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Mutex;

#[derive(Default)]
pub struct Fake {
    pub chats: Mutex<HashMap<String, ChatObs>>,
    pub gone: Mutex<Vec<u32>>,
    pub polls: AtomicUsize,
    /// Make every chat read fail, as a database error would.
    pub fail_chat: std::sync::atomic::AtomicBool,
    /// What a pull request read returns; `None` is a failed read.
    pub pr_ok: Mutex<Option<PrObs>>,
    /// The files that exist; any other path is a missing file in a folder that can be read.
    pub files: Mutex<HashMap<String, FileObs>>,
    /// The paths that count as being on a network share.
    pub shares: Mutex<HashSet<String>>,
    /// Make every file look fail, as a share that is down would.
    pub unreachable: AtomicBool,
    /// How many times a file was looked at.
    pub file_reads: AtomicUsize,
}

impl Probes for Fake {
    fn chat(&self, id: &str) -> Result<Option<ChatObs>, String> {
        if self.fail_chat.load(Ordering::SeqCst) {
            return Err("database is busy".into());
        }
        Ok(self.chats.lock().unwrap().get(id).cloned())
    }
    fn process(&self, pid: u32) -> ProcessObs {
        if self.gone.lock().unwrap().contains(&pid) {
            ProcessObs::Gone(Some(0))
        } else {
            ProcessObs::Alive(Some(1))
        }
    }
    fn file(&self, path: &str) -> Option<FileObs> {
        self.file_reads.fetch_add(1, Ordering::SeqCst);
        if self.unreachable.load(Ordering::SeqCst) {
            return None;
        }
        Some(
            self.files
                .lock()
                .unwrap()
                .get(path)
                .cloned()
                .unwrap_or(FileObs {
                    exists: false,
                    len: 0,
                    modified_ms: 0,
                }),
        )
    }
    fn on_share(&self, path: &str) -> bool {
        self.shares.lock().unwrap().contains(path)
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
