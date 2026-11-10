//! Profiles: the people who use this Helm.
//!
//! A profile is a person. Exactly one is the **home** profile, whose Claude sign-in this install
//! runs under; the others are people whose work this install can label. A chat carries a profile
//! label (a side table, so the chat record is untouched): the label filters the sidebar and
//! nothing else, so a chat labelled for another person still runs here under this computer's
//! sign-in. A profile can be linked to a person known to the hub, which is how the household view
//! lines people up with their computers.
//!
//! | module | what |
//! |---|---|
//! | [`store`] | the tables, the rules (one home profile, unique names, one link per hub person), the active choice, the sidebar filter and the counts |
//! | `ops` | the `api` ops below |
//!
//! Ops (arguments and results are camelCase JSON). Every one that changes something answers with
//! the whole [`store::ProfilesView`] and emits `profiles_changed`.
//!
//! | op | class | args |
//! |---|---|---|
//! | `profiles_list` | blocking | `{}` |
//! | `profiles_create` | maintenance | `{name, colour?, hubPersonId?}`: the first profile becomes the home profile; the answer also carries `profile` |
//! | `profiles_update` | maintenance | `{id, name?, colour?, hubPersonId?}`: a present `null` unlinks the hub person |
//! | `profiles_delete` | maintenance | `{id}`: its chats fall back to the home profile; the home profile is refused |
//! | `profiles_set_active` | maintenance | `{id}`: a profile id, or `null` for all profiles |
//! | `profiles_assign_chats` | maintenance | `{chatIds, profileId}` |
//! | `profiles_chat_ids` | blocking | `{profileId}` → `{chatIds}`: the sidebar filter |
//!
//! Agents never change profiles (the tables are user-only). They see a chat's profile in
//! `app_this_chat` and can filter `app_list_chats` by it.

mod ops;
pub mod store;

pub use ops::register_ops;

pub fn register_migrations(reg: &mut helm_core::db::MigrationRegistry) {
    reg.initializer("profiles", store::init);
}
