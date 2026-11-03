//! What Helm asks of each kind of server.
//!
//! **Ollama** (`/api/*`): the installed models (`tags`), what is loaded (`ps`), a model's own
//! context length (`show`), a Helm variant of a model with a chosen context window (`create`,
//! `delete`), and loading or unloading through `generate` with no prompt.
//!
//! **Strata** (an Anthropic-style model server): `/health` says what is loaded, a model list at
//! `/v1/models` (or `/models`), `/unload` frees its memory, and `/slots` says whether it is
//! answering a request now and how many more it is holding. It answers one request at a time, so
//! the rest wait; Helm reads `/slots` only while one of its chats has a request open there, to
//! tell a chat waiting for its turn from one whose model is working on it. The model loads when a
//! request for it arrives, so Helm never loads one itself.
//!
//! Every answer is read leniently: a field that is missing or shaped differently is skipped, not an
//! error, since these servers differ in small ways between versions.

use crate::net::{self, Conn, Http, NetError, Reply};
use helm_chats::local::QueueInfo;
use serde_json::{json, Value};

/// Why a call to a server gave no usable answer.
#[derive(Debug, PartialEq, Eq)]
pub enum CallError {
    Unreachable(String),
    /// The server refused the key.
    Refused,
    Status(u16, String),
    Shape(String),
}

impl CallError {
    /// A sentence for the user. `who` is the server's name.
    pub fn say(&self, who: &str, url: &str) -> String {
        match self {
            Self::Unreachable(_) => format!("{who} did not answer at {url}. Is it running?"),
            Self::Refused => {
                format!("{who} refused the key. Set it again in Settings > Local models.")
            }
            Self::Status(s, m) if m.is_empty() => format!("{who} answered with an error ({s})."),
            Self::Status(s, m) => format!("{who} answered with an error ({s}): {m}"),
            Self::Shape(m) => format!("{who} sent an answer Helm could not read: {m}"),
        }
    }
}

impl From<NetError> for CallError {
    fn from(e: NetError) -> Self {
        match e {
            NetError::Unreachable(m) => Self::Unreachable(m),
            NetError::Other(m) => Self::Shape(m),
        }
    }
}

/// The reply if it succeeded; a refused key or an error status otherwise.
fn good(r: Reply) -> Result<Reply, CallError> {
    if r.ok() {
        return Ok(r);
    }
    if r.status == 401 || r.status == 403 {
        return Err(CallError::Refused);
    }
    let msg = r
        .json()
        .and_then(|v| {
            v.get("error").and_then(|e| {
                e.as_str()
                    .map(str::to_owned)
                    .or_else(|| e.get("message")?.as_str().map(str::to_owned))
            })
        })
        .unwrap_or_default();
    Err(CallError::Status(r.status, msg.chars().take(200).collect()))
}

fn num(v: &Value) -> Option<u64> {
    v.as_u64()
        .or_else(|| v.as_f64().filter(|f| *f >= 0.0).map(|f| f as u64))
}

// ---- names ----------------------------------------------------------------------------------

/// The name without a trailing `:latest`, which Ollama adds and drops freely.
pub fn norm(name: &str) -> &str {
    name.strip_suffix(":latest").unwrap_or(name)
}

/// Helm's variant of a model with a `ctx`-token window: the model's name plus `-helm-<n>k`.
pub fn variant_name(base: &str, ctx: u32) -> String {
    format!("{}-helm-{}k", norm(base), ctx / 1024)
}

/// The window a variant name stands for.
pub fn variant_context(name: &str) -> Option<u32> {
    let rest = norm(name).rsplit_once("-helm-")?.1;
    let n: u32 = rest.strip_suffix('k')?.parse().ok()?;
    (n > 0).then(|| n * 1024)
}

/// The model a Helm variant was made from, or the name itself when it is not a variant.
pub fn variant_base(name: &str) -> &str {
    let n = norm(name);
    match n.rsplit_once("-helm-") {
        Some((base, _)) if variant_context(n).is_some() => base,
        _ => n,
    }
}

pub fn is_variant(name: &str) -> bool {
    variant_context(name).is_some()
}

// ---- Ollama ---------------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Installed {
    pub name: String,
    pub size: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Resident {
    pub name: String,
    pub size_vram: u64,
}

pub fn ollama_tags(h: &Http, c: &Conn) -> Result<Vec<Installed>, CallError> {
    let r = good(h.get(c, "/api/tags", net::QUICK)?)?;
    let v = r
        .json()
        .ok_or_else(|| CallError::Shape("not JSON".into()))?;
    Ok(v.get("models")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|m| {
                    Some(Installed {
                        name: m
                            .get("name")
                            .or_else(|| m.get("model"))?
                            .as_str()?
                            .to_owned(),
                        size: m.get("size").and_then(num).unwrap_or(0),
                    })
                })
                .collect()
        })
        .unwrap_or_default())
}

pub fn ollama_ps(h: &Http, c: &Conn) -> Result<Vec<Resident>, CallError> {
    let r = good(h.get(c, "/api/ps", net::QUICK)?)?;
    let v = r
        .json()
        .ok_or_else(|| CallError::Shape("not JSON".into()))?;
    Ok(v.get("models")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|m| {
                    Some(Resident {
                        name: m
                            .get("name")
                            .or_else(|| m.get("model"))?
                            .as_str()?
                            .to_owned(),
                        size_vram: m
                            .get("size_vram")
                            .and_then(num)
                            .or_else(|| m.get("size").and_then(num))
                            .unwrap_or(0),
                    })
                })
                .collect()
        })
        .unwrap_or_default())
}

/// A model's own maximum context length, from `model_info`'s `<architecture>.context_length`.
pub fn ollama_context(h: &Http, c: &Conn, model: &str) -> Option<u64> {
    let r = h
        .post(c, "/api/show", &json!({"model": model}), net::QUICK)
        .ok()
        .and_then(|r| good(r).ok())?;
    r.json()?
        .get("model_info")?
        .as_object()?
        .iter()
        .find(|(k, _)| k.ends_with(".context_length"))
        .and_then(|(_, v)| num(v))
}

/// Make `variant` from `from` with a `num_ctx` window. The weights are shared, so it takes no
/// extra disk.
pub fn ollama_create(
    h: &Http,
    c: &Conn,
    variant: &str,
    from: &str,
    num_ctx: u32,
) -> Result<(), CallError> {
    let body = json!({"model": variant, "from": from, "parameters": {"num_ctx": num_ctx}, "stream": false});
    good(h.post(c, "/api/create", &body, net::CREATE)?).map(|_| ())
}

pub fn ollama_delete(h: &Http, c: &Conn, model: &str) -> Result<(), CallError> {
    good(h.delete(c, "/api/delete", &json!({"model": model}), net::MANAGE)?).map(|_| ())
}

/// Load `model` and keep it for `minutes` after its last use. No prompt, so nothing is generated;
/// when the model is already loaded this only resets the timer.
pub fn ollama_load(h: &Http, c: &Conn, model: &str, minutes: u32) -> Result<(), CallError> {
    let body = json!({"model": model, "keep_alive": format!("{minutes}m"), "stream": false});
    good(h.post(c, "/api/generate", &body, net::LOAD)?).map(|_| ())
}

pub fn ollama_unload(h: &Http, c: &Conn, model: &str) -> Result<(), CallError> {
    let body = json!({"model": model, "keep_alive": 0, "stream": false});
    good(h.post(c, "/api/generate", &body, net::MANAGE)?).map(|_| ())
}

// ---- Strata ---------------------------------------------------------------------------------

/// The models Strata has in memory. `*` stands for "something is loaded" when the server does
/// not say which.
pub fn strata_loaded(h: &Http, c: &Conn) -> Result<Vec<String>, CallError> {
    let r = good(h.get(c, "/health", net::QUICK)?)?;
    let v = r.json().unwrap_or(Value::Null);
    let mut names = loaded_names(v.get("loaded"));
    // `{"loaded": true, "model": "name"}`: the flag says something is loaded, `model` says what.
    if names == ["*"] {
        if let Some(m) = v
            .get("model")
            .and_then(Value::as_str)
            .filter(|m| !m.is_empty())
        {
            names = vec![m.to_owned()];
        }
    }
    Ok(names)
}

fn loaded_names(v: Option<&Value>) -> Vec<String> {
    match v {
        Some(Value::String(s)) if !s.is_empty() => vec![s.clone()],
        Some(Value::Bool(true)) => vec!["*".into()],
        Some(Value::Array(a)) => a
            .iter()
            .filter_map(|e| match e {
                Value::String(s) => Some(s.clone()),
                Value::Object(o) => ["model", "name", "id"]
                    .iter()
                    .find_map(|k| o.get(*k).and_then(Value::as_str))
                    .map(str::to_owned),
                _ => None,
            })
            .collect(),
        Some(Value::Object(o)) => ["model", "name", "id"]
            .iter()
            .find_map(|k| o.get(*k).and_then(Value::as_str))
            .map(|s| vec![s.to_owned()])
            .unwrap_or_else(|| vec!["*".into()]),
        _ => Vec::new(),
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Listed {
    pub id: String,
    pub context: Option<u64>,
}

/// Strata's own window from `/health`'s `max_context`, when it says one. Strata's model list
/// often leaves the size out; the health line carries the window the whole server runs with.
pub fn strata_context(h: &Http, c: &Conn) -> Option<u64> {
    let r = good(h.get(c, "/health", net::QUICK).ok()?).ok()?;
    r.json()?.get("max_context").and_then(num)
}

/// Strata's model list: `/v1/models`, then `/models`. `None` when neither gives one.
pub fn strata_models(h: &Http, c: &Conn) -> Option<Vec<Listed>> {
    for path in ["/v1/models", "/models"] {
        let Ok(r) = h.get(c, path, net::QUICK) else {
            continue;
        };
        if !r.ok() {
            continue;
        }
        if let Some(list) = r.json().as_ref().and_then(parse_model_list) {
            return Some(list);
        }
    }
    None
}

fn parse_model_list(v: &Value) -> Option<Vec<Listed>> {
    let arr = v
        .as_array()
        .or_else(|| v.get("data").and_then(Value::as_array))
        .or_else(|| v.get("models").and_then(Value::as_array))?;
    let list: Vec<Listed> = arr
        .iter()
        .filter_map(|e| match e {
            Value::String(s) => Some(Listed {
                id: s.clone(),
                context: None,
            }),
            Value::Object(o) => {
                let id = ["id", "name", "model"]
                    .iter()
                    .find_map(|k| o.get(*k).and_then(Value::as_str))?;
                let context = [
                    "context_length",
                    "max_model_len",
                    "context_window",
                    "max_context",
                ]
                .iter()
                .find_map(|k| o.get(*k).and_then(num));
                Some(Listed {
                    id: id.to_owned(),
                    context,
                })
            }
            _ => None,
        })
        .collect();
    (!list.is_empty()).then_some(list)
}

pub fn strata_unload(h: &Http, c: &Conn) -> Result<(), CallError> {
    good(h.post(c, "/unload", &json!({}), net::MANAGE)?).map(|_| ())
}

/// Strata's own picture of its one-at-a-time request handling: `/slots`. A short read like the
/// health check, so a busy or paused server costs nothing. `None` when the answer says nothing
/// usable.
pub fn strata_queue(h: &Http, c: &Conn) -> Option<QueueInfo> {
    let r = h.get(c, "/slots", net::QUICK).ok()?;
    if !r.ok() {
        return None;
    }
    read_queue(&r.json()?)
}

/// A JSON value read as yes or no: a boolean, a non-zero number, a non-empty list or object, or
/// the usual words. `None` when it says nothing either way.
fn truthy(v: Option<&Value>) -> Option<bool> {
    match v? {
        Value::Bool(b) => Some(*b),
        Value::Number(n) => n.as_f64().map(|f| f != 0.0),
        Value::String(s) => match s.to_ascii_lowercase().as_str() {
            "true" | "yes" | "on" | "busy" | "serving" | "active" => Some(true),
            "false" | "no" | "off" | "idle" | "free" | "empty" => Some(false),
            _ => None,
        },
        Value::Array(a) => Some(!a.is_empty()),
        Value::Object(o) => Some(!o.is_empty()),
        Value::Null => None,
    }
}

/// One entry of a slot list: busy when it says so, by a flag or by a state word.
fn slot_busy(s: &Value) -> bool {
    truthy(s.get("busy").or_else(|| s.get("active"))).unwrap_or_else(|| {
        s.get("state")
            .or_else(|| s.get("status"))
            .and_then(Value::as_str)
            .is_some_and(|w| {
                matches!(
                    w.to_ascii_lowercase().as_str(),
                    "busy" | "serving" | "active" | "working" | "generating"
                )
            })
    })
}

/// A `/slots` answer to queue facts, lenient like every other read: several spellings of the
/// busy flag, a queue given as a number or as a list, or a bare list of slots.
fn read_queue(v: &Value) -> Option<QueueInfo> {
    let (busy, queued) = match v {
        Value::Object(o) => {
            let busy = ["busy", "serving", "active", "occupied"]
                .iter()
                .find_map(|k| truthy(o.get(*k)))
                .or_else(|| truthy(o.get("idle")).map(|idle| !idle));
            let busy = busy.or_else(|| {
                o.get("slots")
                    .and_then(Value::as_array)
                    .map(|a| a.iter().any(slot_busy))
            });
            let queued = ["queued", "waiting", "pending"]
                .iter()
                .find_map(|k| o.get(*k).and_then(num))
                .or_else(|| {
                    ["queued", "waiting", "pending", "queue"]
                        .iter()
                        .find_map(|k| o.get(*k).and_then(Value::as_array).map(|a| a.len() as u64))
                });
            (busy, queued)
        }
        Value::Array(a) => (Some(a.iter().any(slot_busy)), Some(a.len() as u64)),
        _ => (None, None),
    };
    // No busy word anywhere: a queue count still says something is holding or waiting. Where both
    // answers exist they add up: a server that calls itself free but lists two waiting requests is
    // about to be busy.
    let busy = match (busy, queued.map(|q| q > 0)) {
        (Some(b), Some(waits)) => Some(b || waits),
        (b, w) => b.or(w),
    }?;
    Some(QueueInfo { busy, queued })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn variant_names_carry_their_window_and_read_back() {
        for (base, ctx, variant) in [
            ("qwen3:8b", 65_536, "qwen3:8b-helm-64k"),
            ("llama3:latest", 32_768, "llama3-helm-32k"),
            ("hf.co/u/m:Q4_K_M", 131_072, "hf.co/u/m:Q4_K_M-helm-128k"),
        ] {
            assert_eq!(variant_name(base, ctx), variant);
            assert_eq!(variant_context(variant), Some(ctx));
            assert_eq!(variant_base(variant), norm(base));
            assert!(is_variant(variant));
            // Ollama lists a tagless variant with :latest; it is still the same variant.
            assert!(is_variant(&format!("{variant}:latest")));
        }
        for plain in ["qwen3:8b", "llama3:latest", "x-helm-foo", "x-helm-0k"] {
            assert!(!is_variant(plain), "{plain}");
            assert_eq!(variant_base(plain), norm(plain));
        }
    }

    #[test]
    fn loaded_and_listed_forms_are_read_leniently() {
        use serde_json::json;
        for (v, want) in [
            (json!("qwen"), vec!["qwen"]),
            (json!(true), vec!["*"]),
            (json!(false), vec![]),
            (json!(null), vec![]),
            (json!(["a", {"model": "b"}, 3]), vec!["a", "b"]),
            (json!({"name": "c"}), vec!["c"]),
            (json!({"x": 1}), vec!["*"]),
        ] {
            assert_eq!(loaded_names(Some(&v)), want, "{v}");
        }
        assert_eq!(loaded_names(None), Vec::<String>::new());
        let l =
            parse_model_list(&json!({"data": [{"id": "m1", "max_model_len": 262144}, "m2", 5]}))
                .unwrap();
        assert_eq!(
            l,
            vec![
                Listed {
                    id: "m1".into(),
                    context: Some(262_144)
                },
                Listed {
                    id: "m2".into(),
                    context: None
                }
            ]
        );
        assert_eq!(parse_model_list(&json!(["a"])).unwrap().len(), 1);
        assert!(parse_model_list(&json!({"data": []})).is_none());
        assert!(parse_model_list(&json!("x")).is_none());
        // `/slots` queue facts, read the same lenient way.
        for (v, want) in [
            (
                json!({"busy": true, "queued": 2}),
                Some(QueueInfo {
                    busy: true,
                    queued: Some(2),
                }),
            ),
            (
                json!({"serving": "yes"}),
                Some(QueueInfo {
                    busy: true,
                    queued: None,
                }),
            ),
            (
                json!({"active": 1, "pending": "3"}),
                Some(QueueInfo {
                    busy: true,
                    queued: None,
                }),
            ),
            (
                json!({"busy": false, "queue": ["a", "b"]}),
                Some(QueueInfo {
                    busy: true,
                    queued: Some(2),
                }),
            ),
            (
                json!({"idle": true}),
                Some(QueueInfo {
                    busy: false,
                    queued: None,
                }),
            ),
            (
                json!({"slots": [{"id": "a", "state": "idle"}, {"id": "b", "state": "generating"}]}),
                Some(QueueInfo {
                    busy: true,
                    queued: None,
                }),
            ),
            (
                json!([{"state": "serving"}]),
                Some(QueueInfo {
                    busy: true,
                    queued: Some(1),
                }),
            ),
            (
                json!({"queued": 4}),
                Some(QueueInfo {
                    busy: true,
                    queued: Some(4),
                }),
            ),
            (json!({}), None),
            (json!("waiting"), None),
        ] {
            assert_eq!(read_queue(&v), want, "{v}");
        }
    }
}
