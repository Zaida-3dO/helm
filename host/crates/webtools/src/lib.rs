//! Helm's own web search for agents.
//!
//! | module | what |
//! |---|---|
//! | [`brave`] | the Brave Search call and the cleaning of what comes back |
//! | [`store`] | the sealed Brave key, the settings record and the import from an OpenClaw file |
//! | `tool` | the `app_web_search` host tool |
//! | `settings` | the `webSearchInClaudeChats` key of `app_get_settings` / `app_update_settings` |
//! | `ops` | the ops behind Settings > Web search |
//!
//! **Who gets the tool.** A launch lists `app_web_search` only when [`WebToolsSource::offered`] says
//! so: a key is set, and either the chat runs on a local model (which has no web search of its
//! own) or the user switched on "Offer in Claude chats" (off by default). The call checks the same
//! rules again, so a running chat whose tool list is older than the setting cannot go on searching.
//!
//! **The key.** It is typed or imported in Settings, sealed in the credential store, and read only
//! to build the request header. No op, tool result, event, log line or error carries it; the ops
//! answer only whether one is set.
//!
//! Ops (Settings > Web search; the renderer only): `web_search_get` → [`ops::View`],
//! `web_search_save {offerInClaudeChats}`, `web_search_set_key {key}`, `web_search_forget_key`,
//! `web_search_test` → `{ok, message}` (one search) and `web_search_import {path}` →
//! `{imported: true}`. Every one but the test answers with the view or `imported`; none returns
//! the key. The `web_search` event tells the page something changed.

pub mod brave;
mod ops;
mod settings;
pub mod store;
#[cfg(test)]
mod tests;
mod tool;

pub use ops::View;
pub use settings::{settings_extension, KEY as SETTING_KEY};

use brave::{Hit, Refusal};
use helm_core::credentials::CredentialStore;
use helm_core::db::Database;
use helm_core::profile::Profile;
use helm_hosttools::{WebToolsGate, WebToolsSource};
use helm_ipc::{EventSink, OpRegistry, Services};
use helm_platform::Platform;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// The least time between two searches Helm sends: the free plan allows about one a second, and
/// several chats (or a model that loops) would otherwise earn refusals.
const MIN_GAP: Duration = Duration::from_millis(1_100);

/// The shared web search service: the key, the settings and the one door to Brave.
pub struct WebSearch {
    db: Arc<Database>,
    creds: CredentialStore,
    endpoint: String,
    gap: Duration,
    last: Mutex<Option<Instant>>,
}

impl WebSearch {
    pub fn new(db: Arc<Database>, creds: CredentialStore) -> Self {
        Self {
            db,
            creds,
            endpoint: brave::endpoint(),
            gap: MIN_GAP,
            last: Mutex::new(None),
        }
    }

    /// A service for tests: a loopback stand-in for Brave, and no wait between searches.
    #[cfg(test)]
    pub(crate) fn for_test(db: Arc<Database>, creds: CredentialStore, endpoint: &str) -> Self {
        Self {
            endpoint: brave::resolve_endpoint(Some(endpoint)),
            gap: Duration::ZERO,
            ..Self::new(db, creds)
        }
    }

    pub fn has_key(&self) -> bool {
        store::read_key(&self.creds).is_some()
    }

    pub fn settings(&self) -> store::Settings {
        self.db
            .snapshot(|tx| Ok::<_, rusqlite::Error>(store::load(tx)))
            .unwrap_or_default()
    }

    /// Whether a chat runs on a local model (its `provider` names a local server).
    fn is_local_chat(&self, chat_id: &str) -> bool {
        self.db
            .snapshot(|tx| {
                tx.query_row("SELECT provider FROM chats WHERE id = ?1", [chat_id], |r| {
                    r.get::<_, Option<String>>(0)
                })
            })
            .ok()
            .flatten()
            .is_some()
    }

    /// The same rules as [`WebToolsSource::offered`], for one chat at call time. The error is a
    /// sentence the agent can pass on.
    fn check_allowed(&self, chat_id: &str) -> Result<(), String> {
        if !self.has_key() {
            return Err(
                "No Brave search key is set. The user adds one in Settings > Web search."
                    .to_owned(),
            );
        }
        if !self.settings().offer_in_claude_chats && !self.is_local_chat(chat_id) {
            return Err(
                "Web search is not turned on for Claude chats. The user can turn on \
                        \"Offer in Claude chats\" in Settings > Web search."
                    .to_owned(),
            );
        }
        Ok(())
    }

    /// One search with the stored key, at most one a second across Helm.
    fn search(&self, query: &str, count: u8, freshness: Option<&str>) -> Result<Vec<Hit>, Refusal> {
        let Some(key) = store::read_key(&self.creds) else {
            return Err(Refusal("No Brave search key is set.".to_owned()));
        };
        {
            let mut last = self.last.lock().unwrap_or_else(|p| p.into_inner());
            if let Some(t) = *last {
                std::thread::sleep(self.gap.saturating_sub(t.elapsed()));
            }
            *last = Some(Instant::now());
        }
        brave::search(&self.endpoint, &key, query, count, freshness)
    }
}

impl WebToolsSource for WebSearch {
    fn offered(&self, local_chat: bool) -> bool {
        self.has_key() && (local_chat || self.settings().offer_in_claude_chats)
    }
}

pub fn register_migrations(reg: &mut helm_core::db::MigrationRegistry) {
    reg.initializer("web-search-settings", store::init);
}

pub fn register_ops(reg: &mut OpRegistry) {
    ops::register_ops(reg);
}

/// `app_web_search`.
pub fn register_host_tools(r: &mut helm_hosttools::HostToolRegistry) {
    tool::register_host_tools(r);
}

/// The service and the launch gate that tells chats whether to list the tool. Needs the database,
/// profile and platform already in place.
pub fn register_services(services: &mut Services, _events: &Arc<dyn EventSink>) {
    let (Some(db), Some(profile), Some(platform)) = (
        services.get::<Database>(),
        services.get::<Profile>(),
        services.get::<Platform>(),
    ) else {
        log::warn!("web search: no database, profile or platform; the tool is not offered");
        return;
    };
    let creds = CredentialStore::new(
        &profile.path("credentials"),
        &profile.hash,
        platform.keychain.clone(),
    );
    let svc = Arc::new(WebSearch::new(db, creds));
    services.insert(svc.clone());
    services.insert(Arc::new(WebToolsGate(svc)));
}
