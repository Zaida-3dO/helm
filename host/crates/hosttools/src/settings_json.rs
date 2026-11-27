//! A settings key that is one JSON value kept by some table or file: the way a crate gives chats a
//! route to a setting without writing a whole extension.
//!
//! The crate supplies how to read the value, how to check and store a whole new one, and what the
//! defaults are; [`JsonKey`] does the rest: a patch (an object names only the fields to change,
//! `null` restores the defaults), the before and after for the notice and Undo, and Undo itself.
//! It never decides who may change what: a part that needs approval is declared in `staged`, and
//! the tools stage it before anything here runs.
//!
//! A key that belongs to a person (their own copy) says so with [`JsonKey::per_person`]: a chat of
//! that person reads and changes theirs, and Undo goes back to it. A key that belongs to one chat
//! (a switch each chat has) says so with [`JsonKey::per_chat`].

use crate::settings_ext::{chat_profile, SettingsExtension, StagedPart};
use helm_ipc::AppEvent;
use rusqlite::Transaction;
use serde_json::Value;
use std::sync::Arc;

/// Reads the stored value for a person (`None`: the install's own, or the home person's).
pub type Load = dyn Fn(&Transaction<'_>, Option<&str>) -> Result<Value, String> + Send + Sync;
/// Checks a whole new value, stores it, and answers what was stored.
pub type Save =
    dyn Fn(&Transaction<'_>, Option<&str>, &Value) -> Result<Value, String> + Send + Sync;

#[derive(Clone)]
pub struct JsonKey {
    key: &'static str,
    event: &'static str,
    about: Value,
    staged: &'static [StagedPart],
    defaults: Arc<dyn Fn() -> Value + Send + Sync>,
    load: Arc<Load>,
    save: Arc<Save>,
    restore: Option<Arc<Save>>,
    after: Option<Arc<dyn Fn() + Send + Sync>>,
    per_person: bool,
    per_chat: bool,
    profile: Option<String>,
}

impl JsonKey {
    /// A key. `about` is the options the tools show (type, fields, what it does); `event` is the
    /// event that tells the interface it changed.
    pub fn new(
        key: &'static str,
        event: &'static str,
        about: Value,
        defaults: impl Fn() -> Value + Send + Sync + 'static,
        load: impl Fn(&Transaction<'_>, Option<&str>) -> Result<Value, String> + Send + Sync + 'static,
        save: impl Fn(&Transaction<'_>, Option<&str>, &Value) -> Result<Value, String>
            + Send
            + Sync
            + 'static,
    ) -> Self {
        Self {
            key,
            event,
            about,
            staged: &[],
            defaults: Arc::new(defaults),
            load: Arc::new(load),
            save: Arc::new(save),
            restore: None,
            after: None,
            per_person: false,
            per_chat: false,
            profile: None,
        }
    }

    /// Parts of the value a chat stages instead of changing.
    pub fn staged(mut self, parts: &'static [StagedPart]) -> Self {
        self.staged = parts;
        self
    }

    /// How Undo puts a value back, when that is not the same as saving it (a list that a change
    /// only added to, which Undo must take the additions out of).
    pub fn restoring(
        mut self,
        f: impl Fn(&Transaction<'_>, Option<&str>, &Value) -> Result<Value, String>
            + Send
            + Sync
            + 'static,
    ) -> Self {
        self.restore = Some(Arc::new(f));
        self
    }

    /// Work to do once a change (or Undo) has committed, outside the transaction.
    pub fn after_commit(mut self, f: impl Fn() + Send + Sync + 'static) -> Self {
        self.after = Some(Arc::new(f));
        self
    }

    /// Each person has their own copy: the load and save closures get the profile id.
    pub fn per_person(mut self) -> Self {
        self.per_person = true;
        self
    }

    /// Each chat has its own value: the load and save closures get the chat's id.
    pub fn per_chat(mut self) -> Self {
        self.per_chat = true;
        self
    }

    /// The extension to register.
    pub fn extension(self) -> Arc<dyn SettingsExtension> {
        Arc::new(self)
    }

    fn who(&self) -> Option<&str> {
        self.profile.as_deref()
    }
}

/// Merge `patch` into `base`: objects field by field, anything else replaces.
fn merge(base: &mut Value, patch: &Value) {
    match (base, patch) {
        (Value::Object(b), Value::Object(p)) => {
            for (k, v) in p {
                match b.get_mut(k) {
                    Some(slot) => merge(slot, v),
                    None => {
                        b.insert(k.clone(), v.clone());
                    }
                }
            }
        }
        (slot, v) => *slot = v.clone(),
    }
}

impl SettingsExtension for JsonKey {
    fn staged_parts(&self, _key: &str) -> &'static [StagedPart] {
        self.staged
    }

    fn keys(&self) -> Vec<&'static str> {
        vec![self.key]
    }

    fn describe(&self, key: &str) -> Option<Value> {
        (key == self.key).then(|| self.about.clone())
    }

    fn read(&self, tx: &Transaction<'_>, _key: &str) -> Value {
        (self.load)(tx, self.who()).unwrap_or_else(|_| (self.defaults)())
    }

    fn apply(
        &self,
        tx: &Transaction<'_>,
        key: &str,
        value: &Value,
    ) -> Result<(Value, Value), String> {
        let before = (self.load)(tx, self.who())?;
        let wanted = if value.is_null() {
            (self.defaults)()
        } else if before.is_object() && value.is_object() {
            let mut m = before.clone();
            merge(&mut m, value);
            m
        } else if before.is_object() {
            return Err(format!(
                "{key} is an object with only the fields to change (see its options), or null."
            ));
        } else {
            value.clone()
        };
        let after = (self.save)(tx, self.who(), &wanted)?;
        Ok((before, after))
    }

    fn restore(&self, tx: &Transaction<'_>, _key: &str, value: &Value) -> Result<(), String> {
        let f = self.restore.as_ref().unwrap_or(&self.save);
        f(tx, self.who(), value).map(|_| ())
    }

    fn for_chat(&self, tx: &Transaction<'_>, chat: &str) -> Option<Arc<dyn SettingsExtension>> {
        if !self.per_person && !self.per_chat {
            return None;
        }
        let mut mine = self.clone();
        mine.profile = if self.per_chat {
            Some(chat.to_owned())
        } else {
            chat_profile(tx, chat)
        };
        Some(Arc::new(mine))
    }

    fn for_person(
        &self,
        _tx: &Transaction<'_>,
        profile: &str,
    ) -> Option<Arc<dyn SettingsExtension>> {
        if !self.per_person {
            return None;
        }
        let mut theirs = self.clone();
        theirs.profile = Some(profile.to_owned());
        Some(Arc::new(theirs))
    }

    fn changed_event(&self, _key: &str) -> AppEvent {
        AppEvent::new(self.event)
    }

    fn after_commit(&self, _key: &str) {
        if let Some(f) = &self.after {
            f();
        }
    }
}
