//! Watching for a game from the user's list, so local models are unloaded while it runs.
//!
//! The list is empty by default, so nothing is watched until the user names a game. While it
//! holds a name and a server is set up, the process list is read every 15 seconds (a snapshot of
//! names only: no process is opened, and nothing about it is kept). The decision is
//! [`next`], a pure function of what the watch knows, so it is tested as a table:
//!
//! - the first sighting of a listed game sets a game lock (no wait: a false sighting costs only a
//!   reload);
//! - a manual lock is never changed by a game;
//! - a game lock lifts once no listed game has been seen for the delay set in Settings;
//! - turning models back on while a game runs is an override for that game until it closes, and
//!   only then is it watched again;
//! - a game lock with an empty list is lifted.

use crate::lock::LockState;

/// How often the process list is read while there is something to watch.
pub const POLL: std::time::Duration = std::time::Duration::from_secs(15);

/// The names of the processes running now, as the system reports them (`game.exe`).
pub trait ProcessNames: Send + Sync {
    fn names(&self) -> Vec<String>;
}

/// A process or list name in the form they are compared in: trimmed, lower case, ending `.exe`.
/// Linux reports names without the suffix, and a user may type one without it.
pub fn norm(name: &str) -> String {
    let n = name.trim().to_lowercase();
    if n.ends_with(".exe") {
        n
    } else {
        format!("{n}.exe")
    }
}

/// What the watch remembers between reads. Held in memory only.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Watch {
    /// When a listed game was last seen running, in ms.
    pub last_seen: Option<i64>,
    /// Games the user turned models back on during; they are not counted until they close.
    pub overridden: Vec<String>,
    /// The listed games running at the last read.
    pub running: Vec<String>,
}

impl Watch {
    /// Models were turned back on: the games running at the last read, and the one that set the
    /// lock, are not counted again until they close. A game that starts afterwards still counts.
    pub fn override_running(&mut self, lock: &LockState) {
        let extra = lock.game().map(norm);
        for g in self.running.iter().cloned().chain(extra) {
            if !self.overridden.contains(&g) {
                self.overridden.push(g);
            }
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Action {
    /// Set a game lock for this game.
    Lock(String),
    /// Lift the game lock.
    Lift,
    Nothing,
}

/// The decision after one read of the process list. `games` is the list from Settings, `seen`
/// the process names now, `now` in ms.
pub fn next(
    lock: &LockState,
    watch: &mut Watch,
    seen: &[String],
    now: i64,
    games: &[String],
    lift_minutes: u32,
) -> Action {
    if games.is_empty() {
        watch.overridden.clear();
        watch.running.clear();
        return if matches!(lock, LockState::Game { .. }) {
            Action::Lift
        } else {
            Action::Nothing
        };
    }
    let seen: Vec<String> = seen.iter().map(|s| norm(s)).collect();
    let running: Vec<String> = games
        .iter()
        .map(|g| norm(g))
        .filter(|g| seen.contains(g))
        .collect();
    // An override ends when its game closes.
    watch.overridden.retain(|g| seen.contains(g));
    let counted: Vec<String> = running
        .iter()
        .filter(|g| !watch.overridden.contains(g))
        .cloned()
        .collect();
    watch.running = running;
    if let Some(first) = counted.first() {
        watch.last_seen = Some(now);
        return match lock {
            LockState::On => Action::Lock(first.clone()),
            _ => Action::Nothing,
        };
    }
    match lock {
        LockState::Game { .. } => {
            let lift_ms = i64::from(lift_minutes) * 60_000;
            if watch.last_seen.is_none_or(|t| now - t >= lift_ms) {
                watch.last_seen = None;
                Action::Lift
            } else {
                Action::Nothing
            }
        }
        _ => Action::Nothing,
    }
}

/// The process list of this machine.
pub struct SystemProcesses;

#[cfg(windows)]
impl ProcessNames for SystemProcesses {
    fn names(&self) -> Vec<String> {
        use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
        use windows_sys::Win32::System::Diagnostics::ToolHelp::{
            CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W,
            TH32CS_SNAPPROCESS,
        };
        let mut out = Vec::new();
        // SAFETY: the snapshot handle is checked before use and closed once; the entry is a
        // plain struct sized as the API asks, and its name is read up to its terminator.
        unsafe {
            let snap = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
            if snap == INVALID_HANDLE_VALUE {
                return out;
            }
            let mut e: PROCESSENTRY32W = std::mem::zeroed();
            e.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;
            let mut more = Process32FirstW(snap, &mut e) != 0;
            while more {
                let len = e
                    .szExeFile
                    .iter()
                    .position(|&c| c == 0)
                    .unwrap_or(e.szExeFile.len());
                out.push(String::from_utf16_lossy(&e.szExeFile[..len]));
                more = Process32NextW(snap, &mut e) != 0;
            }
            CloseHandle(snap);
        }
        out
    }
}

/// On Linux the system cuts a process name to 15 characters, so a listed game with a longer name
/// never matches.
#[cfg(not(windows))]
impl ProcessNames for SystemProcesses {
    fn names(&self) -> Vec<String> {
        let Ok(dir) = std::fs::read_dir("/proc") else {
            return Vec::new();
        };
        dir.filter_map(|e| e.ok())
            .filter(|e| {
                e.file_name()
                    .to_string_lossy()
                    .bytes()
                    .all(|b| b.is_ascii_digit())
            })
            .filter_map(|e| std::fs::read_to_string(e.path().join("comm")).ok())
            .map(|s| s.trim().to_owned())
            .filter(|s| !s.is_empty())
            .collect()
    }
}
