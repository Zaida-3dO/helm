//! What Helm asks of each kind of server.
//!
//! **Ollama** (`/api/*`): the installed models (`tags`), what is loaded (`ps`), a model's own
//! context length (`show`), a Helm variant of a model with a chosen context window (`create`,
//! `delete`), and loading or unloading through `generate` with no prompt.
//!
//! **Strata** (an Anthropic-style model server): `/health` says what is loaded, a model list at
//! `/v1/models` (or `/models`), and `/unload` frees its memory. The model loads when a request
//! for it arrives, so Helm never loads one itself.
//!
//! Every answer is read leniently: a field that is missing or shaped differently is skipped, not an
//! error, since these servers differ in small ways between versions.

use crate::net::{self, Conn, Http, NetError, Reply};
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
            Self::Refused => format!("{who} refused the key. Set it again in Settings > Local models."),
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
            v.get("error")
                .and_then(|e| e.as_str().map(str::to_owned).or_else(|| e.get("message")?.as_str().map(str::to_owned)))
        })
        .unwrap_or_default();
    Err(CallError::Status(r.status, msg.chars().take(200).collect()))
}

fn num(v: &Value) -> Option<u64> {
    v.as_u64().or_else(|| v.as_f64().filter(|f| *f >= 0.0).map(|f| f as u64))
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
    let v = r.json().ok_or_else(|| CallError::Shape("not JSON".into()))?;
    Ok(v.get("models")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|m| {
                    Some(Installed {
                        name: m.get("name").or_else(|| m.get("model"))?.as_str()?.to_owned(),
                        size: m.get("size").and_then(num).unwrap_or(0),
                    })
                })
                .collect()
        })
        .unwrap_or_default())
}

pub fn ollama_ps(h: &Http, c: &Conn) -> Result<Vec<Resident>, CallError> {
    let r = good(h.get(c, "/api/ps", net::QUICK)?)?;
    let v = r.json().ok_or_else(|| CallError::Shape("not JSON".into()))?;
    Ok(v.get("models")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|m| {
                    Some(Resident {
                        name: m.get("name").or_else(|| m.get("model"))?.as_str()?.to_owned(),
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
pub fn ollama_create(h: &Http, c: &Conn, variant: &str, from: &str, num_ctx: u32) -> Result<(), CallError> {
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
    Ok(loaded_names(v.get("loaded")))
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

/// Strata's model list: `/v1/models`, then `/models`. `None` when neither gives one.
pub fn strata_models(h: &Http, c: &Conn) -> Option<Vec<Listed>> {
    for path in ["/v1/models", "/models"] {
        let Ok(r) = h.get(c, path, net::QUICK) else { continue };
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
            Value::String(s) => Some(Listed { id: s.clone(), context: None }),
            Value::Object(o) => {
                let id = ["id", "name", "model"]
                    .iter()
                    .find_map(|k| o.get(*k).and_then(Value::as_str))?;
                let context = ["context_length", "max_model_len", "context_window", "max_context"]
                    .iter()
                    .find_map(|k| o.get(*k).and_then(num));
                Some(Listed { id: id.to_owned(), context })
            }
            _ => None,
        })
        .collect();
    (!list.is_empty()).then_some(list)
}

pub fn strata_unload(h: &Http, c: &Conn) -> Result<(), CallError> {
    good(h.post(c, "/unload", &json!({}), net::MANAGE)?).map(|_| ())
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
        let l = parse_model_list(&json!({"data": [{"id": "m1", "max_model_len": 262144}, "m2", 5]})).unwrap();
        assert_eq!(l, vec![Listed { id: "m1".into(), context: Some(262_144) }, Listed { id: "m2".into(), context: None }]);
        assert_eq!(parse_model_list(&json!(["a"])).unwrap().len(), 1);
        assert!(parse_model_list(&json!({"data": []})).is_none());
        assert!(parse_model_list(&json!("x")).is_none());
    }
}
