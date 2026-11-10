//! The one write this client makes: a board project's title, after the user renames the project in
//! Helm.
//!
//! It is kept apart from `lib.rs` and `reads.rs`, which stay read-only (a test scans them). The
//! write goes through its own seam, [`TitleTransport`], so nothing else can borrow it, and it
//! follows the same rules as a read where they apply: redirects are never followed, the token is
//! scrubbed from anything that is returned, responses are capped, and the call has its own short
//! deadline ([`TITLE_TIMEOUT`]). It is not retried here: the caller keeps the rename pending and
//! sends it again after the next sync.

use crate::reads::check_id;
use crate::{encode, Client, RawResponse, StandupError, TransportError, MAX_RESPONSE_BYTES};
use serde_json::json;
use std::time::Duration;

/// How long a title write may take.
pub const TITLE_TIMEOUT: Duration = Duration::from_secs(10);
/// The longest title sent, in characters.
pub const MAX_TITLE_CHARS: usize = 200;

/// The one way a title write leaves this crate.
pub trait TitleTransport: Send + Sync {
    fn patch_json(
        &self,
        url: &str,
        bearer: &str,
        body: &str,
        timeout: Duration,
    ) -> Result<RawResponse, TransportError>;
}

/// The real transport: ureq, redirects off, status codes returned rather than raised.
pub struct HttpTitleTransport {
    agent: ureq::Agent,
}

impl Default for HttpTitleTransport {
    fn default() -> Self {
        let agent = ureq::Agent::config_builder()
            .max_redirects(0)
            .http_status_as_error(false)
            .timeout_global(Some(TITLE_TIMEOUT))
            .user_agent("helm-home")
            .build()
            .new_agent();
        Self { agent }
    }
}

impl TitleTransport for HttpTitleTransport {
    fn patch_json(
        &self,
        url: &str,
        bearer: &str,
        body: &str,
        timeout: Duration,
    ) -> Result<RawResponse, TransportError> {
        let res = self
            .agent
            .patch(url)
            .header("Authorization", &format!("Bearer {bearer}"))
            .header("Content-Type", "application/json")
            .header("Accept", "application/json")
            .config()
            .timeout_global(Some(timeout))
            .build()
            .send(body);
        let mut res = match res {
            Ok(r) => r,
            Err(ureq::Error::StatusCode(s)) => {
                return Ok(RawResponse {
                    status: s,
                    body: String::new(),
                })
            }
            Err(
                e @ (ureq::Error::Io(_)
                | ureq::Error::Timeout(_)
                | ureq::Error::HostNotFound
                | ureq::Error::ConnectionFailed),
            ) => return Err(TransportError::Unreachable(e.to_string())),
            Err(e) => return Err(TransportError::Other(e.to_string())),
        };
        let status = res.status().as_u16();
        let body = res
            .body_mut()
            .with_config()
            .limit(MAX_RESPONSE_BYTES)
            .read_to_string()
            .map_err(|e| match e {
                ureq::Error::BodyExceedsLimit(_) => TransportError::TooLarge,
                other => TransportError::Other(other.to_string()),
            })?;
        Ok(RawResponse { status, body })
    }
}

impl Client {
    /// Give the board project `board_project_id` a new title: `PATCH /api/items/<id>` with
    /// `{"title": …}`. The title is one line of 1 to [`MAX_TITLE_CHARS`] characters.
    pub fn set_title(&self, board_project_id: &str, title: &str) -> Result<(), StandupError> {
        let id = check_id(board_project_id)?;
        let title = title.trim();
        if title.is_empty()
            || title.chars().count() > MAX_TITLE_CHARS
            || title.chars().any(char::is_control)
        {
            return Err(StandupError::Invalid(
                "a title is one line of 1 to 200 characters".to_owned(),
            ));
        }
        let url = format!("{}/api/items/{}", self.base, encode(id));
        let body = json!({ "title": title }).to_string();
        let res = self
            .title_transport
            .patch_json(&url, self.token.expose(), &body, TITLE_TIMEOUT)
            .map_err(|e| match e {
                TransportError::Unreachable(m) => StandupError::Unreachable(self.token.scrub(&m)),
                TransportError::TooLarge => StandupError::TooLarge,
                TransportError::Other(m) => StandupError::BadResponse(self.token.scrub(&m)),
            })?;
        match res.status {
            200..=299 => Ok(()),
            300..=399 => Err(StandupError::Redirected),
            401 | 403 => Err(StandupError::Unauthorized),
            s => Err(StandupError::Status(s)),
        }
    }
}
