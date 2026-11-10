//! Helm hub: the always-on service on the home server: shared memory, the relay
//! lines to each person's Helm, Helm's Telegram bot, the phone's voice line and the Remote page. See
//! `hub/README.md`.

pub mod admin;
pub mod apptools;
pub mod auth;
pub mod devices;
pub mod images;
pub mod import;
pub mod knowledge;
pub mod lines;
pub mod mcp;
pub mod peer;
pub mod registry;
pub mod relay;
pub mod remote;
pub mod server;
pub mod store;
pub mod stt;
pub mod telegram;
pub mod voice;
pub mod voice_http;
