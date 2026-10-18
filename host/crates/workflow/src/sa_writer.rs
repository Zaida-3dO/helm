//! Writing stage records to Agent Standup (the app is the recorder). The rows are
//! queued by [`crate::sa_outbox`]; this module sends them.
//!
//! - **Where and who**: the board address helm-standup already holds (its shared `BoardLink`),
//!   and the token from the same credential-store entry Settings → Projects writes. No address
//!   or token is written in code, tests or fakes here.
//! - **Only two routes**: `POST /api/items/{id}/artifacts` and `POST /api/items/{id}/transition`,
//!   plus `GET` reads of the same item for the read-back.
//! - **Outcome unknown**: a request that may have reached the board (a timeout or a dropped
//!   connection after sending, or a 5xx) is never blindly resent, because artifacts cannot be
//!   deleted. The next pass reads the item back first: an artifact whose body starts with the
//!   row's marker, or an item already in the target state, means it landed.
//! - Redirects are never followed, the token is never logged, and responses are capped at 1 MiB.

use serde_json::Value;
use std::time::Duration;

pub const TIMEOUT: Duration = Duration::from_secs(15);
pub const MAX_RESPONSE: u64 = 1024 * 1024;
/// Same entry `helm-projects` writes from Settings → Projects (`board.rs`).
pub const CRED_NAME: &str = "agent-standup";
pub const CRED_IDENTITY: &str = "agent-standup-token";

#[derive(Debug)]
pub enum SendError {
    /// Nothing reached the board (connection refused, DNS): safe to resend.
    NotSent(String),
    /// It may have reached the board: read back before any resend.
    Unknown(String),
    /// The board answered and refused: do not resend.
    Refused(u16, String),
}

pub struct Response {
    pub status: u16,
    pub body: String,
}

/// The one way a request leaves this module.
pub trait WriteTransport: Send + Sync {
    fn send(
        &self,
        method: &str,
        url: &str,
        bearer: &str,
        body: Option<&str>,
    ) -> Result<Response, SendError>;
}

/// The board connection: its address and token.
#[derive(Clone)]
pub struct Target {
    pub base: String,
    token: String,
}

impl std::fmt::Debug for Target {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Target")
            .field("base", &self.base)
            .finish_non_exhaustive()
    }
}

impl Target {
    pub fn new(base: &str, token: &str) -> Option<Self> {
        let base = helm_standup::normalize_url(base).ok()?;
        let token = token.trim();
        (!token.is_empty()).then(|| Self {
            base,
            token: token.to_owned(),
        })
    }

    fn scrub(&self, s: &str) -> String {
        if self.token.len() < 4 {
            s.to_owned()
        } else {
            s.replace(&self.token, "[token]")
        }
    }

    fn call(
        &self,
        t: &dyn WriteTransport,
        method: &str,
        path: &str,
        body: Option<&Value>,
    ) -> Result<Value, SendError> {
        let url = format!("{}{}", self.base, path);
        let text = body.map(Value::to_string);
        let r = t
            .send(method, &url, &self.token, text.as_deref())
            .map_err(|e| match e {
                SendError::NotSent(m) => SendError::NotSent(self.scrub(&m)),
                SendError::Unknown(m) => SendError::Unknown(self.scrub(&m)),
                SendError::Refused(s, m) => SendError::Refused(s, self.scrub(&m)),
            })?;
        match r.status {
            // A read that does not parse is not "not found": the read-back must not resend on it.
            200..=299 if method == "GET" => serde_json::from_str(&r.body)
                .map_err(|_| SendError::Unknown("the board's reply could not be read".into())),
            200..=299 => Ok(serde_json::from_str(&r.body).unwrap_or(Value::Null)),
            500..=599 => Err(SendError::Unknown(format!(
                "the board answered {}",
                r.status
            ))),
            s => Err(SendError::Refused(
                s,
                self.scrub(&r.body.chars().take(300).collect::<String>()),
            )),
        }
    }

    pub fn artifact(
        &self,
        t: &dyn WriteTransport,
        item: &str,
        body: &Value,
    ) -> Result<Value, SendError> {
        self.call(
            t,
            "POST",
            &format!("/api/items/{}/artifacts", enc(item)),
            Some(body),
        )
    }

    pub fn transition(
        &self,
        t: &dyn WriteTransport,
        item: &str,
        body: &Value,
    ) -> Result<Value, SendError> {
        self.call(
            t,
            "POST",
            &format!("/api/items/{}/transition", enc(item)),
            Some(body),
        )
    }

    /// The item's artifacts of one kind (for the read-back).
    pub fn artifacts(
        &self,
        t: &dyn WriteTransport,
        item: &str,
        kind: &str,
    ) -> Result<Value, SendError> {
        self.call(
            t,
            "GET",
            &format!(
                "/api/items/{}/artifacts?kind={}&limit=50",
                enc(item),
                enc(kind)
            ),
            None,
        )
    }

    pub fn item(&self, t: &dyn WriteTransport, item: &str) -> Result<Value, SendError> {
        self.call(t, "GET", &format!("/api/items/{}", enc(item)), None)
    }
}

fn enc(s: &str) -> String {
    s.bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.') {
                (b as char).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect()
}

/// The real transport: ureq, redirects off, statuses returned rather than raised.
pub struct HttpWrites {
    agent: ureq::Agent,
}

impl Default for HttpWrites {
    fn default() -> Self {
        let agent = ureq::Agent::config_builder()
            .max_redirects(0)
            .http_status_as_error(false)
            .timeout_global(Some(TIMEOUT))
            .user_agent("helm-home")
            .build()
            .new_agent();
        Self { agent }
    }
}

impl WriteTransport for HttpWrites {
    fn send(
        &self,
        method: &str,
        url: &str,
        bearer: &str,
        body: Option<&str>,
    ) -> Result<Response, SendError> {
        let auth = format!("Bearer {bearer}");
        let res = match (method, body) {
            ("GET", _) => self
                .agent
                .get(url)
                .header("Authorization", &auth)
                .header("Accept", "application/json")
                .call(),
            (_, b) => self
                .agent
                .post(url)
                .header("Authorization", &auth)
                .header("Content-Type", "application/json")
                .send(b.unwrap_or("{}")),
        };
        let mut res = match res {
            Ok(r) => r,
            Err(ureq::Error::HostNotFound | ureq::Error::ConnectionFailed) => {
                return Err(SendError::NotSent("could not reach the board".into()))
            }
            // Refused before a connection existed: nothing was sent.
            Err(ureq::Error::Io(e))
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::ConnectionRefused | std::io::ErrorKind::AddrNotAvailable
                ) =>
            {
                return Err(SendError::NotSent(
                    "the board refused the connection".into(),
                ))
            }
            Err(e) if method == "GET" => return Err(SendError::NotSent(e.to_string())),
            Err(e) => return Err(SendError::Unknown(e.to_string())),
        };
        let status = res.status().as_u16();
        let body = res
            .body_mut()
            .with_config()
            .limit(MAX_RESPONSE)
            .read_to_string()
            .unwrap_or_default();
        Ok(Response { status, body })
    }
}

/// The board target from the app's services: helm-standup's address and the stored token. Debug
/// builds honour the same test variables as Settings → Projects.
pub fn target(services: &helm_ipc::Services) -> Option<Target> {
    if cfg!(debug_assertions) {
        if let (Ok(u), Ok(t)) = (
            std::env::var("HELM_TEST_STANDUP_URL"),
            std::env::var("HELM_TEST_STANDUP_TOKEN"),
        ) {
            if !u.trim().is_empty() {
                return Target::new(&u, &t);
            }
        }
    }
    let base = services
        .get::<helm_standup::BoardLink>()?
        .attach_only()
        .ok()?
        .base()
        .to_owned();
    let profile = services.get::<helm_core::profile::Profile>()?;
    let platform = services.get::<helm_platform::Platform>()?;
    let creds = helm_core::credentials::CredentialStore::new(
        &profile.path("credentials"),
        &profile.hash,
        platform.keychain.clone(),
    );
    let token = String::from_utf8(creds.read(CRED_NAME, CRED_IDENTITY).ok()?).ok()?;
    Target::new(&base, &token)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_target_never_prints_its_token() {
        let t = Target::new("http://127.0.0.1:9/", "secret-value").unwrap();
        assert_eq!(t.base, "http://127.0.0.1:9");
        assert!(!format!("{t:?}").contains("secret-value"));
        assert_eq!(t.scrub("x secret-value y"), "x [token] y");
        assert!(Target::new("ftp://x", "t").is_none());
        assert!(Target::new("http://x", "  ").is_none());
    }

    #[test]
    fn item_ids_are_encoded_into_the_path() {
        assert_eq!(enc("ab-12"), "ab-12");
        assert_eq!(enc("../x"), "..%2Fx");
    }

    /// The real transport classifies a refused connection as not sent, and a connection dropped
    /// after the request went out as unknown (so it is read back, never blindly resent).
    #[test]
    fn the_http_transport_tells_not_sent_from_unknown() {
        use std::io::Read;
        let free = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let closed = free.local_addr().unwrap();
        drop(free);
        let t = HttpWrites::default();
        let r = t.send(
            "POST",
            &format!("http://{closed}/api/items/x/artifacts"),
            "tok",
            Some("{}"),
        );
        assert!(
            matches!(r, Err(SendError::NotSent(_))),
            "refused connection"
        );
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = l.local_addr().unwrap();
        let h = std::thread::spawn(move || {
            let (mut s, _) = l.accept().unwrap();
            let mut buf = [0u8; 4096];
            let _ = s.read(&mut buf);
            drop(s); // read the request, never answer
        });
        let r = t.send(
            "POST",
            &format!("http://{addr}/api/items/x/artifacts"),
            "tok",
            Some("{}"),
        );
        h.join().unwrap();
        assert!(
            matches!(r, Err(SendError::Unknown(_))),
            "dropped after sending"
        );
    }
}
