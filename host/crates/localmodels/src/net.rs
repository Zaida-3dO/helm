//! Requests to a local model server. Blocking, on loopback only, and never through a proxy: a
//! proxy setting in the environment must not see a server's key or carry a local request away.
//! Redirects are off and a status code is an answer, not an error.

use serde_json::Value;
use std::time::Duration;

/// A short answer is expected from a catalogue or health request.
pub const QUICK: Duration = Duration::from_secs(2);
/// An unload or a small management call.
pub const MANAGE: Duration = Duration::from_secs(15);
/// Making a variant of a model (a metadata change, but it reads the model).
pub const CREATE: Duration = Duration::from_secs(120);
/// Loading a model into memory.
pub const LOAD: Duration = Duration::from_secs(300);

const MAX_BODY: u64 = 4 * 1024 * 1024;

/// The base address of a server that must not be contacted (the test override `off`). A request
/// to it fails as unreachable without opening a connection.
pub const OFF: &str = "off";

#[derive(Debug)]
pub struct Reply {
    pub status: u16,
    pub body: String,
}

impl Reply {
    pub fn ok(&self) -> bool {
        (200..300).contains(&self.status)
    }

    pub fn json(&self) -> Option<Value> {
        serde_json::from_str(&self.body).ok()
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum NetError {
    /// Nothing answered: refused, timed out or the host could not be found.
    Unreachable(String),
    Other(String),
}

impl std::fmt::Display for NetError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unreachable(m) | Self::Other(m) => f.write_str(m),
        }
    }
}

pub struct Http {
    agent: ureq::Agent,
}

impl Default for Http {
    fn default() -> Self {
        let agent = ureq::Agent::config_builder()
            .proxy(None)
            .max_redirects(0)
            .http_status_as_error(false)
            .timeout_global(Some(MANAGE))
            .user_agent("helm-home")
            .build()
            .new_agent();
        Self { agent }
    }
}

/// How to reach one server for one call.
#[derive(Clone)]
pub struct Conn {
    pub base: String,
    pub key: Option<String>,
}

impl std::fmt::Debug for Conn {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Conn")
            .field("base", &self.base)
            .field("key", &self.key.as_ref().map(|_| "<hidden>"))
            .finish()
    }
}

fn off(c: &Conn) -> Result<(), NetError> {
    if c.base == OFF {
        return Err(NetError::Unreachable(
            "local model servers are switched off for this run".into(),
        ));
    }
    Ok(())
}

fn map_err(e: ureq::Error) -> NetError {
    match e {
        ureq::Error::Io(_)
        | ureq::Error::Timeout(_)
        | ureq::Error::HostNotFound
        | ureq::Error::ConnectionFailed => NetError::Unreachable(e.to_string()),
        other => NetError::Other(other.to_string()),
    }
}

fn read(mut res: ureq::http::Response<ureq::Body>) -> Result<Reply, NetError> {
    let status = res.status().as_u16();
    let body = res
        .body_mut()
        .with_config()
        .limit(MAX_BODY)
        .read_to_string()
        .map_err(|e| NetError::Other(e.to_string()))?;
    Ok(Reply { status, body })
}

impl Http {
    fn auth(key: &Option<String>) -> Option<String> {
        key.as_ref().map(|k| format!("Bearer {k}"))
    }

    pub fn get(&self, c: &Conn, path: &str, timeout: Duration) -> Result<Reply, NetError> {
        off(c)?;
        let mut req = self
            .agent
            .get(format!("{}{path}", c.base))
            .header("Accept", "application/json");
        if let Some(a) = Self::auth(&c.key) {
            req = req.header("Authorization", &a);
        }
        let res = req
            .config()
            .timeout_global(Some(timeout))
            .build()
            .call()
            .map_err(map_err)?;
        read(res)
    }

    pub fn post(
        &self,
        c: &Conn,
        path: &str,
        body: &Value,
        timeout: Duration,
    ) -> Result<Reply, NetError> {
        off(c)?;
        let mut req = self
            .agent
            .post(format!("{}{path}", c.base))
            .header("Content-Type", "application/json");
        if let Some(a) = Self::auth(&c.key) {
            req = req.header("Authorization", &a);
        }
        let res = req
            .config()
            .timeout_global(Some(timeout))
            .build()
            .send(body.to_string())
            .map_err(map_err)?;
        read(res)
    }

    pub fn delete(
        &self,
        c: &Conn,
        path: &str,
        body: &Value,
        timeout: Duration,
    ) -> Result<Reply, NetError> {
        off(c)?;
        let mut req = self
            .agent
            .delete(format!("{}{path}", c.base))
            .force_send_body()
            .header("Content-Type", "application/json");
        if let Some(a) = Self::auth(&c.key) {
            req = req.header("Authorization", &a);
        }
        let res = req
            .config()
            .timeout_global(Some(timeout))
            .build()
            .send(body.to_string())
            .map_err(map_err)?;
        read(res)
    }
}
