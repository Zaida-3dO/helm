//! Filling in item titles nobody gave.
//!
//! An agent passes an item's title when it links a chat, and a board project's title comes from
//! the board mirror. An item with neither shows only its first characters, so the app looks those
//! up on the task board in the background: [`fill`] runs one round, [`start`] runs a round every
//! [`ROUND_GAP`] on a thread of its own. What asks the board is the app's [`TitleSource`]; this
//! crate only decides which items to ask about and stores what comes back.

use crate::store;
use helm_core::db::Database;
use helm_ipc::{AppEvent, EventSink};
use serde_json::json;
use std::sync::Arc;
use std::time::Duration;

/// How many items one round looks up.
pub const PER_ROUND: usize = 10;
/// The time between two rounds.
pub const ROUND_GAP: Duration = Duration::from_secs(60);

/// The board could not be asked now (not connected, not reachable, signed out).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Unavailable;

/// Where a missing title is looked up: the task board, in the app.
pub trait TitleSource: Send + Sync {
    /// The item's title as the board has it. `Ok(None)` when the board has no such item or it has
    /// no title; `Err` when the board cannot be asked now, so the item is asked about again at the
    /// next round.
    fn title(&self, item_id: &str) -> Result<Option<String>, Unavailable>;
}

/// One round: claim the items that need a title, ask the source about each, store what it says.
/// The ids that got a title come back, so the caller can tell the UI to re-read them.
pub fn fill(db: &Database, source: &dyn TitleSource, now: i64) -> Vec<String> {
    let claimed = match db.write(|tx| store::claim_untitled(tx, now, PER_ROUND)) {
        Ok(c) => c,
        Err(e) => {
            log::warn!("item titles: the links could not be read: {e}");
            return Vec::new();
        }
    };
    let mut filled = Vec::new();
    for (i, id) in claimed.iter().enumerate() {
        match source.title(id) {
            Ok(Some(title)) => match db.write(|tx| store::set_title(tx, id, &title)) {
                Ok(()) => filled.push(id.clone()),
                Err(e) => log::warn!("item titles: a title could not be stored: {e}"),
            },
            Ok(None) => {}
            Err(Unavailable) => {
                // Nothing more can be asked this round; the rest wait for the next one, untried.
                if let Err(e) = db.write(|tx| store::unclaim(tx, &claimed[i..])) {
                    log::warn!("item titles: the lookups could not be handed back: {e}");
                }
                break;
            }
        }
    }
    filled
}

/// Run a round every [`ROUND_GAP`] for as long as the app runs, telling the UI (`item_links`)
/// about every item that got a title.
pub fn start(db: Arc<Database>, source: Arc<dyn TitleSource>, events: Arc<dyn EventSink>) {
    let spawned = std::thread::Builder::new()
        .name("item-titles".into())
        .spawn(move || loop {
            let filled = fill(&db, &*source, helm_core::now_ms());
            if !filled.is_empty() {
                events.emit(AppEvent::new("item_links").with("itemIds", json!(filled)));
            }
            std::thread::sleep(ROUND_GAP);
        });
    if let Err(e) = spawned {
        log::warn!("item titles: the lookup thread did not start: {e}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::tests::db;
    use crate::types::LinkedBy;
    use std::collections::HashMap;
    use std::sync::Mutex;

    /// A board in memory: titles by id, the ids it was asked about, and whether it answers.
    #[derive(Default)]
    struct FakeBoard {
        titles: HashMap<String, String>,
        asked: Mutex<Vec<String>>,
        down: std::sync::atomic::AtomicBool,
    }

    impl TitleSource for FakeBoard {
        fn title(&self, item_id: &str) -> Result<Option<String>, Unavailable> {
            if self.down.load(std::sync::atomic::Ordering::SeqCst) {
                return Err(Unavailable);
            }
            self.asked.lock().unwrap().push(item_id.to_owned());
            Ok(self.titles.get(item_id).cloned())
        }
    }

    #[test]
    fn untitled_items_are_looked_up_once_stored_and_tried_again_later_or_when_the_board_is_back() {
        let (_d, db) = db(&["a", "b", "c"]);
        let (named, known, unknown) = (
            "a1b2c3d4e5f64a7b8c9d0e1f2a3b4c5d",
            "b2c3d4e5f6a74b8c9d0e1f2a3b4c5d6e",
            "c3d4e5f6a7b84c9d0e1f2a3b4c5d6e7f",
        );
        db.write::<_, rusqlite::Error>(|tx| {
            for (chat, item) in [("a", named), ("b", known), ("c", unknown)] {
                store::link(tx, chat, item, false, LinkedBy::Agent, 1)
                    .map_err(rusqlite::Error::InvalidParameterName)?;
            }
            store::set_title(tx, named, "Given by the agent")
        })
        .unwrap();
        let board = FakeBoard {
            titles: HashMap::from([(known.to_owned(), "Fix the deploy".to_owned())]),
            ..Default::default()
        };
        let title_of = |item: &str| {
            db.snapshot(|tx| {
                store::for_item(tx, item).map_err(rusqlite::Error::InvalidParameterName)
            })
            .unwrap()[0]
                .item_title
                .clone()
        };

        // The board is down: nothing is asked, nothing is marked tried.
        board.down.store(true, std::sync::atomic::Ordering::SeqCst);
        assert!(fill(&db, &board, 100).is_empty());
        board.down.store(false, std::sync::atomic::Ordering::SeqCst);

        // Back up: only the untitled items are asked about; what the board knows is stored.
        assert_eq!(fill(&db, &board, 200), [known]);
        let mut asked = board.asked.lock().unwrap().clone();
        asked.sort();
        assert_eq!(asked, [known, unknown]);
        assert_eq!(title_of(known).as_deref(), Some("Fix the deploy"));
        assert_eq!(title_of(named).as_deref(), Some("Given by the agent"));

        // An item the board does not know is not asked about again until the retry time.
        board.asked.lock().unwrap().clear();
        assert!(fill(&db, &board, 300).is_empty());
        assert!(board.asked.lock().unwrap().is_empty());
        fill(&db, &board, 200 + store::TITLE_RETRY_MS);
        assert_eq!(*board.asked.lock().unwrap(), [unknown]);
    }
}
