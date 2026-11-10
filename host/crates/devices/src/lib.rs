//! Devices: which computer a chat runs on, and how the other devices of a household reach it.
//!
//! Every install has one device identity, made the first time it is needed: a random 128-bit id
//! and a name people recognise. Each chat records the id of the device it runs on, so a chat keeps
//! its place when more than one device belongs to the household.
//!
//! A chat runs only on its own device. Other devices read it (a list of every chat, and the
//! transcript of the ones they open) and may act on it within limits. They talk through the hub
//! line the relay crate keeps open: this crate defines what is said and who answers, and has no
//! network code of its own.
//!
//! | module | what |
//! |---|---|
//! | [`identity`] | the `device_self` table, the id, the default and edited names |
//! | `ops` | the `api` ops below |
//! | [`wire`] | the frames and the ops they carry, with their limits |
//! | [`link`] | the [`link::DeviceLink`] seam, [`link::Rpc`] for awaitable requests, and [`link::MemRouter`], an in-process stand-in for the hub |
//! | [`serve`] | the host side: [`serve::Service`] answers other devices through a [`serve::ChatPort`] |
//! | [`subs`] | per-watcher subscriptions: sequence numbers, 250 ms batching, trimming of heavy rows |
//! | [`host`] | the production [`serve::ChatPort`] over this install's chats, profiles and ops |
//! | [`mirror`] | the watcher side: this PC's copy of other devices' chats (tables, subscriptions, catch-up, events) |
//! | [`route`] | the chat ops for a copied chat: reads from the copy, changes sent to the device, refusals in words |
//! | [`sends`] | the `remote_sends` table that makes a repeated send run once |
//! | [`testing`] | a fake port and builders for tests |
//!
//! Ops (arguments and results are camelCase JSON; each change emits `device_changed`):
//!
//! | op | class | args |
//! |---|---|---|
//! | `device_get` | blocking | `{}` → `{uid, name}` |
//! | `device_rename` | maintenance | `{name}` (1 to 40 characters) → `{uid, name}` |
//! | `devices_list` | blocking | `{}` → `{self, devices: [{uid, name, person, personId?, online, lastSeenAt, run, isSelf, state}]}`; `state` is `live`, `readonly`, `offline` or `reconnecting`; event `devices_changed` |
//! | `devices_chats` | blocking | `{deviceUid, profileId?, archived?, offset?, limit? (≤ 200)}` → `{chats, projects: [{id, name, colour, kind}], more}`; for another device the chats are read from the copy and carry `personId`, `sharedWithPeople`, `projectName`, `needsAttention`; event `devices_chats_changed {deviceUid}` |
//! | `devices_create_chat` | async | `{deviceUid, projectId, model?, effort?}` → `{chatId}`: starts the chat for the active profile's person on that device, then opens it here |
//! | `devices_open_chat` | async | `{chatId}` → `{}`: listen to a copied chat; its rows arrive as `chat`, `thread`, `pending`, `message` and `refresh` events |
//!
//! `model_catalogue` also takes `deviceUid` (the models that device last reported). Every other
//! chat op that names a copied chat is answered by [`route`] (see `helm_chats::remote`).
//!
//! What another device may ask of this one (`wire::Op`): `hello`, `chats_sub`, `chat_sub`,
//! `chat_unsub`, `chats_unsub`, `page` (reads, which need only the pairing), and `send`, `stop`,
//! `answer`, `create`, `set_model`, `rename`, `archive`, `share` (which need the switch "Let other
//! devices use chats on this PC"). Consent, settings, projects, files and the rest are refused:
//! they stay with whoever is at this PC.

pub mod host;
pub mod identity;
pub mod link;
pub mod mirror;
mod ops;
pub mod route;
pub mod sends;
pub mod serve;
pub mod subs;
pub mod testing;
pub mod wire;

pub use identity::{self_name, self_uid, DeviceError};
pub use mirror::{Mirror, MirrorSlot, PeopleSync};
pub use ops::register_ops;
pub use serve::{ChatPort, EventTap, Service};

pub fn register_migrations(reg: &mut helm_core::db::MigrationRegistry) {
    reg.initializer("devices-self", identity::init)
        .one_time("devices-make-self", identity::ensure_self)
        .initializer("devices-remote-sends", sends::init)
        .initializer("devices-mirror", mirror::init);
}

/// The place the app's event tap hands chat events to the service once one is running.
pub fn register_services(
    services: &mut helm_ipc::Services,
    _events: &std::sync::Arc<dyn helm_ipc::EventSink>,
) {
    services.insert(std::sync::Arc::new(EventTap::default()));
    // Chats that run on other devices: where the mirror attaches, and the check that sends the
    // ops naming such a chat to it.
    services.insert(std::sync::Arc::new(mirror::MirrorSlot::default()));
    services.insert(std::sync::Arc::new(
        helm_chats::remote::RemoteSlot::default(),
    ));
    services.insert(std::sync::Arc::new(helm_ipc::OpIntercept(
        std::sync::Arc::new(helm_chats::remote::Routes),
    )));
}
