//! The hub's memories, seen as knowledge layers.
//!
//! On the hub a memory is either **shared** (the whole household sees it: the Everyone layer) or
//! **private** to the person who wrote it (that person's layer). Settings lists them per layer
//! and moves one between layers. Every call acts as a person (the `X-Helm-Person` header, needs
//! the peer scope), because a private memory is only visible to its owner:
//!
//! - the Everyone layer is listed as the home person, and shows every shared memory;
//! - a profile's layer is listed as that profile's person, and shows what they keep private;
//! - a memory is moved by its owner, who is found by trying each linked person in turn. A memory
//!   stays with the person who wrote it, so it can go to Everyone or to its owner's own layer,
//!   never into someone else's.
//!
//! The hub is reached through [`MemoryHub`]; the production one calls the connected `helm_hub`
//! server ([`HubMemory`]).

use crate::layers::Layer;
use helm_mcp::memory::CallError;
use helm_mcp::presets::connected_memory_server;
use helm_mcp::McpStore;
use serde::Serialize;
use serde_json::{json, Value};
use std::sync::Arc;

/// The most memories one list shows.
pub const LIST_LIMIT: u64 = 1000;

/// Why a memory call did not go through.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MemError {
    /// Shared memory is not connected, or the hub cannot be reached.
    Unavailable(String),
    /// The request cannot be done; the sentence says why.
    Refused(String),
}

impl From<CallError> for MemError {
    fn from(e: CallError) -> Self {
        match e {
            CallError::Unreachable(m) => MemError::Unavailable(m),
            CallError::Failed(m) => MemError::Refused(m),
        }
    }
}

/// Calls a memory tool on the hub, acting as `person` (a hub person id) when given.
pub trait MemoryHub: Send + Sync {
    fn call(&self, person: Option<&str>, tool: &str, args: Value) -> Result<Value, MemError>;
}

/// The connected `helm_hub` server.
pub struct HubMemory {
    pub store: Arc<McpStore>,
}

impl MemoryHub for HubMemory {
    fn call(&self, person: Option<&str>, tool: &str, args: Value) -> Result<Value, MemError> {
        let unavailable = |m: &str| MemError::Unavailable(m.to_owned());
        let servers = self
            .store
            .load()
            .map_err(|e| MemError::Unavailable(e.to_string()))?;
        let id = connected_memory_server(&servers)
            .ok_or_else(|| unavailable("Shared memory isn't connected."))?;
        let server = self
            .store
            .resolve_server(&id, person)
            .map_err(|e| MemError::Unavailable(e.to_string()))?
            .ok_or_else(|| unavailable("Shared memory isn't connected."))?;
        Ok(helm_mcp::memory::call_tool(&server, tool, args)?)
    }
}

/// One memory in a layer's list.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MemoryRow {
    /// The hub's number, as text.
    pub id: String,
    pub title: String,
    /// The name of the person who wrote it; empty when the hub does not say.
    pub owner: String,
    pub layer: String,
}

/// The memories of `layer`. `person` is the hub person the list is read as: the home person for
/// Everyone, the profile's own person for a profile layer (`None` there means the profile has no
/// person, so no memories).
pub fn list(
    hub: &dyn MemoryHub,
    layer: &Layer,
    person: Option<&str>,
) -> Result<Vec<MemoryRow>, MemError> {
    if matches!(layer, Layer::Profile(_)) && person.is_none() {
        return Ok(Vec::new());
    }
    let answer = hub.call(person, "memory_list", json!({ "limit": LIST_LIMIT }))?;
    let shared = matches!(layer, Layer::Everyone);
    Ok(answer["results"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|m| {
            let visibility = m["visibility"].as_str().unwrap_or("");
            if shared {
                visibility == "shared"
            } else {
                visibility == "private" && m["mine"].as_bool() == Some(true)
            }
        })
        .filter_map(|m| {
            Some(MemoryRow {
                id: m["id"].as_i64()?.to_string(),
                title: m["title"].as_str().unwrap_or("").to_owned(),
                owner: m["owner"].as_str().unwrap_or("").to_owned(),
                layer: layer.key(),
            })
        })
        .collect())
}

/// Move memory `id` to `to`. `to_person` is the hub person of the target profile layer; `persons`
/// are every linked person, the one this Helm belongs to first (empty when none is linked, and
/// the call then acts as this machine's own person).
pub fn move_to(
    hub: &dyn MemoryHub,
    id: i64,
    to: &Layer,
    to_person: Option<&str>,
    persons: &[String],
) -> Result<(), MemError> {
    let actors: Vec<Option<&str>> = if persons.is_empty() {
        vec![None]
    } else {
        persons.iter().map(|p| Some(p.as_str())).collect()
    };
    let mut owner = None;
    for actor in actors {
        match hub.call(actor, "memory_read", json!({ "id": id })) {
            Ok(m) if m["mine"].as_bool() == Some(true) => {
                owner = Some(actor);
                break;
            }
            Ok(_) => {}
            Err(MemError::Unavailable(m)) => return Err(MemError::Unavailable(m)),
            Err(MemError::Refused(_)) => {}
        }
    }
    let Some(owner) = owner else {
        return Err(MemError::Refused(
            "That memory is not there, or it belongs to someone Helm cannot act for.".to_owned(),
        ));
    };
    let scope = match to {
        Layer::Everyone => "everyone",
        Layer::Profile(_) => {
            if to_person.is_none() || to_person != owner {
                return Err(MemError::Refused(
                    "A memory stays with the person who wrote it, so it can only move into their own layer."
                        .to_owned(),
                ));
            }
            "me"
        }
    };
    hub.call(owner, "memory_update", json!({ "id": id, "scope": scope }))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// A memory store: (id, title, owner person id, shared).
    #[allow(clippy::type_complexity)]
    #[derive(Default)]
    struct Fake {
        rows: Mutex<Vec<(i64, &'static str, &'static str, bool)>>,
        down: bool,
    }

    impl MemoryHub for Fake {
        fn call(&self, person: Option<&str>, tool: &str, args: Value) -> Result<Value, MemError> {
            if self.down {
                return Err(MemError::Unavailable("Shared memory isn't connected.".into()));
            }
            let me = person.unwrap_or("3");
            let mut rows = self.rows.lock().unwrap();
            let view = |r: &(i64, &str, &str, bool)| {
                json!({ "id": r.0, "title": r.1, "owner": format!("Person {}", r.2),
                        "visibility": if r.3 { "shared" } else { "private" }, "mine": r.2 == me })
            };
            let visible = |r: &&(i64, &str, &str, bool)| r.3 || r.2 == me;
            match tool {
                "memory_list" => Ok(json!({ "results":
                    rows.iter().filter(visible).map(view).collect::<Vec<_>>() })),
                "memory_read" => rows
                    .iter()
                    .filter(visible)
                    .find(|r| Some(r.0) == args["id"].as_i64())
                    .map(view)
                    .ok_or_else(|| MemError::Refused("not found".into())),
                "memory_update" => {
                    let id = args["id"].as_i64();
                    let row = rows
                        .iter_mut()
                        .find(|r| Some(r.0) == id && r.2 == me)
                        .ok_or_else(|| MemError::Refused("That memory is not yours.".into()))?;
                    row.3 = args["scope"] == "everyone";
                    Ok(json!({ "id": row.0 }))
                }
                _ => Err(MemError::Refused("no such tool".into())),
            }
        }
    }

    #[test]
    fn memories_list_by_layer_and_move_only_for_their_owner() {
        let hub = Fake {
            rows: Mutex::new(vec![
                (1, "House rules", "3", true),
                (2, "Sam's note", "7", true),
                (3, "Home diary", "3", false),
                (4, "Sam diary", "7", false),
            ]),
            down: false,
        };
        let (ev, home, sam) = (
            Layer::Everyone,
            Layer::Profile("home".into()),
            Layer::Profile("sam".into()),
        );
        let ids = |rows: Vec<MemoryRow>| {
            rows.iter()
                .map(|r| r.id.parse::<i64>().unwrap())
                .collect::<Vec<_>>()
        };
        assert_eq!(ids(list(&hub, &ev, Some("3")).unwrap()), [1, 2]);
        let mine = list(&hub, &home, Some("3")).unwrap();
        assert_eq!(ids(mine.clone()), [3]);
        assert_eq!((mine[0].layer.as_str(), mine[0].owner.as_str()), ("profile:home", "Person 3"));
        assert_eq!(ids(list(&hub, &sam, Some("7")).unwrap()), [4]);
        assert!(list(&hub, &Layer::Profile("guest".into()), None).unwrap().is_empty());

        let persons = ["3".to_owned(), "7".to_owned()];
        // Sam's private diary goes to Everyone (found through Sam's own person), then back.
        move_to(&hub, 4, &ev, None, &persons).unwrap();
        assert_eq!(ids(list(&hub, &ev, Some("3")).unwrap()), [1, 2, 4]);
        move_to(&hub, 4, &sam, Some("7"), &persons).unwrap();
        assert_eq!(ids(list(&hub, &sam, Some("7")).unwrap()), [4]);
        // A shared memory goes into its owner's layer, not into someone else's.
        move_to(&hub, 2, &sam, Some("7"), &persons).unwrap();
        for (id, to, person) in [(1, &sam, Some("7")), (2, &home, Some("3")), (3, &sam, None)] {
            let e = move_to(&hub, id, to, person, &persons).unwrap_err();
            assert!(matches!(e, MemError::Refused(_)), "{id}: {e:?}");
        }
        assert!(matches!(
            move_to(&hub, 99, &ev, None, &persons),
            Err(MemError::Refused(_))
        ));
        // With nothing linked the call acts as the machine's own person.
        move_to(&hub, 1, &ev, None, &[]).unwrap();

        let down = Fake {
            down: true,
            ..Default::default()
        };
        assert!(matches!(list(&down, &ev, None), Err(MemError::Unavailable(_))));
        assert!(matches!(
            move_to(&down, 1, &ev, None, &persons),
            Err(MemError::Unavailable(_))
        ));
    }
}
