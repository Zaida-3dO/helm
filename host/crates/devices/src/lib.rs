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
//! | [`tunnel`] | port forwarding: a server on one PC's loopback opened from another PC's, carried as pushes over the hub line |
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
//! | `devices_attach` | async | `{chatId, kind, name, mime?, data?, path?}` → `{attachmentId}`: stage a picture (base64) or a file (path on this PC) for the next message to a chat that runs on another device; the send names the ids |
//! | `devices_pages` | async | `{}` → `{devices: [{deviceUid, name, pages, problem?}]}`: the pages pinned on the other PCs of the household for the active profile's person (their own, then those pinned before profiles), each as the Pages list shows it; a PC that is offline or has its switch off carries a `problem` in words |
//! | `devices_page_open` | async | `{deviceUid, pageId}` → as `page_open`: brings the page and the files beside it that it names across and opens it here, online because the pin that names it lives there; its messages go to the chat the pin names there, and its saved state stays on this PC |
//! | `devices_mcp_list` | async | `{deviceUid}` → `{servers}`: the MCP tool servers set up on that PC, every secret value, saved secret id and credential-file path blanked there before it leaves; that PC answers only its own person |
//! | `devices_discard` | blocking | `{chatId, attachmentId}` → `{}`: take back a staged attachment |
//! | `devices_typed_on` | blocking | `{chatId}` → `{deviceUid, name, at} \| null`: the PC the last message of a chat that runs here was typed on, when that was another device (`at` in Unix ms); `null` when it was typed here, or nothing is known since Helm started |
//! | `devices_file_save` | async | `{chatId, path}` → `{path: string \| null}`: a copy of a file of a chat that runs on another device (inside its project folder there), brought across in chunks and saved on this PC where the person chooses; `null` when the dialog is cancelled |
//! | `devices_forward_offer` | async | `{chatId, port, title?}` → `{port, title, typedOn: {deviceUid, name} \| null, samePc}`: an agent of a chat here offers a port of this PC; refused in words when the port may not be forwarded, nothing answers on it, or (for a person on another PC) the switch is off |
//! | `devices_forward_open` | async | `{chatId, port, title?}` → `Forward`: the port of the PC that runs a copied chat, opened on this PC's `127.0.0.1` (the same port when free); event `devices_forwards_changed` |
//! | `devices_forwards` | blocking | `{}` → `{forwards: [Forward]}`, the ports of another PC opened here, oldest first |
//! | `devices_forward_close` | blocking | `{forwardId}` → `{}`: stop one; the other PC is told |
//! | `devices_forwards_hosted` | blocking | `{}` → `{forwards: [Forward]}`, oldest first: the ports of this PC another PC is viewing now (`deviceUid` names the viewing PC) |
//! | `devices_forward_hosted_stop` | blocking | `{forwardId}` → `{}`: stop sharing one of this PC's ports; the viewing PC is told |
//!
//! `Forward` is `{forwardId, chatId, deviceUid, deviceName, port, localPort, url, title, streams,
//! openedAt, lastUsedAt}` (times in Unix ms). A forward ends when stopped, after 30 minutes with
//! no connection, when either PC's line drops or it quits, and on the other PC when its switch
//! goes off.
//!
//! `model_catalogue` also takes `deviceUid` (the models that device last reported). Every other
//! chat op that names a copied chat is answered by [`route`] (see `helm_chats::remote`).
//!
//! These answers are built as JSON in `ops`, and the UI's copy of them (`ui/src/devices/api.ts`) is
//! typed by hand from the table above: there is no Rust type to generate bindings from. The
//! serialised types in [`wire`] are the device-to-device frames, which the UI never sees.
//!
//! What another device may ask of this one (`wire::Op`): reads — `hello`, `chats_sub`, `chat_sub`,
//! `chat_unsub`, `chats_unsub`, `page`, `attachment_get` (a picture of a message), what an agent
//! showed (`shown_gallery`, `shown_image`, `option_board`), the chunked `blob_read` of a picture,
//! the panel reads of the chat's project folder (`file_view`, `file_stat`, `changes`, `diff`,
//! `archive_list`, `path_resolve`, `chat_folder`, `quick_open`) and the task-board reads
//! (`board_connection`, `board_read`, `board_cancel`) — need only the pairing, except that the
//! panel reads touch this PC's disk and run with the switch; everything that changes something
//! (`send`, `stop`, `answer`, `option_board_answer`, `create`, `set_model`, `rename`, `archive`,
//! `share`, staging, and the rest) and `blob_read` of a project file or a tool's output also need
//! the switch "Let other devices use chats on this PC". Panel paths and project files never
//! leave the chat's project folder. A `send` tells the agent the PC it was typed on, in a first
//! line `[Typed on <PC>]` (a household member's header names it too); `stop` with `noResend`
//! leaves unread words unsent; `answer` with `goto` moves a question of several steps.
//! Pinned pages come across the same way: a device may list the pages its person pinned on this PC
//! (and those pinned before profiles) and read a pinned page's document and the files beside it
//! that the document names (`pages_list`, `pages_files`, `pages_read`, all run with the switch),
//! never any other path. A page is opened by its pin's id whoever pinned it, so a link to it from
//! a shared chat still opens. The `hello` names this PC's projects with the asking person's pins
//! only (their profile's, and those pinned before profiles), and a pin made from afar is theirs.
//! Ports: `forward_open` (with the switch, for a chat the acting person owns or that is shared
//! with them) forwards one of this PC's loopback ports to the asking PC, and `forward_close` ends
//! one; the bytes travel as pushes (see [`tunnel`]).
//! `mcp_list_get` reads this PC's MCP tool servers for Settings → MCP's "Copy from another PC":
//! only a PC acting as this PC's own person is answered, and the answer carries no secrets —
//! every secret value, saved secret id and credential-file path is blanked before it crosses.

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
pub mod tunnel;
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
    let slot = std::sync::Arc::new(helm_chats::remote::RemoteSlot::default());
    services.insert(slot.clone());
    // Host tools leave changes to chats that run elsewhere to the user.
    services.insert(std::sync::Arc::new(
        helm_hosttools::registry::RemoteChatGuard(std::sync::Arc::new(move |id| {
            slot.get().is_some_and(|r| r.owns(id).is_some())
        })),
    ));
    services.insert(std::sync::Arc::new(helm_ipc::OpIntercept(
        std::sync::Arc::new(helm_chats::remote::Routes),
    )));
}
