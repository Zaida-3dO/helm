//! A fake authorization server for tests: an in-process server on this computer that answers
//! the same documents a real one publishes. It is compiled only for tests, and nothing here
//! ever talks to a real service.

use super::pkce;
use serde_json::json;
use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::{Arc, Mutex};

#[derive(Clone, Default)]
pub struct Cfg {
    /// The sign-in and the exchange must both carry the `resource` parameter.
    pub require_resource: bool,
    /// Only this client id may sign in, and it must answer with these credentials (Basic).
    pub expect_client: Option<(String, String)>,
    /// Never honour the PKCE proof: every code exchange fails.
    pub break_pkce: bool,
    /// Publish no protected-resource document at all.
    pub no_prm: bool,
    /// Skip the authorization-server document; answer only the OpenID one.
    pub oidc_only: bool,
    /// Also skip the OpenID document: nothing is published.
    pub hide_as_doc: bool,
    /// Do not answer the registration address, and do not advertise it.
    pub no_register: bool,
    /// Publish documents that are not JSON at all.
    pub garble_metadata: bool,
    /// Publish documents larger than any reader should swallow.
    pub huge_metadata: bool,
    /// A refresh answer leaves out the refresh token (a server that does not rotate them).
    pub no_refresh_echo: bool,
    /// Every refresh hands out a new refresh token and retires the one that was used.
    pub rotate_refresh: bool,
    /// The protected-resource document names a different server than the one asked about.
    pub wrong_resource: bool,
    /// The authorization-server document names a different issuer than the address it sits at.
    pub wrong_issuer: bool,
    /// Answer Google's own addresses (`/o/oauth2/v2/auth`, `/oauth2/token`) and enforce what
    /// Google's sign-in page is picky about (an offline `access_type`, a `consent` in `prompt`).
    pub google_shape: bool,
    /// The token answer for a sign-in code carries an `id_token` whose email claim is this.
    pub id_email: Option<String>,
}

#[derive(Clone)]
struct Grant {
    challenge: String,
    redirect: String,
    client: String,
    resource: Option<String>,
    scope: Option<String>,
    refresh_key: String,
}

struct State {
    codes: HashMap<String, Grant>,
    refreshes: HashMap<String, Grant>,
    codes_issued: u32,
    accesses_issued: u32,
    refreshes_served: u32,
    /// The `redirect_uri` of the last sign-in request that was accepted.
    last_redirect: Option<String>,
    /// Client ids this server handed out, with the redirect addresses each was registered for.
    registered: HashMap<String, Vec<String>>,
    /// The query of the last accepted sign-in-page request (empty for a server that never saw one).
    authorize: HashMap<String, String>,
}

pub struct Fake {
    pub base: String,
    state: Arc<Mutex<State>>,
}

impl Fake {
    pub fn start(cfg: Cfg) -> Fake {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let base = format!("http://127.0.0.1:{port}");
        let state = Arc::new(Mutex::new(State {
            codes: HashMap::new(),
            refreshes: HashMap::new(),
            codes_issued: 0,
            accesses_issued: 0,
            refreshes_served: 0,
            last_redirect: None,
            registered: HashMap::new(),
            authorize: HashMap::new(),
        }));
        let serve_state = state.clone();
        let serve_base = base.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { break };
                let _ = stream.set_read_timeout(Some(std::time::Duration::from_secs(5)));
                if let Some(req) = read_request(&mut stream) {
                    respond(&mut stream, &req, &serve_base, &cfg, &serve_state);
                }
                let _ = stream.flush();
                let _ = stream.shutdown(std::net::Shutdown::Write);
                // Let the client finish reading before the socket closes.
                let _ = stream.set_read_timeout(Some(std::time::Duration::from_millis(200)));
                let mut rest = Vec::new();
                let _ = stream.read_to_end(&mut rest);
            }
        });
        Fake { base, state }
    }

    /// How many refresh calls were answered.
    pub fn refreshes_served(&self) -> u32 {
        self.state.lock().unwrap().refreshes_served
    }

    /// The return address of the last sign-in request the fake accepted.
    pub fn last_redirect(&self) -> Option<String> {
        self.state.lock().unwrap().last_redirect.clone()
    }

    /// How many client ids were handed out.
    pub fn registrations(&self) -> usize {
        self.state.lock().unwrap().registered.len()
    }

    pub fn resource_url(&self) -> String {
        format!("{}/mcp", self.base)
    }

    /// The query parameters of the last sign-in-page request the fake accepted.
    pub fn authorize_params(&self) -> HashMap<String, String> {
        self.state.lock().unwrap().authorize.clone()
    }
}

struct Req {
    method: String,
    path: String,
    query: String,
    headers: HashMap<String, String>,
    body: String,
}

fn read_request(stream: &mut impl Read) -> Option<Req> {
    let mut raw = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        let n = stream.read(&mut chunk).ok()?;
        if n == 0 {
            break;
        }
        raw.extend_from_slice(&chunk[..n]);
        if raw.windows(4).any(|w| w == b"\r\n\r\n") || raw.len() > 64 * 1024 {
            break;
        }
    }
    let text = String::from_utf8_lossy(&raw).into_owned();
    let (head, body_start) = text.split_once("\r\n\r\n")?;
    let mut body = body_start.to_owned();
    let want = head
        .lines()
        .find_map(|l| {
            let l = l.to_ascii_lowercase();
            l.strip_prefix("content-length:")
                .map(|n| n.trim().to_owned())
        })
        .and_then(|n| n.parse::<usize>().ok())
        .unwrap_or(0);
    while body.len() < want {
        let n = stream.read(&mut chunk).ok()?;
        if n == 0 {
            break;
        }
        body.push_str(String::from_utf8_lossy(&chunk[..n]).as_ref());
    }
    let mut lines = head.lines();
    let mut words = lines.next()?.split_whitespace();
    let (method, target) = (words.next()?.to_owned(), words.next()?.to_owned());
    let (path, query) = target.split_once('?').unwrap_or((target.as_str(), ""));
    let mut headers = HashMap::new();
    for line in lines {
        if let Some((k, v)) = line.split_once(':') {
            headers.insert(k.trim().to_ascii_lowercase(), v.trim().to_owned());
        }
    }
    Some(Req {
        method,
        path: path.to_owned(),
        query: query.to_owned(),
        headers,
        body,
    })
}

fn respond(stream: &mut impl Write, req: &Req, base: &str, cfg: &Cfg, state: &Mutex<State>) {
    match (req.method.as_str(), req.path.as_str()) {
        ("GET", "/authorize") => authorize(stream, req, cfg, state),
        ("POST", "/token") => token(stream, req, cfg, state),
        // Google's own addresses (only reached when the caller points its fixed endpoints here).
        ("GET", "/o/oauth2/v2/auth") => authorize(stream, req, cfg, state),
        ("POST", "/oauth2/token") => token(stream, req, cfg, state),
        ("POST", "/register") => {
            if cfg.no_register {
                return send(stream, "404 Not Found", "application/json", "");
            }
            let asked: serde_json::Value = serde_json::from_str(&req.body).unwrap_or_default();
            let redirects: Vec<String> = asked["redirect_uris"]
                .as_array()
                .map(|l| {
                    l.iter()
                        .filter_map(|v| v.as_str().map(str::to_owned))
                        .collect()
                })
                .unwrap_or_default();
            let mut st = state.lock().unwrap();
            let id = format!("fake-client-{}", st.registered.len() + 1);
            st.registered.insert(id.clone(), redirects);
            send(
                stream,
                "200 OK",
                "application/json",
                &json!({
                    "client_id": id,
                    "client_secret": "fake-secret-9",
                    "token_endpoint_auth_method": "client_secret_basic"
                })
                .to_string(),
            )
        }
        ("GET", path)
            if path == "/prm" || path.starts_with("/.well-known/oauth-protected-resource") =>
        {
            if cfg.no_prm {
                return send(stream, "404 Not Found", "application/json", "");
            }
            let resource = if cfg.wrong_resource {
                format!("{base}/other")
            } else {
                format!("{base}/mcp")
            };
            let doc = json!({"resource": resource, "authorization_servers": [base]});
            metadata(stream, cfg, &doc.to_string())
        }
        // Documents for a server whose address carries a path keep it after the well-known part.
        ("GET", p) if p.starts_with("/.well-known/oauth-authorization-server") => {
            if cfg.oidc_only || cfg.hide_as_doc {
                return send(stream, "404 Not Found", "application/json", "");
            }
            metadata(stream, cfg, &as_document(base, cfg))
        }
        ("GET", p) if p.ends_with("/.well-known/openid-configuration") => {
            if cfg.hide_as_doc {
                return send(stream, "404 Not Found", "application/json", "");
            }
            metadata(stream, cfg, &as_document(base, cfg))
        }
        _ => send(stream, "404 Not Found", "text/plain", "not here"),
    }
}

fn as_document(base: &str, cfg: &Cfg) -> String {
    let issuer = if cfg.wrong_issuer {
        "https://elsewhere.example".to_owned()
    } else {
        base.to_owned()
    };
    let mut doc = json!({
        "issuer": issuer,
        "authorization_endpoint": format!("{base}/authorize"),
        "token_endpoint": format!("{base}/token"),
        "response_types_supported": ["code"],
        "code_challenge_methods_supported": ["S256"],
        "grant_types_supported": ["authorization_code", "refresh_token"]
    });
    if !cfg.no_register {
        doc["registration_endpoint"] = json!(format!("{base}/register"));
    }
    doc.to_string()
}

fn metadata(stream: &mut impl Write, cfg: &Cfg, body: &str) {
    if cfg.garble_metadata {
        return send(stream, "200 OK", "application/json", "this is not json {{{");
    }
    if cfg.huge_metadata {
        let mut padded = body.trim_end_matches('}').to_owned();
        padded.push_str(&format!(",\"padding\":\"{}\"}}", "x".repeat(100 * 1024)));
        return send(stream, "200 OK", "application/json", &padded);
    }
    send(stream, "200 OK", "application/json", body)
}

fn authorize(stream: &mut impl Write, req: &Req, cfg: &Cfg, state: &Mutex<State>) {
    let q = parse_query(&req.query);
    let sane = q.get("response_type").map(String::as_str) == Some("code")
        && q.get("code_challenge_method").map(String::as_str) == Some("S256")
        && q.get("code_challenge").is_some_and(|c| !c.is_empty())
        && q.get("state").is_some_and(|s| !s.is_empty())
        && q.get("client_id").is_some_and(|c| !c.is_empty())
        && q.get("redirect_uri")
            .is_some_and(|r| r.starts_with("http://127.0.0.1:") && r.ends_with("/callback"))
        && (!cfg.require_resource || q.get("resource").is_some_and(|r| !r.is_empty()))
        && cfg
            .expect_client
            .as_ref()
            .is_none_or(|(id, _)| q.get("client_id").map(String::as_str) == Some(id.as_str()))
        && (!cfg.google_shape
            || (q.get("access_type").map(String::as_str) == Some("offline")
                && q.get("prompt")
                    .is_some_and(|p| p.split_whitespace().any(|w| w == "consent"))));
    // A client id this server handed out may only come back on an address it was registered
    // with, compared exactly (an id the person entered is not checked here).
    let registered_ok = {
        let st = state.lock().unwrap();
        match q.get("client_id").and_then(|c| st.registered.get(c)) {
            Some(list) => q.get("redirect_uri").is_some_and(|r| list.contains(r)),
            None => true,
        }
    };
    if !sane || !registered_ok {
        return send(
            stream,
            "400 Bad Request",
            "text/plain",
            "bad sign-in request",
        );
    }
    let code = {
        let mut st = state.lock().unwrap();
        st.last_redirect = q.get("redirect_uri").cloned();
        st.authorize = q.clone();
        st.codes_issued += 1;
        let code = format!("code-{}", st.codes_issued);
        let grant = Grant {
            challenge: q["code_challenge"].clone(),
            redirect: q["redirect_uri"].clone(),
            client: q["client_id"].clone(),
            resource: q.get("resource").filter(|r| !r.is_empty()).cloned(),
            scope: q.get("scope").filter(|s| !s.is_empty()).cloned(),
            refresh_key: format!("fake-refresh-{}", st.codes_issued),
        };
        st.codes.insert(code.clone(), grant);
        code
    };
    let location = format!("{}?code={}&state={}", q["redirect_uri"], code, q["state"]);
    send_headers(
        stream,
        "302 Found",
        &[("Location", location.as_str())],
        "text/plain",
        "",
    );
}

fn token(stream: &mut impl Write, req: &Req, cfg: &Cfg, state: &Mutex<State>) {
    // The credentials, when the client is expected to have them.
    if let Some((id, secret)) = &cfg.expect_client {
        let want = (id.clone(), secret.clone());
        if basic_pair(&req.headers) != Some(want) {
            return send(
                stream,
                "401 Unauthorized",
                "application/json",
                &json!({"error": "invalid_client"}).to_string(),
            );
        }
    }
    let p = parse_query(&req.body);
    let grant_type = p.get("grant_type").map(String::as_str).unwrap_or("");
    let mut st = state.lock().unwrap();
    let mut refreshed = false;
    let grant = match grant_type {
        "authorization_code" => {
            let Some(code) = p.get("code") else {
                return refused(stream, "invalid_grant");
            };
            let Some(grant) = st.codes.remove(code) else {
                return refused(stream, "invalid_grant");
            };
            let proof = p.get("code_verifier").map(String::as_str).unwrap_or("");
            if cfg.break_pkce || pkce::challenge(proof) != grant.challenge {
                return refused(stream, "invalid_grant");
            }
            if p.get("redirect_uri").map(String::as_str) != Some(grant.redirect.as_str()) {
                return refused(stream, "invalid_grant");
            }
            if grant.resource.is_some()
                && p.get("resource").map(String::as_str) != grant.resource.as_deref()
            {
                return refused(stream, "invalid_grant");
            }
            st.refreshes
                .insert(grant.refresh_key.clone(), grant.clone());
            grant
        }
        "refresh_token" => {
            let Some(token) = p.get("refresh_token") else {
                return refused(stream, "invalid_grant");
            };
            match st.refreshes.get(token).cloned() {
                Some(mut grant) => {
                    st.refreshes_served += 1;
                    if cfg.rotate_refresh {
                        // The used token is retired; only the new one works from now on.
                        st.refreshes.remove(token);
                        grant.refresh_key =
                            format!("{}.{}", grant.refresh_key, st.refreshes_served);
                        st.refreshes
                            .insert(grant.refresh_key.clone(), grant.clone());
                    }
                    refreshed = true;
                    grant
                }
                None => return refused(stream, "invalid_grant"),
            }
        }
        _ => return refused(stream, "unsupported_grant_type"),
    };
    if grant.client != p.get("client_id").map(String::as_str).unwrap_or("") {
        return refused(stream, "invalid_grant");
    }
    st.accesses_issued += 1;
    let mut body = json!({
        "access_token": format!("fake-access-{}", st.accesses_issued),
        "token_type": "Bearer",
        "expires_in": 3600,
        "refresh_token": grant.refresh_key,
        "scope": grant.scope
    });
    if refreshed && cfg.no_refresh_echo {
        body.as_object_mut().unwrap().remove("refresh_token");
    }
    // Google sends an id_token only with the sign-in code, never with a refresh; its signature
    // is never verified by anything here, so a fake one is enough.
    if !refreshed && grant_type == "authorization_code" {
        if let Some(email) = &cfg.id_email {
            let aud = p.get("client_id").map(String::as_str).unwrap_or("");
            body["id_token"] = json!(id_token(email, aud));
        }
    }
    send(stream, "200 OK", "application/json", &body.to_string());
}

/// A three-part JWT whose payload carries this email; the signature is decoration.
fn id_token(email: &str, audience: &str) -> String {
    use base64::Engine as _;
    let b64 = base64::engine::general_purpose::URL_SAFE_NO_PAD;
    let header = b64.encode(br#"{"alg":"RS256","kid":"fake"}"#);
    let payload = b64.encode(
        json!({
            "iss": "https://accounts.google.com",
            "email": email,
            "email_verified": true,
            "aud": audience
        })
        .to_string(),
    );
    format!("{header}.{payload}.fakesignature")
}

fn refused(stream: &mut impl Write, reason: &str) {
    send(
        stream,
        "400 Bad Request",
        "application/json",
        &json!({"error": reason}).to_string(),
    );
}

/// The id and secret of a Basic header; each half is form-decoded after the split, as the
/// standard says clients encode them.
fn basic_pair(headers: &HashMap<String, String>) -> Option<(String, String)> {
    let b64 = headers.get("authorization")?.strip_prefix("Basic ")?;
    use base64::Engine as _;
    let bytes = base64::engine::general_purpose::STANDARD.decode(b64).ok()?;
    let text = String::from_utf8(bytes).ok()?;
    let (id, secret) = text.split_once(':')?;
    Some((unescape(id), unescape(secret)))
}

fn parse_query(s: &str) -> HashMap<String, String> {
    let mut out = HashMap::new();
    for pair in s.split('&').filter(|p| !p.is_empty()) {
        let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
        out.insert(unescape(k), unescape(v));
    }
    out
}

fn unescape(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 2 < bytes.len() => {
                let h = |b: u8| match b {
                    b'0'..=b'9' => b - b'0',
                    b'a'..=b'f' => b - b'a' + 10,
                    b'A'..=b'F' => b - b'A' + 10,
                    _ => 0,
                };
                out.push(h(bytes[i + 1]) * 16 + h(bytes[i + 2]));
                i += 3;
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn send(stream: &mut impl Write, status: &str, ctype: &str, body: &str) {
    send_headers(stream, status, &[], ctype, body);
}

fn send_headers(
    stream: &mut impl Write,
    status: &str,
    extra: &[(&str, &str)],
    ctype: &str,
    body: &str,
) {
    let _ = write!(
        stream,
        "HTTP/1.1 {status}\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\nConnection: close\r\n",
        body.len()
    );
    for (k, v) in extra {
        let _ = write!(stream, "{k}: {v}\r\n");
    }
    let _ = write!(stream, "\r\n{body}");
}
