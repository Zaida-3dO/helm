//! Turning a search query into a vector.
//!
//! The hub never embeds documents. A query is embedded by, in order:
//!
//! 1. the asker's own PC, over the relay line: an `ix_embed` frame, answered with `ix_embedded`
//!    within [`PC_TIMEOUT`] (a PC whose local models are busy answers `locked`);
//! 2. the embedding server the hub is pointed at (`HUB_EMBED_URL`, OpenAI-style
//!    `/v1/embeddings`), within [`NAS_TIMEOUT`], one request at a time;
//! 3. nobody: the search then runs on keywords alone and says why (`off: <reason>`).
//!
//! Queries go out as `search_query: <text>` and documents carry no prefix, the convention the
//! model was trained with. The frame's `text` is already prefixed: the PC embeds it as it is.
//! The last 512 answers are kept, so a repeated query costs nothing.

use crate::lines::Lines;
use crate::relay::{CallOutcome, Channel};
use serde_json::{json, Value};
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Duration;

pub const PC_TIMEOUT: Duration = Duration::from_millis(600);
pub const NAS_TIMEOUT: Duration = Duration::from_millis(2500);
const CACHE: usize = 512;
pub const DEFAULT_MODEL: &str = "nomic-embed-text";
const QUERY_PREFIX: &str = "search_query: ";

#[derive(Debug, Clone)]
pub struct Embedded {
    pub vector: Vec<f32>,
    /// Where it came from: `pc` or `nas`.
    pub meaning: &'static str,
}

pub struct QueryEmbedder {
    lines: Arc<Lines>,
    url: Option<String>,
    model: String,
    pc_timeout: Duration,
    nas_timeout: Duration,
    cache: Mutex<VecDeque<(String, Embedded)>>,
    /// The NAS server takes one query at a time.
    gate: tokio::sync::Semaphore,
}

impl QueryEmbedder {
    pub fn new(lines: Arc<Lines>, url: Option<String>, model: &str) -> Self {
        let url = url
            .map(|u| u.trim().trim_end_matches('/').to_owned())
            .filter(|u| !u.is_empty());
        QueryEmbedder {
            lines,
            url: url.map(|u| {
                if u.ends_with("/embeddings") {
                    u
                } else if u.ends_with("/v1") {
                    format!("{u}/embeddings")
                } else {
                    format!("{u}/v1/embeddings")
                }
            }),
            model: crate::index::normalise_model(model),
            pc_timeout: PC_TIMEOUT,
            nas_timeout: NAS_TIMEOUT,
            cache: Mutex::new(VecDeque::new()),
            gate: tokio::sync::Semaphore::new(1),
        }
    }

    pub fn with_timeouts(mut self, pc: Duration, nas: Duration) -> Self {
        self.pc_timeout = pc;
        self.nas_timeout = nas;
        self
    }

    /// The model every vector the hub compares must come from.
    pub fn model(&self) -> &str {
        &self.model
    }

    /// The query's vector, or why there is none.
    pub async fn embed(&self, viewer: i64, text: &str) -> Result<Embedded, String> {
        let text = format!("{QUERY_PREFIX}{}", text.trim());
        if let Some(hit) = self.cached(&text) {
            return Ok(hit);
        }
        let mut why = None;
        if let Some(line) = self.lines.route_for(viewer, Channel::App) {
            match self.ask_pc(&line, &text).await {
                Ok(v) => return Ok(self.remember(text, v, "pc")),
                Err(e) => why = Some(e),
            }
        }
        let Some(url) = &self.url else {
            return Err(why.unwrap_or_else(|| "the PC is off and no embedder is configured".into()));
        };
        let v = self.ask_nas(url, &text).await?;
        Ok(self.remember(text, v, "nas"))
    }

    fn cached(&self, text: &str) -> Option<Embedded> {
        let cache = self.cache.lock().unwrap_or_else(|p| p.into_inner());
        cache
            .iter()
            .find(|(k, _)| k == text)
            .map(|(_, v)| v.clone())
    }

    fn remember(&self, text: String, vector: Vec<f32>, meaning: &'static str) -> Embedded {
        let e = Embedded { vector, meaning };
        let mut cache = self.cache.lock().unwrap_or_else(|p| p.into_inner());
        cache.push_back((text, e.clone()));
        if cache.len() > CACHE {
            cache.pop_front();
        }
        e
    }

    async fn ask_pc(&self, line: &crate::relay::Line, text: &str) -> Result<Vec<f32>, String> {
        let out = line
            .ix_request(
                self.pc_timeout,
                |id| json!({ "type": "ix_embed", "id": id, "text": text }),
            )
            .await;
        let CallOutcome::Ok(frame) = out else {
            return Err("the PC did not answer in time".into());
        };
        if frame.get("locked").and_then(Value::as_bool) == Some(true) {
            return Err("the PC's models are busy".into());
        }
        if let Some(m) = frame.get("model").and_then(Value::as_str) {
            if crate::index::normalise_model(m) != self.model {
                return Err("the PC embeds with a different model".into());
            }
        }
        numbers(frame.get("vec")).ok_or_else(|| "the PC sent no vector".into())
    }

    async fn ask_nas(&self, url: &str, text: &str) -> Result<Vec<f32>, String> {
        let wait = self.nas_timeout;
        let Ok(Ok(_permit)) = tokio::time::timeout(wait, self.gate.acquire()).await else {
            return Err("the embedder is busy".into());
        };
        let (url, model, text) = (url.to_owned(), self.model.clone(), text.to_owned());
        let call = crate::stt::blocking(move || {
            let agent = crate::stt::agent_with(wait);
            let mut resp = agent
                .post(&url)
                .send_json(json!({ "model": model, "input": text }))
                .map_err(|e| match e {
                    ureq::Error::Timeout(_) => "the embedder timed out".to_owned(),
                    _ => "the embedder is unreachable".to_owned(),
                })?;
            if !resp.status().is_success() {
                return Err("the embedder refused the query".into());
            }
            let body: Value = resp
                .body_mut()
                .read_json()
                .map_err(|_| "the embedder sent something unreadable".to_owned())?;
            numbers(body.pointer("/data/0/embedding"))
                .ok_or_else(|| "the embedder sent no vector".to_owned())
        });
        match tokio::time::timeout(wait, call).await {
            Ok(r) => r,
            Err(_) => Err("the embedder timed out".into()),
        }
    }
}

/// A JSON array of finite numbers, 1 to 4096 of them.
fn numbers(v: Option<&Value>) -> Option<Vec<f32>> {
    let out: Vec<f32> = v?
        .as_array()?
        .iter()
        .map(|x| x.as_f64().map(|f| f as f32))
        .collect::<Option<_>>()?;
    (!out.is_empty() && out.len() <= 4096 && out.iter().all(|x| x.is_finite())).then_some(out)
}
