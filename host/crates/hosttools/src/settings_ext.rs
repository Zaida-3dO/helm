//! Settings that live outside the preference table but that chats read and change through the
//! same two tools, `app_get_settings` and `app_update_settings`.
//!
//! A crate that owns such a setting implements [`SettingsExtension`] and registers it in the
//! [`SettingsExtensions`] service. The tools then list its keys beside the preference keys, report
//! its options, record each change in the change ring and undo it like any other.
//!
//! An extension never decides who may change what. A key's access is one line in
//! `settings_allow::NAMED`, and a part of a Direct key that needs approval is declared in
//! [`SettingsExtension::staged_parts`]; the tools check both before `apply` is ever called, so
//! `apply` only validates and stores. It has no way to answer "locked": its only refusals are
//! about the value itself.

use helm_ipc::AppEvent;
use rusqlite::Transaction;
use serde_json::Value;
use std::sync::Arc;

/// A part of an otherwise directly changeable key that a chat stages for the user to approve
/// instead of changing: a dotted path into the key's value, and the one line of why. The tools
/// stage a change that would alter it; the approved change is applied like any other.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StagedPart {
    pub path: &'static str,
    pub reason: &'static str,
}

impl StagedPart {
    /// A part with its reason. In a `const`, an empty path or reason is a compile error.
    pub const fn new(path: &'static str, reason: &'static str) -> Self {
        assert!(!path.is_empty(), "a staged part needs its path");
        assert!(!reason.is_empty(), "a staged part needs its one-line reason");
        assert!(
            reason.len() <= helm_core::prefs::Access::MAX_REASON,
            "the reason must fit on one line of the card"
        );
        Self { path, reason }
    }
}

pub trait SettingsExtension: Send + Sync {
    /// The keys this extension owns. They must not clash with a preference key.
    fn keys(&self) -> Vec<&'static str>;

    /// The full description of a key for `options`: its shape, default, what it does, and which
    /// parts a chat may not change. `None` for a key the extension does not own.
    fn describe(&self, key: &str) -> Option<Value>;

    /// The parts of `key` a chat stages for the user to approve instead of changing (none for
    /// most keys). Required, so that adding a key means deciding it.
    fn staged_parts(&self, key: &str) -> &'static [StagedPart];

    /// The value in effect now (never null: a setting at its default reads as that default).
    fn read(&self, tx: &Transaction<'_>, key: &str) -> Value;

    /// Check `value` (a partial update, merged into the current one), store it, and return the
    /// value before and after. A value that is wrong is a sentence an agent can act on and stores
    /// nothing. `null` resets the key's directly changeable parts. This runs for a chat's own
    /// change and for an approved one alike.
    fn apply(
        &self,
        tx: &Transaction<'_>,
        key: &str,
        value: &Value,
    ) -> Result<(Value, Value), String>;

    /// What [`apply`](Self::apply) would do, stored nowhere: checks a staged change up front. The
    /// default runs the real thing, which is right for a setting kept in the database, because
    /// the caller rolls the transaction back; an extension that keeps its value elsewhere (a
    /// file) overrides it so that checking writes nothing.
    fn check_approved(
        &self,
        tx: &Transaction<'_>,
        key: &str,
        value: &Value,
    ) -> Result<(Value, Value), String> {
        self.apply(tx, key, value)
    }

    /// Put a value from an earlier [`apply`](Self::apply) back, for Undo.
    fn restore(&self, tx: &Transaction<'_>, key: &str, value: &Value) -> Result<(), String>;

    /// The same extension for the person who owns `chat`, when the setting belongs to a person
    /// (their own policy file) rather than to the install. `None`: it serves everyone alike. A
    /// change made from a chat, and its Undo, go to the owner's own copy.
    fn for_chat(&self, _tx: &Transaction<'_>, _chat: &str) -> Option<Arc<dyn SettingsExtension>> {
        None
    }

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

/// A rule an owning crate adds for one preference key: what the generic writer cannot know about
/// its value (a revision to bump, a board that must exist, a chord to normalise).
pub trait PrefRule: Send + Sync {
    fn key(&self) -> &'static str;

    /// Check `value` (never null) against what is stored now and answer what to store. A refusal
    /// is a sentence an agent can act on; nothing is stored.
    fn prepare(
        &self,
        tx: &Transaction<'_>,
        before: Option<&Value>,
        value: &Value,
    ) -> Result<Value, String>;

    /// Called once the change (an update, an approval or an Undo) is committed, for work that
    /// must not run inside the transaction. `after` is the stored value now.
    fn after_commit(&self, _after: Option<&Value>) {}
}

/// The registered preference rules, one service.
#[derive(Clone, Default)]
pub struct PrefRules(pub Vec<Arc<dyn PrefRule>>);

impl PrefRules {
    pub fn for_key(&self, key: &str) -> Option<&Arc<dyn PrefRule>> {
        self.0.iter().find(|r| r.key() == key)
    }
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
