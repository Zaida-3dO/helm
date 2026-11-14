//! The graphics-card lock: while it is on, nothing in Helm loads a local model.
//!
//! The user turns it on with "Unload local models" (a **manual** lock), or a game from the list in
//! Settings turns it on while it runs (a **game** lock). It is one row in this crate's own
//! `local_model_lock` table, so it outlives a restart of Helm: no row means local models are on.
//! The service keeps a copy in memory, so every check before a load is a memory read.
//!
//! A manual lock beats a game lock: a game that starts while the user has unloaded by hand changes
//! nothing, and a game that closes lifts only the lock it set.

use rusqlite::{OptionalExtension, Transaction};
use serde::{Deserialize, Serialize};

/// What the lock is now.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LockState {
    /// No lock: local models load as usual.
    On,
    /// The user unloaded local models; only they turn them back on.
    Manual { since: i64 },
    /// A game on the list is running; local models come back a few minutes after it closes.
    Game { since: i64, game: String },
}

impl LockState {
    pub fn is_locked(&self) -> bool {
        !matches!(self, Self::On)
    }

    /// When the lock was set, in ms since the epoch.
    pub fn since(&self) -> Option<i64> {
        match self {
            Self::On => None,
            Self::Manual { since } | Self::Game { since, .. } => Some(*since),
        }
    }

    pub fn game(&self) -> Option<&str> {
        match self {
            Self::Game { game, .. } => Some(game),
            _ => None,
        }
    }

    /// The sentence every refused load gets, or `None` when nothing is refused. `lift_minutes` is
    /// how long after a game closes its lock lifts.
    pub fn refusal(&self, lift_minutes: u32) -> Option<String> {
        match self {
            Self::On => None,
            Self::Manual { .. } => Some(MANUAL_TEXT.to_owned()),
            Self::Game { game, .. } => Some(format!(
                "GPU reserved while {game} is running: local models are unloaded. They come back {lift_minutes} minute{} after it closes, or turn them back on from the top bar.",
                if lift_minutes == 1 { "" } else { "s" }
            )),
        }
    }
}

/// What a load gets while the user has unloaded local models by hand.
pub const MANUAL_TEXT: &str =
    "GPU reserved: local models are unloaded. Turn them back on from the top bar to use this model.";

/// The line a chat gets in its transcript when Unload stopped it.
pub const STOPPED_TEXT: &str = "Stopped: local models were unloaded to free the graphics card.";

/// What the pause file says, for whoever opens it.
pub const PAUSE_TEXT: &str =
    "Paused by Helm: local models are unloaded. Delete this file to allow restarts.\n";

/// The stored form: `{mode, since, game?}`.
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Stored {
    mode: String,
    since: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    game: Option<String>,
}

pub fn init(tx: &Transaction<'_>) -> rusqlite::Result<()> {
    tx.execute_batch(
        "CREATE TABLE IF NOT EXISTS local_model_lock (
             id     INTEGER PRIMARY KEY CHECK (id = 1),
             value  TEXT NOT NULL
         );",
    )
}

/// The stored lock. A row that can't be read counts as a manual lock: when in doubt, the card
/// stays reserved and the user can turn models back on.
pub fn load(tx: &Transaction<'_>) -> rusqlite::Result<LockState> {
    let raw: Option<String> = tx
        .query_row("SELECT value FROM local_model_lock WHERE id = 1", [], |r| {
            r.get(0)
        })
        .optional()?;
    let Some(raw) = raw else {
        return Ok(LockState::On);
    };
    Ok(match serde_json::from_str::<Stored>(&raw) {
        Ok(Stored {
            mode,
            since,
            game: Some(game),
        }) if mode == "game" && !game.is_empty() => LockState::Game { since, game },
        Ok(Stored { since, .. }) => LockState::Manual { since },
        Err(_) => LockState::Manual { since: 0 },
    })
}

pub fn store(tx: &Transaction<'_>, state: &LockState) -> rusqlite::Result<()> {
    let stored = match state {
        LockState::On => {
            tx.execute("DELETE FROM local_model_lock WHERE id = 1", [])?;
            return Ok(());
        }
        LockState::Manual { since } => Stored {
            mode: "manual".into(),
            since: *since,
            game: None,
        },
        LockState::Game { since, game } => Stored {
            mode: "game".into(),
            since: *since,
            game: Some(game.clone()),
        },
    };
    let v = serde_json::to_string(&stored).expect("the lock serialises");
    tx.execute(
        "INSERT INTO local_model_lock (id, value) VALUES (1, ?1)
         ON CONFLICT(id) DO UPDATE SET value = excluded.value",
        [v],
    )?;
    Ok(())
}
