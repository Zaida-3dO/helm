//! Settings that live outside the preference table but that chats read and change through the
//! same two tools, `app_get_settings` and `app_update_settings`.
//!
//! A crate that owns such a setting implements [`SettingsExtension`] and registers it in the
//! [`SettingsExtensions`] service. The tools then list its keys beside the preference keys, report
//! its options, record each change in the change ring and undo it like any other.
//!
//! The extension, not the tools, decides which parts of its value a chat may change. A value a
//! chat has no right to change must make [`SettingsExtension::apply`] return a sentence naming it
//! and where the user changes it.

use helm_ipc::AppEvent;
use rusqlite::Transaction;
use serde_json::Value;
use std::sync::Arc;

pub trait SettingsExtension: Send + Sync {
    /// The keys this extension owns. They must not clash with a preference key.
    fn keys(&self) -> Vec<&'static str>;

    /// The full description of a key for `options`: its shape, default, what it does, and which
    /// parts a chat may not change. `None` for a key the extension does not own.
    fn describe(&self, key: &str) -> Option<Value>;

    /// True when the key is in the All tier: like the preference keys of that tier, chats may
    /// read and change it only while "Chats can change all settings" is on.
    fn needs_all_settings(&self, _key: &str) -> bool {
        false
    }

    /// The value in effect now (never null: a setting at its default reads as that default).
    fn read(&self, tx: &Transaction<'_>, key: &str) -> Value;

    /// Check `value` (a partial update, merged into the current one), store it, and return the
    /// value before and after. A refusal is a sentence an agent can act on and stores nothing.
    /// `null` resets the parts a chat may change.
    fn apply(
        &self,
        tx: &Transaction<'_>,
        key: &str,
        value: &Value,
    ) -> Result<(Value, Value), String>;

    /// Put a value from an earlier [`apply`](Self::apply) back, for Undo.
    fn restore(&self, tx: &Transaction<'_>, key: &str, value: &Value) -> Result<(), String>;

    /// The event that tells the interface this key changed.
    fn changed_event(&self, key: &str) -> AppEvent;

    /// Extra read-only facts about a key, shown beside its value in `app_get_settings` (for
    /// example a queue the setting governs). `None` when there is nothing to add. Anything in it
    /// that another chat wrote must be framed as untrusted text.
    fn details(&self, _tx: &Transaction<'_>, _key: &str) -> Option<Value> {
        None
    }

    /// Called once the change (an update or an Undo) is committed, for work that must not run
    /// inside the transaction, such as waking a service that reads the new value.
    fn after_commit(&self, _key: &str) {}
}

/// The registered extensions, one service.
#[derive(Clone, Default)]
pub struct SettingsExtensions(pub Vec<Arc<dyn SettingsExtension>>);

impl SettingsExtensions {
    pub fn owner(&self, key: &str) -> Option<&Arc<dyn SettingsExtension>> {
        self.0.iter().find(|x| x.keys().contains(&key))
    }

    /// Every key the extensions own.
    pub fn keys(&self) -> Vec<&'static str> {
        self.0.iter().flat_map(|x| x.keys()).collect()
    }
}
