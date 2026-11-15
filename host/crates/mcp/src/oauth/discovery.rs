//! Finding the sign-in and token addresses from the server's own published documents.
//!
//! Order of work: the `WWW-Authenticate` hint's `resource_metadata` address, else the protected
//! resource document on the server's own address, else the authorization server documents on the
//! server's address directly (servers published before the resource document existed). Then the
//! authorization server's own document, falling back to the OpenID Connect one. Every document
//! is size-capped, and anything that is not readable JSON with the parts needed is a plain
//! refusal — a garbled document never becomes a half-configured sign-in.

use super::{endpoint_allowed, OauthError};
use url::Url;

const MAX_DOC: u64 = 64 * 1024;
const RESOURCE_WELL_KNOWN: &str = ".well-known/oauth-protected-resource";
const AS_WELL_KNOWN: &str = ".well-known/oauth-authorization-server";
const OIDC_WELL_KNOWN: &str = ".well-known/openid-configuration";

/// Where a server's sign-in lives, as the server itself published it.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct AuthServer {
    pub issuer: String,
    pub authorization_endpoint: String,
    pub token_endpoint: String,
    /// Present when the server hands out client ids by itself.
    pub registration_endpoint: Option<String>,
}

/// The documents the server published, for a protected server at `server_url`.
/// `www_authenticate` is the raw header from a `401`, used when it carries a
/// `resource_metadata` address.
pub fn discover(
    agent: &ureq::Agent,
    server_url: &str,
    www_authenticate: Option<&str>,
) -> Result<AuthServer, OauthError> {
    let base = Url::parse(server_url).map_err(|_| OauthError::BadUrl)?;
    if !endpoint_allowed(&base) {
        return Err(OauthError::InsecureUrl);
    }

    // 1. Protected resource metadata: the hint's address, else the well-known one on the
    //    server's address (with its path, then without).
    let mut candidates: Vec<Url> = Vec::new();
    if let Some(hint) = hint_url(www_authenticate) {
        candidates.push(hint);
    } else {
        candidates.push(well_known(&base, RESOURCE_WELL_KNOWN));
        let mut bare = well_known(&base, RESOURCE_WELL_KNOWN);
        bare.set_path(&format!("/{RESOURCE_WELL_KNOWN}"));
        candidates.push(bare);
    }
    let mut issuer: Option<Url> = None;
    for prm_url in candidates {
        if let Some(doc) = get_json(agent, &prm_url)? {
            // The document must be about this very server (RFC 9728 section 3.3): a resource
            // document that names another one is not trusted for this address.
            let names = doc.get("resource").and_then(|v| v.as_str());
            if !names.is_some_and(|n| same_address(n, base.as_str())) {
                return Err(OauthError::BadMetadata);
            }
            issuer = resource_issuer(&doc)?;
            break;
        }
    }
    // 2. No resource document: the server's own origin is the authorization server.
    let issuer = match issuer {
        Some(i) => i,
        None => {
            let mut origin = base.clone();
            origin.set_path("/");
            origin.set_query(None);
            origin.set_fragment(None);
            origin
        }
    };

    // 3. The authorization server's document, falling back to the OpenID Connect one.
    let mut doc = get_json(agent, &well_known(&issuer, AS_WELL_KNOWN))?;
    if doc.is_none() {
        let mut oidc = issuer.clone();
        let path = oidc.path().trim_end_matches('/').to_owned();
        oidc.set_path(&format!("{path}/{OIDC_WELL_KNOWN}"));
        doc = get_json(agent, &oidc)?;
    }
    as_server(&doc.ok_or(OauthError::NoAuthServer)?, &issuer)
}

/// The authorization server named by a protected resource document, first one that is usable.
fn resource_issuer(doc: &serde_json::Value) -> Result<Option<Url>, OauthError> {
    let Some(list) = doc.get("authorization_servers") else {
        return Err(OauthError::BadMetadata);
    };
    let Some(list) = list.as_array() else {
        return Err(OauthError::BadMetadata);
    };
    for entry in list {
        let Some(raw) = entry.as_str() else { continue };
        let Ok(url) = Url::parse(raw) else { continue };
        if !endpoint_allowed(&url) {
            return Err(OauthError::InsecureUrl);
        }
        return Ok(Some(url));
    }
    Err(OauthError::BadMetadata)
}

/// Whether two addresses are the same place, ignoring a trailing slash.
pub(crate) fn same_address(a: &str, b: &str) -> bool {
    match (Url::parse(a), Url::parse(b)) {
        (Ok(a), Ok(b)) => a.as_str().trim_end_matches('/') == b.as_str().trim_end_matches('/'),
        _ => false,
    }
}

/// The endpoints from an authorization server document (same shape in both families).
fn as_server(doc: &serde_json::Value, issuer: &Url) -> Result<AuthServer, OauthError> {
    // The document must name the issuer it was fetched for (RFC 8414 section 3.3), or a server
    // could pass off another one's endpoints.
    let named = doc.get("issuer").and_then(|v| v.as_str());
    if !named.is_some_and(|n| same_address(n, issuer.as_str())) {
        return Err(OauthError::BadMetadata);
    }
    let field = |name: &str| -> Result<String, OauthError> {
        let raw = doc
            .get(name)
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .ok_or(OauthError::BadMetadata)?;
        let url = Url::parse(raw).map_err(|_| OauthError::BadMetadata)?;
        if !endpoint_allowed(&url) || url.host_str() != issuer.host_str() {
            return Err(OauthError::InsecureUrl);
        }
        Ok(raw.to_owned())
    };
    let registration_endpoint = doc
        .get("registration_endpoint")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .and_then(|raw| {
            let url = Url::parse(raw).ok()?;
            (endpoint_allowed(&url) && url.host_str() == issuer.host_str()).then(|| raw.to_owned())
        });
    Ok(AuthServer {
        issuer: issuer.to_string(),
        authorization_endpoint: field("authorization_endpoint")?,
        token_endpoint: field("token_endpoint")?,
        registration_endpoint,
    })
}

/// `https://host/a/b` + `.well-known/x` → `https://host/.well-known/x/a/b`.
fn well_known(base: &Url, middle: &str) -> Url {
    let mut u = base.clone();
    let path = base.path().trim_end_matches('/').to_owned();
    u.set_path(&format!("/{middle}{path}"));
    u.set_query(None);
    u.set_fragment(None);
    u
}

/// The `resource_metadata="..."` address from a `WWW-Authenticate` header, if it carries one.
fn hint_url(header: Option<&str>) -> Option<Url> {
    let header = header?;
    let lower = header.to_ascii_lowercase();
    // Lower-casing must not move byte offsets (it can with non-ASCII text).
    if lower.len() != header.len() {
        return None;
    }
    let at = lower.find("resource_metadata=\"")? + "resource_metadata=\"".len();
    let end = header[at..].find('"')?;
    Url::parse(&header[at..at + end]).ok()
}

/// Fetch a JSON document, capped. `Ok(None)` means "answered it is not there" (404/403).
pub(crate) fn get_json(
    agent: &ureq::Agent,
    url: &Url,
) -> Result<Option<serde_json::Value>, OauthError> {
    if !endpoint_allowed(url) {
        return Err(OauthError::InsecureUrl);
    }
    let mut res = match agent
        .get(url.as_str())
        .header("Accept", "application/json")
        .call()
    {
        Ok(r) => r,
        // The address was answered but says the document is not there.
        Err(ureq::Error::StatusCode(404 | 403)) => return Ok(None),
        Err(ureq::Error::StatusCode(code)) => return Err(OauthError::Unreachable(code)),
        Err(_) => return Err(OauthError::Connection),
    };
    let status = res.status().as_u16();
    if !(200..300).contains(&status) {
        return Err(OauthError::Unreachable(status));
    }
    let body = res
        .body_mut()
        .with_config()
        .limit(MAX_DOC)
        .read_to_string()
        .map_err(|_| OauthError::BadMetadata)?;
    let doc: serde_json::Value =
        serde_json::from_str(&body).map_err(|_| OauthError::BadMetadata)?;
    if !doc.is_object() {
        return Err(OauthError::BadMetadata);
    }
    Ok(Some(doc))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn well_known_paths_are_inserted_before_the_path() {
        let u = Url::parse("https://host/a/b").unwrap();
        assert_eq!(
            well_known(&u, AS_WELL_KNOWN).as_str(),
            "https://host/.well-known/oauth-authorization-server/a/b"
        );
        let u = Url::parse("https://host:9/").unwrap();
        assert_eq!(
            well_known(&u, RESOURCE_WELL_KNOWN).as_str(),
            "https://host:9/.well-known/oauth-protected-resource"
        );
    }

    #[test]
    fn the_hint_address_is_taken_from_the_header() {
        let h =
            "Bearer realm=\"x\", error=\"invalid_token\", resource_metadata=\"https://host/prm\"";
        assert_eq!(hint_url(Some(h)).unwrap().as_str(), "https://host/prm");
        assert!(hint_url(Some("Bearer error=\"invalid_token\"")).is_none());
        assert!(hint_url(None).is_none());
    }

    #[test]
    fn documents_are_read_for_their_endpoints_and_refused_otherwise() {
        let issuer = Url::parse("https://as.example/").unwrap();
        let good = serde_json::json!({
            "issuer": "https://as.example",
            "authorization_endpoint": "https://as.example/auth",
            "token_endpoint": "https://as.example/token",
            "registration_endpoint": "https://as.example/reg"
        });
        let s = as_server(&good, &issuer).unwrap();
        assert_eq!(s.token_endpoint, "https://as.example/token");
        assert_eq!(
            s.registration_endpoint.as_deref(),
            Some("https://as.example/reg")
        );
        // Missing pieces, non-https endpoints and another host are clean refusals.
        for bad in [
            serde_json::json!({"issuer": "https://as.example", "authorization_endpoint": "https://as.example/auth"}),
            serde_json::json!({
                "issuer": "https://as.example",
                "authorization_endpoint": "http://as.example/auth",
                "token_endpoint": "https://as.example/token"
            }),
            serde_json::json!({
                "issuer": "https://as.example",
                "authorization_endpoint": "https://elsewhere.example/auth",
                "token_endpoint": "https://as.example/token"
            }),
            serde_json::json!("not an object"),
            // A document that names another issuer than the one it was fetched for.
            serde_json::json!({
                "issuer": "https://elsewhere.example",
                "authorization_endpoint": "https://as.example/auth",
                "token_endpoint": "https://as.example/token"
            }),
        ] {
            assert!(as_server(&bad, &issuer).is_err(), "{bad}");
        }
    }
}
