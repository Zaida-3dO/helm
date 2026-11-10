//! The ops behind dictation: the composer's microphone button and Settings > Shell > Dictation.
//! Their errors are fixed sentences that never carry the audio or the text.

use crate::settings::{self, Settings};
use crate::transcribe::{self, Failure, MAX_WAV_BYTES};
use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use helm_core::db::Database;
use helm_ipc::{ApiError, AppEvent, OpClass, OpCtx, OpRegistry};
use helm_localmodels::settings as local;
use helm_localmodels::LocalService;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::Arc;

/// The event that tells the composer and Settings to read the settings again.
pub const EVENT: &str = "dictation";

/// What the page may see of the settings.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct View {
    pub stt_url: Option<String>,
    pub allow_private_network: bool,
    pub model: String,
    pub language: String,
    /// An address is saved and passes the address rule: dictation can be used.
    pub configured: bool,
    /// Graphics memory (GB) a local model load leaves free for speech, from Local models.
    pub speech_reserve_gb: u32,
    pub speech_reserve_max_gb: u32,
}

fn db(ctx: &OpCtx) -> Result<Arc<Database>, ApiError> {
    ctx.services.need::<Database>()
}

fn load(db: &Database) -> Settings {
    db.snapshot(|tx| Ok::<_, rusqlite::Error>(settings::load(tx)))
        .unwrap_or_default()
}

fn view(ctx: &OpCtx, s: &Settings) -> View {
    let configured = s
        .stt_url
        .as_deref()
        .is_some_and(|u| settings::check_stt_url(u, s.allow_private_network).is_ok());
    View {
        stt_url: s.stt_url.clone(),
        allow_private_network: s.allow_private_network,
        model: s.model.clone(),
        language: s.language.clone(),
        configured,
        speech_reserve_gb: ctx
            .services
            .get::<LocalService>()
            .map_or(local::DEFAULT_SPEECH_RESERVE_GB, |l| {
                l.settings().speech_reserve_gb
            }),
        speech_reserve_max_gb: local::MAX_SPEECH_RESERVE_GB,
    }
}

fn to_value<T: Serialize>(v: &T) -> Result<Value, ApiError> {
    serde_json::to_value(v).map_err(|e| ApiError::internal(e.to_string()))
}

/// Read the arguments; a failure never echoes them (a recording may be among them).
fn args<T: for<'de> Deserialize<'de>>(v: Value, shape: &str) -> Result<T, ApiError> {
    serde_json::from_value(v).map_err(|_| ApiError::bad_args(format!("Expected {shape}.")))
}

fn failed(f: &Failure) -> ApiError {
    ApiError::new(f.code(), f.say())
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SetArgs {
    /// An empty address clears it.
    #[serde(default)]
    stt_url: Option<String>,
    #[serde(default)]
    allow_private_network: Option<bool>,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    language: Option<String>,
    #[serde(default)]
    speech_reserve_gb: Option<u32>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct TranscribeArgs {
    wav_base64: String,
}

pub(crate) fn get(ctx: &OpCtx, _args: Value) -> Result<Value, ApiError> {
    to_value(&view(ctx, &load(&*db(ctx)?)))
}

/// Save what is named; the rest stays. The result is refused whole when any part is not valid,
/// and the saved record always passes the address rule.
pub(crate) fn set(ctx: &OpCtx, v: Value) -> Result<Value, ApiError> {
    let a: SetArgs = args(
        v,
        "sttUrl, allowPrivateNetwork, model, language, speechReserveGb (all optional)",
    )?;
    let db = db(ctx)?;
    let mut s = load(&db);
    if let Some(m) = a.model {
        let m = m.trim().to_owned();
        if !settings::model_ok(&m) {
            return Err(ApiError::bad_args(
                "The model name may hold letters, digits and . _ - : / only.",
            ));
        }
        s.model = m;
    }
    if let Some(l) = a.language {
        let l = l.trim().to_owned();
        if !settings::language_ok(&l) {
            return Err(ApiError::bad_args(
                "The language is a code like en or fr, or auto.",
            ));
        }
        s.language = l;
    }
    if let Some(on) = a.allow_private_network {
        s.allow_private_network = on;
    }
    if let Some(u) = a.stt_url {
        s.stt_url = (!u.trim().is_empty()).then(|| u.trim().to_owned());
    }
    // Whatever is saved passes the rule, so a later request never meets an address that was
    // allowed only by a tick that has since gone.
    if let Some(u) = &s.stt_url {
        s.stt_url =
            Some(settings::check_stt_url(u, s.allow_private_network).map_err(ApiError::bad_args)?);
    }
    if let Some(gb) = a.speech_reserve_gb {
        if !(local::MIN_SPEECH_RESERVE_GB..=local::MAX_SPEECH_RESERVE_GB).contains(&gb) {
            return Err(ApiError::bad_args(format!(
                "The memory kept for speech must be a whole number of GB from {} to {}.",
                local::MIN_SPEECH_RESERVE_GB,
                local::MAX_SPEECH_RESERVE_GB
            )));
        }
        let Some(svc) = ctx.services.get::<LocalService>() else {
            return Err(ApiError::bad_args(
                "The memory kept for speech is a local model setting, and local models are not available here.",
            ));
        };
        svc.save(&local::Changes {
            speech_reserve_gb: Some(gb),
            ..Default::default()
        })
        .map_err(|m| ApiError::new("refused", m))?;
    }
    db.write(|tx| settings::save(tx, &s))
        .map_err(|_| ApiError::internal("The dictation settings could not be saved."))?;
    ctx.events.emit(AppEvent::new(EVENT));
    to_value(&view(ctx, &s))
}

/// Turn a recording into text: `{text}`, empty when nothing was said. When the card is short of
/// the speech reserve, idle local models are unloaded first.
pub(crate) fn transcribe(ctx: &OpCtx, v: Value) -> Result<Value, ApiError> {
    let a: TranscribeArgs = args(v, "wavBase64")?;
    // Over the cap before the copy is decoded: a base64 string is a third longer than its bytes.
    if a.wav_base64.len() > MAX_WAV_BYTES / 3 * 4 + 8 {
        return Err(failed(&Failure::TooBig));
    }
    let wav = STANDARD
        .decode(a.wav_base64.as_bytes())
        .map_err(|_| failed(&Failure::NotWav))?;
    let s = load(&*db(ctx)?);
    let models = ctx.services.get::<LocalService>();
    let text = transcribe::transcribe(&s, &wav, || {
        if let Some(l) = &models {
            if l.speech_room_short() {
                l.free_for_speech();
            }
        }
    })
    .map_err(|f| failed(&f))?;
    Ok(json!({"text": text}))
}

/// One second of silence to the saved server: `{ok, message}`. A refusal is an answer, not an
/// error, so the page can show it beside the button.
pub(crate) fn test(ctx: &OpCtx, _args: Value) -> Result<Value, ApiError> {
    let s = load(&*db(ctx)?);
    Ok(
        match transcribe::transcribe(&s, &transcribe::silent_wav(1), || {}) {
            Ok(_) => json!({"ok": true, "message": "The speech-to-text server answered."}),
            Err(f) => json!({"ok": false, "message": f.say()}),
        },
    )
}

pub fn register_ops(reg: &mut OpRegistry) {
    reg.blocking("dictation_settings_get", OpClass::Blocking, get)
        .blocking("dictation_settings_set", OpClass::Maintenance, set)
        .blocking("dictation_transcribe", OpClass::Blocking, transcribe)
        .blocking("dictation_test", OpClass::Blocking, test);
}
