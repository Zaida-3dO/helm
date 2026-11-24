//! Dictation in the composer, and keeping speech-to-text working beside a local model.
//!
//! | module | what |
//! |---|---|
//! | [`settings`] | the saved settings and the address rule for the speech-to-text server |
//! | [`transcribe`] | the multipart call that turns a WAV into text |
//! | [`live`] | live dictation: the Windows speech service streaming words into the composer |
//! | `ops` | the ops behind the composer's microphone button and Settings > Shell > Dictation |
//! | `tool` | the `app_make_room_for_speech` host tool, kept for hubs that still call it; it does nothing |
//!
//! **The page records and Helm sends.** The renderer is not allowed to reach the network, so the
//! composer records the user's voice, encodes it as a 16 kHz mono WAV and hands it to the host,
//! which posts it to the OpenAI-style `/v1/audio/transcriptions` route of the server set in
//! Settings. The address is on this computer by default; an address on the user's own network
//! needs a tick, and nothing else is ever used ([`settings::check_stt_url`]). The text comes back
//! to the page, which puts it in the draft: it is never sent as a message.
//!
//! **Nothing here logs or stores a recording or its text.** A recording is held in memory for the
//! length of one call.
//!
//! **Live dictation streams from Windows itself.** Where the Windows speech service can run, the
//! composer dictates live: [`live`] starts a continuous recognition session on this machine, words
//! arrive as `live_transcript` events (the sentence in progress, then each settled sentence), and
//! the page shows them as they come — nothing is recorded or sent anywhere. Where the service has
//! no model, or no microphone, the op answers in one line and the page keeps the recording path.
//!
//! **Speech-to-text never unloads a local model.** A recording goes straight to the server, loaded
//! model or not: a slower transcription costs less than a model that has to load again. The
//! speech reserve (a setting of Local models) is still kept, but only when a local model has to
//! unload others to fit anyway: then Helm frees that much more, so speech-to-text fits beside it.
//!
//! Ops (the renderer only):
//!
//! | Op | Class | Args | Result |
//! |---|---|---|---|
//! | `dictation_settings_get` | blocking | none | [`ops::View`] |
//! | `dictation_settings_set` | maintenance | `{sttUrl?, allowPrivateNetwork?, model?, language?, speechReserveGb?}` | [`ops::View`] |
//! | `dictation_transcribe` | blocking | `{wavBase64}` | `{text}`, or an error whose `code` is `not_set_up`, `bad_address`, `too_big`, `bad_audio`, `unreachable`, `server` or `unreadable` |
//! | `dictation_test` | blocking | none | `{ok, message}` (one second of silence to the server) |
//! | `dictation_live` | blocking | `{action: start, stop or cancel}` | `{ok}`, or an error whose `code` is `not_available`. A `start` takes the microphone from any session still running. |
//!
//! The `dictation` event tells the page the settings changed. A live session speaks for itself:
//! `live_transcript {final, text}` carries the words, and `dictation_live {state: failed, message}`
//! says when a session ended on its own.

pub mod bindings;
pub mod live;
mod ops;
pub mod settings;
#[cfg(test)]
mod tests;
mod tool;
pub mod transcribe;

pub mod ext;

pub use live::LiveDictation;
pub use ops::{View, EVENT};

pub fn register_migrations(reg: &mut helm_core::db::MigrationRegistry) {
    reg.initializer("dictation-settings", settings::init);
}

pub fn register_ops(reg: &mut helm_ipc::OpRegistry) {
    ops::register_ops(reg);
}

/// The one live-dictation session slot, shared by the `dictation_live` op handlers.
pub fn register_services(services: &mut helm_ipc::Services) {
    services.insert(std::sync::Arc::new(LiveDictation::new()));
}

/// `app_make_room_for_speech`, which answers at once and unloads nothing.
pub fn register_host_tools(r: &mut helm_hosttools::HostToolRegistry) {
    tool::register_host_tools(r);
}
