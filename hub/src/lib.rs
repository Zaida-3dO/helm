//! Helm hub: the always-on service on the home server: shared memory, the relay
//! lines to each person's Helm, Helm's Telegram bot and the phone's voice line. See
//! `hub/README.md`.

pub mod admin;
pub mod apptools;
pub mod auth;
pub mod images;
pub mod import;
pub mod lines;
pub mod mcp;
pub mod peer;
pub mod registry;
pub mod relay;
pub mod server;
pub mod store;
pub mod stt;
pub mod telegram;
pub mod voice;
