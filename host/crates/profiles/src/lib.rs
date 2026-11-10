//! Profiles: the people who use this Helm.
//!
//! A profile is a person. Exactly one is the **home** profile, whose Claude sign-in this install
//! runs under (a placeholder named `Me` is made when there is none, waiting for its person's
//! name); the others are people whose work this install can label. Every chat carries two columns:
//! the profile it belongs to and the device it runs on. The profile filters the sidebar and
//! nothing else, so a chat belonging to another person still runs here under this computer's
//! sign-in. A chat can also be shared with other profiles, which shows it in their sidebar without
//! moving it. A profile can be linked to a person known to the hub, which is how the household view
//! lines people up with their computers, and carries an `about` text that tells agents how to
//! address and write for that person.
//!
//! | module | what |
//! |---|---|
//! | [`store`] | the table, the rules (one home profile, unique names, one link per hub person), the identity columns and their fill, owners, shares, the active choice, the sidebar filter and the counts |
//! | [`launch`] | the `who` prompt fragment: whose chat this is and where it runs |
//! | `ops` | the `api` ops below |
//!
//! Ops (arguments and results are camelCase JSON). Every one that changes something answers with
//! the whole [`store::ProfilesView`] and emits `profiles_changed`.
//!
//! | op | class | args |
//! |---|---|---|
//! | `profiles_list` | blocking | `{}` |
//! | `profiles_create` | maintenance | `{name, colour?, hubPersonId?}`; the answer also carries `profile` |
//! | `profiles_update` | maintenance | `{id, name?, colour?, hubPersonId?, about?}`: a present `null` unlinks the hub person; a name settles a placeholder |
//! | `profiles_delete` | maintenance | `{id}`: its chats move to the home profile; the home profile is refused |
//! | `profiles_set_active` | maintenance | `{id}`: a profile id, or `null` for all profiles |
//! | `profiles_assign_chats` | maintenance | `{chatIds, profileId}`: the chats now belong to that profile |
//! | `profiles_share_chats` | maintenance | `{chatIds, profileId, shared}`: show the chats to that profile too, or stop |
//! | `profiles_chat_ids` | blocking | `{profileId}` → `{chatIds}`: the chats it owns and those shared with it (the sidebar filter) |
//!
//! Agents never change profiles (the tables are user-only). They see a chat's profile in
//! `app_this_chat` and can filter `app_list_chats` by it.

pub mod launch;
mod ops;
pub mod store;

pub use ops::register_ops;
pub use store::chat_owner_profile;

pub fn register_migrations(reg: &mut helm_core::db::MigrationRegistry) {
    reg.initializer("profiles", store::init)
        .one_time("profiles-chat-identity", store::fill_identity)
        .recovery_step("profiles-chat-identity-check", store::fill_identity);
}
