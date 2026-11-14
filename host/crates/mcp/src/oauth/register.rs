//! The client identity used at a server: either one the server handed out when asked, or one
//! the person entered once. Only the id is public; the secret, when there is one, is never
//! printed and lives only inside the credential store.

use super::discovery::AuthServer;
use super::{endpoint_allowed, OauthError};
use serde::{Deserialize, Serialize};
use serde_json::json;

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Client {
    pub client_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_secret: Option<String>,
}

impl Client {
    pub fn pre_registered(client_id: &str, secret: Option<&str>) -> Self {
        Self {
            client_id: client_id.to_owned(),
            client_secret: secret.map(str::to_owned),
        }
    }
}

/// The id and secret are never printed; only their presence is.
impl std::fmt::Debug for Client {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Client")
            .field("client_id", &self.client_id)
            .field("secret", &self.client_secret.as_ref().map(|_| "<set>"))
            .finish()
    }
}

/// Ask a server that publishes a registration address for a client id.
pub fn register(
    agent: &ureq::Agent,
    server: &AuthServer,
    redirect_uri: &str,
) -> Result<Client, OauthError> {
    let Some(reg) = server.registration_endpoint.as_deref() else {
        return Err(OauthError::NoRegistration);
    };
    let Ok(reg_url) = url::Url::parse(reg) else {
        return Err(OauthError::BadMetadata);
    };
    if !endpoint_allowed(&reg_url) {
        return Err(OauthError::InsecureUrl);
    }
    let body = json!({
        "client_name": "Helm",
        "redirect_uris": [redirect_uri],
        "grant_types": ["authorization_code", "refresh_token"],
        "response_types": ["code"],
        "token_endpoint_auth_method": "client_secret_basic"
    });
    let mut res = match agent
        .post(reg)
        .header("Content-Type", "application/json")
        .header("Accept", "application/json")
        .send(body.to_string())
    {
        Ok(r) => r,
        Err(ureq::Error::StatusCode(code)) => return Err(OauthError::Unreachable(code)),
        Err(_) => return Err(OauthError::Connection),
    };
    if !(200..300).contains(&res.status().as_u16()) {
        return Err(OauthError::Unreachable(res.status().as_u16()));
    }
    let body = res
        .body_mut()
        .with_config()
        .limit(64 * 1024)
        .read_to_string()
        .map_err(|_| OauthError::BadMetadata)?;
    // The answer's own fields are read without echoing the body: a refusal says only that.
    #[derive(Deserialize)]
    struct Reg {
        client_id: String,
        #[serde(default)]
        client_secret: Option<String>,
    }
    let reg: Reg = serde_json::from_str(&body).map_err(|_| OauthError::BadMetadata)?;
    if reg.client_id.is_empty() {
        return Err(OauthError::BadMetadata);
    }
    Ok(Client {
        client_id: reg.client_id,
        client_secret: reg.client_secret.filter(|s| !s.is_empty()),
    })
}
