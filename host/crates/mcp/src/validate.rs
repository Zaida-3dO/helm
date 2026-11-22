//! Server rules and the plain-http address rule.
//!
//! Every message here is public: it is shown to the user or returned to an agent as is, so none
//! of them quotes a value.

use crate::types::*;
use std::collections::BTreeSet;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

/// Header names the client sets itself.
pub const RESERVED_HEADERS: &[&str] = &[
    "host",
    "content-length",
    "connection",
    "accept",
    "content-type",
    "mcp-session-id",
    "mcp-protocol-version",
];

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct Invalid(pub String);

fn bad(msg: impl Into<String>) -> Invalid {
    Invalid(msg.into())
}

fn has_control(s: &str) -> bool {
    s.chars().any(char::is_control)
}

pub fn validate_id(id: &str) -> Result<(), Invalid> {
    let ok = !id.is_empty()
        && id.chars().count() <= MAX_ID_CHARS
        && id
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_');
    if ok {
        Ok(())
    } else {
        Err(bad(
            "A server id is 1 to 24 characters of lowercase letters, digits and underscores.",
        ))
    }
}

fn is_identifier(s: &str) -> bool {
    let mut b = s.bytes();
    matches!(b.next(), Some(c) if c.is_ascii_alphabetic() || c == b'_')
        && b.all(|c| c.is_ascii_alphanumeric() || c == b'_')
}

fn is_header_name(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= MAX_HEADER_NAME
        && s.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-')
}

/// Where a reference sits, which changes what a literal may contain.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum RefPlace {
    Env,
    Header,
}

pub fn validate_ref(r: &ValueRef, place: RefPlace, label: &str) -> Result<(), Invalid> {
    validate_profile_secrets(r, place, label)?;
    if let Some(p) = &r.prefix {
        if p.len() > MAX_PREFIX_BYTES || has_control(p) {
            return Err(bad(format!(
                "{label}: the prefix is at most 64 bytes with no control characters."
            )));
        }
    }
    match r.kind {
        RefKind::Literal => match (&r.value, &r.secret_id) {
            (Some(v), None) => {
                if v.len() > MAX_LITERAL_BYTES || v.contains('\0') {
                    return Err(bad(format!(
                        "{label}: a literal value is at most 16 KiB with no NUL."
                    )));
                }
                if place == RefPlace::Header && v.chars().any(|c| c.is_control() && c != '\t') {
                    return Err(bad(format!(
                        "{label}: a header value may not contain control characters."
                    )));
                }
                Ok(())
            }
            (None, Some(id)) if is_secret_id(id) => Ok(()),
            // A blank slot: a server copied from another PC whose secret has not been entered
            // yet. It never connects as it stands; the row says it needs its token.
            (None, None) => Ok(()),
            _ => Err(bad(format!(
                "{label}: a literal carries either a new value or the saved secret's id, not both."
            ))),
        },
        RefKind::ChatPerson => {
            if place != RefPlace::Header {
                return Err(bad(format!(
                    "{label}: the chat's person can only be sent as a header."
                )));
            }
            if r.value.is_some() || r.secret_id.is_some() {
                return Err(bad(format!("{label}: the chat's person takes no value.")));
            }
            Ok(())
        }
        RefKind::Environment | RefKind::File => {
            if r.secret_id.is_some() {
                return Err(bad(format!(
                    "{label}: only a literal has a saved secret id."
                )));
            }
            let v = r.value.as_deref().unwrap_or("");
            if v.is_empty() || v.chars().count() > MAX_REF_VALUE_CHARS || has_control(v) {
                return Err(bad(format!(
                    "{label}: the reference is 1 to 4096 characters with no control characters."
                )));
            }
            if r.kind == RefKind::Environment && !is_identifier(v) {
                return Err(bad(format!(
                    "{label}: an environment reference names a variable (letters, digits, underscores)."
                )));
            }
            if r.kind == RefKind::File && !is_local_absolute(v) {
                return Err(bad(format!(
                    "{label}: a file reference is an absolute path on this computer (not a network or device path)."
                )));
            }
            Ok(())
        }
    }
}

/// A profile's own values for a literal secret: only a literal has them, there are few, each is
/// named by a plain profile id and carries a real value or the saved secret's id (never both,
/// never neither: a profile with nothing to say has no entry).
fn validate_profile_secrets(r: &ValueRef, place: RefPlace, label: &str) -> Result<(), Invalid> {
    if r.profiles.is_empty() {
        return Ok(());
    }
    if r.kind != RefKind::Literal {
        return Err(bad(format!(
            "{label}: only a literal secret can have a value for a profile."
        )));
    }
    if r.profiles.len() > MAX_PROFILE_SECRETS {
        return Err(bad(format!(
            "{label}: a secret has its own value for at most {MAX_PROFILE_SECRETS} profiles."
        )));
    }
    for (profile, p) in &r.profiles {
        let plain = !profile.is_empty()
            && profile.len() <= 64
            && profile
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_');
        if !plain {
            return Err(bad(format!("{label}: a profile id is not valid.")));
        }
        match (&p.value, &p.secret_id) {
            (Some(v), None) if !v.is_empty() => {
                if v.len() > MAX_LITERAL_BYTES || v.contains('\0') {
                    return Err(bad(format!(
                        "{label}: a literal value is at most 16 KiB with no NUL."
                    )));
                }
                if place == RefPlace::Header && v.chars().any(|c| c.is_control() && c != '\t') {
                    return Err(bad(format!(
                        "{label}: a header value may not contain control characters."
                    )));
                }
            }
            (None, Some(id)) if is_secret_id(id) => {}
            _ => {
                return Err(bad(format!(
                    "{label}: a profile's value is either a new value or the saved secret's id."
                )))
            }
        }
    }
    Ok(())
}

/// An absolute path on a local drive. Network shares (`\\host\share`, `//host/share`) and device
/// paths (`\\?\`, `\\.\`) are refused: opening one can make Windows sign in to another host.
pub fn is_local_absolute(p: &str) -> bool {
    let b = p.as_bytes();
    let sep = |c: u8| c == b'\\' || c == b'/';
    let unc = b.len() >= 2 && sep(b[0]) && sep(b[1]);
    !unc && std::path::Path::new(p).is_absolute()
}

/// `lit_` plus 32 lowercase hex: the shape [`crate::config`] mints.
pub fn is_secret_id(id: &str) -> bool {
    id.len() == 36
        && id.starts_with("lit_")
        && id[4..]
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// How a URL's host is classed for plain http.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostClass {
    Loopback,
    /// RFC 1918, Tailscale's 100.64.0.0/10, or an IPv6 unique-local address.
    Private,
    Public,
    /// A DNS name: classed by its resolved address when the worker connects.
    Name,
}

pub fn class_ip(ip: IpAddr) -> HostClass {
    match ip {
        IpAddr::V4(v4) => class_v4(v4),
        IpAddr::V6(v6) => {
            if v6.is_loopback() {
                HostClass::Loopback
            } else if let Some(v4) = v6.to_ipv4_mapped() {
                class_v4(v4)
            } else if (v6.segments()[0] & 0xfe00) == 0xfc00 {
                HostClass::Private
            } else {
                HostClass::Public
            }
        }
    }
}

fn class_v4(v4: Ipv4Addr) -> HostClass {
    let o = v4.octets();
    if o[0] == 127 {
        HostClass::Loopback
    } else if o[0] == 10
        || (o[0] == 172 && (16..=31).contains(&o[1]))
        || (o[0] == 192 && o[1] == 168)
        || (o[0] == 100 && (64..=127).contains(&o[1]))
    {
        HostClass::Private
    } else {
        HostClass::Public
    }
}

pub fn class_host(host: &url::Host<&str>) -> HostClass {
    match host {
        url::Host::Ipv4(a) => class_ip(IpAddr::V4(*a)),
        url::Host::Ipv6(a) => class_ip(IpAddr::V6(*a)),
        url::Host::Domain(d) => {
            if d.eq_ignore_ascii_case("localhost") {
                HostClass::Loopback
            } else {
                HostClass::Name
            }
        }
    }
}

/// The URL rule: https anywhere; plain http for loopback, or (with the server's opt-in and no
/// OAuth) a private address or a name the worker must resolve to one.
pub fn validate_url(
    raw: &str,
    allow_private_http: bool,
    has_oauth: bool,
) -> Result<url::Url, Invalid> {
    if raw.chars().count() > MAX_URL_CHARS {
        return Err(bad("The URL is longer than 4096 characters."));
    }
    let u = url::Url::parse(raw).map_err(|_| bad("The URL is not valid."))?;
    if !u.username().is_empty() || u.password().is_some() {
        return Err(bad("The URL may not carry a user name or password."));
    }
    if u.query().is_some() || u.fragment().is_some() {
        return Err(bad("The URL may not have a query or a fragment."));
    }
    let host = u.host().ok_or_else(|| bad("The URL has no host."))?;
    match u.scheme() {
        "https" => Ok(u),
        "http" => match class_host(&host) {
            HostClass::Loopback => Ok(u),
            HostClass::Public => Err(bad(
                "Plain http is only allowed on this computer or your private network; use https.",
            )),
            HostClass::Private | HostClass::Name if has_oauth => Err(bad(
                "A server that signs in with OAuth must use https unless it is on this computer.",
            )),
            HostClass::Private | HostClass::Name if !allow_private_http => Err(bad(NEEDS_OPT_IN)),
            _ => Ok(u),
        },
        _ => Err(bad(
            "The URL must start with https:// (or http:// on a private network).",
        )),
    }
}

/// What a plain-http server on another machine is told when its opt-in is off.
pub const NEEDS_OPT_IN: &str =
    "Plain http to another machine needs \"Allow unencrypted on my network\" for this server in Settings → MCP.";

fn validate_redirect(raw: &str) -> Result<(), Invalid> {
    let u = url::Url::parse(raw).map_err(|_| bad("The OAuth redirect URI is not valid."))?;
    let loopback = u
        .host()
        .is_some_and(|h| class_host(&h) == HostClass::Loopback);
    let port_ok = matches!(u.port(), Some(0) | Some(1024..));
    // The listener is on 127.0.0.1 and answers at one path, so the address must be exactly that
    // (a name could resolve to another interface in the browser).
    if u.scheme() != "http"
        || u.host_str() != Some("127.0.0.1")
        || !loopback
        || !port_ok
        || !u.username().is_empty()
        || u.password().is_some()
        || u.query().is_some()
        || u.fragment().is_some()
    {
        return Err(bad(
            "The OAuth redirect URI is http://127.0.0.1 with port 0 or 1024 and above, and no credentials, query or fragment.",
        ));
    }
    Ok(())
}

/// One server's rules. Literal `value`s are still allowed here (input form).
pub fn validate_server(s: &McpServer) -> Result<(), Invalid> {
    validate_id(&s.id)?;
    if s.name.trim().is_empty() || s.name.chars().count() > MAX_NAME_CHARS || has_control(&s.name) {
        return Err(bad(
            "A server name is 1 to 120 characters with no control characters.",
        ));
    }
    if let Some(p) = &s.preset {
        if p.len() > 64 || !p.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_') {
            return Err(bad("The preset id is not valid."));
        }
    }
    let t = &s.transport;
    match t.kind {
        TransportKind::Stdio => {
            if t.url.is_some() || !t.headers.is_empty() || t.oauth.is_some() {
                return Err(bad(
                    "A stdio server has a command, not a URL, headers or OAuth.",
                ));
            }
            if s.allow_private_http {
                return Err(bad("\"Allow unencrypted\" applies to http servers only."));
            }
            let cmd = t.command.as_deref().unwrap_or("");
            if cmd.trim().is_empty()
                || cmd.chars().count() > MAX_COMMAND_CHARS
                || cmd.contains('\0')
            {
                return Err(bad(
                    "A stdio server needs a command of at most 4096 characters.",
                ));
            }
            if t.args.len() > MAX_ARGS {
                return Err(bad("A stdio server has at most 64 arguments."));
            }
            if t.args
                .iter()
                .any(|a| a.chars().count() > MAX_ARG_CHARS || a.contains('\0'))
            {
                return Err(bad("Each argument is at most 4096 characters with no NUL."));
            }
            if t.env.len() > MAX_ENV {
                return Err(bad("A stdio server has at most 32 environment entries."));
            }
            for (k, v) in &t.env {
                if !is_identifier(k) {
                    return Err(bad(
                        "Environment names are a letter or underscore, then letters, digits or underscores.",
                    ));
                }
                validate_ref(v, RefPlace::Env, &format!("Environment {k}"))?;
            }
            if s.preset.as_deref() == Some(crate::presets::BROKER_PRESET_ID) {
                validate_broker_env(&t.env)?;
            }
        }
        TransportKind::Http | TransportKind::Sse => {
            if t.command.is_some() || !t.args.is_empty() || !t.env.is_empty() {
                return Err(bad(
                    "A remote server has a URL, not a command, arguments or env.",
                ));
            }
            let url = t
                .url
                .as_deref()
                .ok_or_else(|| bad("A remote server needs a URL."))?;
            validate_url(url, s.allow_private_http, t.oauth.is_some())?;
            if t.headers.len() > MAX_HEADERS {
                return Err(bad("A server has at most 32 headers."));
            }
            let mut seen = BTreeSet::new();
            for (k, v) in &t.headers {
                if !is_header_name(k) {
                    return Err(bad(
                        "Header names are 1 to 128 letters, digits or dashes (no underscores).",
                    ));
                }
                let lower = k.to_ascii_lowercase();
                if RESERVED_HEADERS.contains(&lower.as_str()) {
                    return Err(bad(format!(
                        "The {k} header is set by Helm and cannot be configured."
                    )));
                }
                if !seen.insert(lower.clone()) {
                    return Err(bad(format!(
                        "The {k} header appears twice with different case."
                    )));
                }
                if lower == "authorization" && t.oauth.is_some() {
                    return Err(bad(
                        "OAuth cannot be combined with an Authorization header.",
                    ));
                }
                validate_ref(v, RefPlace::Header, &format!("Header {k}"))?;
            }
            if let Some(o) = &t.oauth {
                if let Some(secret) = &o.client_secret {
                    if secret.kind != RefKind::Literal {
                        return Err(bad(
                            "The OAuth client secret is entered as a literal value.",
                        ));
                    }
                    if !secret.profiles.is_empty() {
                        return Err(bad(
                            "The OAuth client secret is the same for every profile.",
                        ));
                    }
                    validate_ref(secret, RefPlace::Header, "OAuth client secret")?;
                }
                for f in [&o.client_id, &o.scope].into_iter().flatten() {
                    if f.chars().count() > MAX_OAUTH_FIELD || has_control(f) {
                        return Err(bad(
                            "OAuth client id and scope are at most 4096 characters with no control characters.",
                        ));
                    }
                }
                if let Some(r) = &o.redirect_uri {
                    validate_redirect(r)?;
                }
            }
        }
    }
    Ok(())
}

/// A browser name as the broker reads it: a lowercase letter, then lowercase letters, digits
/// and dashes, at most 32 — it also names the browser's program variable.
fn browser_name_ok(s: &str) -> bool {
    let b = s.as_bytes();
    !b.is_empty()
        && b.len() <= 32
        && b[0].is_ascii_lowercase()
        && b.iter().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == b'-')
}

/// A Chrome extension id: exactly 32 letters from `a` to `p`, the alphabet Chrome assigns.
fn extension_id_ok(s: &str) -> bool {
    s.len() == 32 && s.bytes().all(|c| (b'a'..=b'p').contains(&c))
}

/// One acting-site entry: a host, or a `*.` wildcard in front of one. Hosts are case-insensitive;
/// a scheme, path or port is not part of a host and is refused.
fn site_entry_ok(s: &str) -> bool {
    let lower = s.to_ascii_lowercase();
    let host = lower.strip_prefix("*.").unwrap_or(lower.as_str());
    !host.is_empty()
        && host.len() <= 253
        && host.split('.').all(|label| {
            let b = label.as_bytes();
            !b.is_empty()
                && b.len() <= 63
                && b[0].is_ascii_alphanumeric()
                && b[b.len() - 1].is_ascii_alphanumeric()
                && b.iter().all(|c| c.is_ascii_alphanumeric() || *c == b'-')
        })
}

/// The rows the browser preset's "Your browser" form writes. Only literal values actually typed
/// in are judged (a reference to an environment variable is the user's own wiring); an empty
/// value is a blank row, checked nowhere else either. The connect token is never looked at, and
/// no refusal quotes a value — only the variable's name.
fn validate_broker_env(env: &BTreeMap<String, ValueRef>) -> Result<(), Invalid> {
    for (k, v) in env {
        if v.kind != RefKind::Literal {
            continue;
        }
        let value = v.value.as_deref().unwrap_or("");
        if value.is_empty() {
            continue;
        }
        match k.as_str() {
            "BROKER_ATTACHED_BROWSERS" => {
                if !browser_name_ok(value) {
                    return Err(bad(
                        "BROKER_ATTACHED_BROWSERS: a browser name is 1 to 32 lowercase letters, digits and dashes, starting with a letter; the broker builds its browser-program variable from it.",
                    ));
                }
            }
            "BROKER_ATTACHED_EXTENSION_ID" => {
                if !extension_id_ok(value) {
                    return Err(bad(
                        "BROKER_ATTACHED_EXTENSION_ID: a browser extension id is exactly 32 letters from a to p, the id your browser shows for the broker's extension.",
                    ));
                }
            }
            "BROKER_ATTACHED_ACT_SITES" => {
                for entry in value.split(',').map(|e| e.trim()) {
                    if entry.is_empty() {
                        continue;
                    }
                    if !site_entry_ok(entry) {
                        return Err(bad(
                            "BROKER_ATTACHED_ACT_SITES: each comma-separated entry is a host like shop.example or a wildcard like *.shop.example — no scheme, path or port.",
                        ));
                    }
                }
            }
            _ => {}
        }
    }
    Ok(())
}

/// The whole list: each server, unique ids, ≤ 16 servers, ≤ 128 KiB serialised.
pub fn validate_list(servers: &[McpServer]) -> Result<(), Invalid> {
    if servers.len() > MAX_SERVERS {
        return Err(bad("Helm keeps at most 16 MCP servers."));
    }
    let mut ids = BTreeSet::new();
    for s in servers {
        validate_server(s)?;
        if !ids.insert(s.id.as_str()) {
            return Err(bad(format!("Two servers share the id {}.", s.id)));
        }
    }
    let size = serde_json::to_vec(servers)
        .map(|v| v.len())
        .unwrap_or(usize::MAX);
    if size > MAX_CONFIG_BYTES {
        return Err(bad("The MCP configuration is larger than 128 KiB."));
    }
    Ok(())
}

/// Used by the connect-time check's tests and by the worker contract: the IPv6 any/unspecified
/// address is never private.
pub fn is_private_or_loopback(ip: IpAddr) -> bool {
    !matches!(ip, IpAddr::V6(a) if a == Ipv6Addr::UNSPECIFIED)
        && matches!(class_ip(ip), HostClass::Loopback | HostClass::Private)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn http(url: &str, opt_in: bool) -> McpServer {
        McpServer {
            id: "hub".into(),
            name: "Hub".into(),
            enabled: true,
            transport: Transport {
                kind: TransportKind::Http,
                command: None,
                args: vec![],
                env: BTreeMap::new(),
                url: Some(url.into()),
                headers: BTreeMap::new(),
                oauth: None,
            },
            allow_private_http: opt_in,
            preset: None,
        }
    }

    fn stdio(cmd: &str) -> McpServer {
        McpServer {
            id: "fake".into(),
            name: "Fake".into(),
            enabled: true,
            transport: Transport {
                kind: TransportKind::Stdio,
                command: Some(cmd.into()),
                args: vec![],
                env: BTreeMap::new(),
                url: None,
                headers: BTreeMap::new(),
                oauth: None,
            },
            allow_private_http: false,
            preset: None,
        }
    }

    fn lit(v: &str) -> ValueRef {
        ValueRef {
            profiles: Default::default(),
            kind: RefKind::Literal,
            value: Some(v.into()),
            secret_id: None,
            prefix: None,
        }
    }

    #[test]
    fn ids() {
        assert!(validate_id("agent_standup").is_ok());
        assert!(validate_id("a").is_ok());
        assert!(validate_id("").is_err());
        assert!(validate_id("Upper").is_err());
        assert!(validate_id("has-dash").is_err());
        assert!(validate_id(&"a".repeat(24)).is_ok());
        assert!(validate_id(&"a".repeat(25)).is_err());
    }

    #[test]
    fn https_anywhere_http_loopback_only_by_default() {
        assert!(validate_server(&http("https://example.com/mcp", false)).is_ok());
        assert!(validate_server(&http("http://127.0.0.1:9/mcp", false)).is_ok());
        assert!(validate_server(&http("http://localhost:9/mcp", false)).is_ok());
        assert!(validate_server(&http("http://[::1]:9/mcp", false)).is_ok());
        assert!(validate_server(&http("http://10.20.30.5:9/mcp", false)).is_err());
        assert!(validate_server(&http("http://hub.lan:9/mcp", false)).is_err());
        assert!(validate_server(&http("ftp://example.com/", false)).is_err());
    }

    #[test]
    fn private_http_needs_the_opt_in_and_never_reaches_a_public_ip() {
        for ok in [
            "http://10.0.0.2/mcp",
            "http://172.16.0.1/mcp",
            "http://172.31.255.1/mcp",
            "http://192.168.0.9/mcp",
            "http://100.64.0.1/mcp",
            "http://100.127.255.255/mcp",
            "http://[fd7a:115c:a1e0::1]/mcp",
            "http://somehost/mcp",
        ] {
            assert!(validate_server(&http(ok, true)).is_ok(), "{ok}");
        }
        for no in [
            "http://8.8.8.8/mcp",
            "http://172.32.0.1/mcp",
            "http://100.128.0.1/mcp",
            "http://11.0.0.1/mcp",
            "http://[2001:db8::1]/mcp",
            "http://[::ffff:8.8.8.8]/mcp",
        ] {
            assert!(validate_server(&http(no, true)).is_err(), "{no}");
        }
    }

    #[test]
    fn oauth_keeps_https_off_loopback() {
        let mut s = http("http://10.20.30.5/mcp", true);
        s.transport.oauth = Some(OAuthConfig::default());
        assert!(validate_server(&s).is_err());
        s.transport.url = Some("http://127.0.0.1:5/mcp".into());
        assert!(validate_server(&s).is_ok());
    }

    #[test]
    fn url_shape() {
        assert!(validate_server(&http("https://u:p@example.com/", false)).is_err());
        assert!(validate_server(&http("https://example.com/?a=1", false)).is_err());
        assert!(validate_server(&http("https://example.com/#f", false)).is_err());
    }

    #[test]
    fn headers() {
        let mut s = http("https://example.com/mcp", false);
        s.transport
            .headers
            .insert("Authorization".into(), lit("Bearer x"));
        assert!(validate_server(&s).is_ok());
        s.transport.headers.insert("authorization".into(), lit("y"));
        assert!(validate_server(&s).is_err(), "case duplicate");
        let mut s = http("https://example.com/mcp", false);
        s.transport.headers.insert("X_Under".into(), lit("v"));
        assert!(validate_server(&s).is_err(), "underscore");
        let mut s = http("https://example.com/mcp", false);
        s.transport
            .headers
            .insert("Mcp-Session-Id".into(), lit("v"));
        assert!(validate_server(&s).is_err(), "reserved");
        let mut s = http("https://example.com/mcp", false);
        s.transport.headers.insert("X-A".into(), lit("a\nb"));
        assert!(validate_server(&s).is_err(), "control in header literal");
        let mut s = http("https://example.com/mcp", false);
        s.transport.headers.insert("Authorization".into(), lit("v"));
        s.transport.oauth = Some(OAuthConfig::default());
        assert!(validate_server(&s).is_err(), "oauth + authorization");
    }

    #[test]
    fn stdio_rules() {
        assert!(validate_server(&stdio("node.exe")).is_ok());
        assert!(validate_server(&stdio("  ")).is_err());
        let mut s = stdio("node.exe");
        s.transport.args = vec!["a".into(); 64];
        assert!(validate_server(&s).is_ok());
        s.transport.args.push("b".into());
        assert!(validate_server(&s).is_err());
        let mut s = stdio("node.exe");
        s.transport.env.insert("1BAD".into(), lit("v"));
        assert!(validate_server(&s).is_err());
        let mut s = stdio("node.exe");
        s.allow_private_http = true;
        assert!(validate_server(&s).is_err());
        let mut s = stdio("node.exe");
        s.transport.url = Some("https://x/".into());
        assert!(validate_server(&s).is_err());
    }

    #[test]
    fn broker_rows_are_checked_by_shape_and_a_bad_value_is_never_echoed() {
        let broker = |rows: Vec<(&str, ValueRef)>| {
            let mut s = stdio("node.exe");
            s.preset = Some("browser_broker".into());
            for (k, v) in rows {
                s.transport.env.insert(k.into(), v);
            }
            s
        };
        let envr = |v: &str| ValueRef {
            profiles: Default::default(),
            kind: RefKind::Environment,
            value: Some(v.into()),
            secret_id: None,
            prefix: None,
        };
        let good = broker(vec![
            ("BROKER_ATTACHED_BROWSERS", lit("brave")),
            ("BROKER_BROWSER_BRAVE_PATH", lit(r"C:\Program Files\BraveSoftware\Brave-Browser\Application\brave.exe")),
            ("BROKER_ATTACHED_EXTENSION_ID", lit(&"a".repeat(32))),
            ("BROKER_ATTACHED_ACT_SITES", lit("shop.example, *.read.example.org")),
            ("BROKER_ATTACHED_TOKEN", lit("any token shape at all, never checked")),
            ("BROKER_OWNER_GRANT_SECRET", envr("GRANT_VAR")),
        ]);
        assert!(validate_server(&good).is_ok());
        // Each rule names its variable in plain words and quotes no value.
        for (key, bad_value) in [
            ("BROKER_ATTACHED_BROWSERS", "Brave".to_string()),
            ("BROKER_ATTACHED_BROWSERS", "1brave".into()),
            ("BROKER_ATTACHED_BROWSERS", "a brave".into()),
            ("BROKER_ATTACHED_BROWSERS", "b".repeat(33)),
            ("BROKER_ATTACHED_EXTENSION_ID", "a".repeat(31)),
            ("BROKER_ATTACHED_EXTENSION_ID", "a".repeat(33)),
            ("BROKER_ATTACHED_EXTENSION_ID", "q".repeat(32)),
            ("BROKER_ATTACHED_ACT_SITES", "https://shop.example/".into()),
            ("BROKER_ATTACHED_ACT_SITES", "shop.example:8443".into()),
            ("BROKER_ATTACHED_ACT_SITES", "shop.example/path".into()),
            ("BROKER_ATTACHED_ACT_SITES", "*shop.example".into()),
        ] {
            let e = validate_server(&broker(vec![(key, lit(&bad_value))])).unwrap_err().0;
            assert!(e.contains(key), "{key}={bad_value}: {e}");
            assert!(!e.contains(&bad_value), "{key}={bad_value}: the refusal quotes the value");
        }
        // An empty row waits like anywhere else; an environment reference is the user's wiring,
        // shape-checked nowhere; and a non-broker server keeps its env rows as free text.
        assert!(validate_server(&broker(vec![("BROKER_ATTACHED_BROWSERS", lit(""))])).is_ok());
        assert!(validate_server(&broker(vec![("BROKER_ATTACHED_BROWSERS", envr("Bad Var!"))])).is_ok());
        let mut other = stdio("node.exe");
        other.transport.env.insert("BROKER_ATTACHED_BROWSERS".into(), lit("Not A Name"));
        assert!(validate_server(&other).is_ok());
    }

    #[test]
    fn references() {
        let env = |v: &str| ValueRef {
            profiles: Default::default(),
            kind: RefKind::Environment,
            value: Some(v.into()),
            secret_id: None,
            prefix: None,
        };
        assert!(validate_ref(&env("HUB_TOKEN"), RefPlace::Env, "x").is_ok());
        assert!(validate_ref(&env("HUB-TOKEN"), RefPlace::Env, "x").is_err());
        let file = ValueRef {
            profiles: Default::default(),
            kind: RefKind::File,
            value: Some("relative.txt".into()),
            secret_id: None,
            prefix: None,
        };
        assert!(validate_ref(&file, RefPlace::Env, "x").is_err());
        let mut l = lit("v");
        l.prefix = Some("p".repeat(65));
        assert!(validate_ref(&l, RefPlace::Env, "x").is_err());
        assert!(validate_ref(&lit(&"v".repeat(16 * 1024)), RefPlace::Env, "x").is_ok());
        assert!(validate_ref(&lit(&"v".repeat(16 * 1024 + 1)), RefPlace::Env, "x").is_err());
        let both = ValueRef {
            profiles: Default::default(),
            kind: RefKind::Literal,
            value: Some("v".into()),
            secret_id: Some(format!("lit_{}", "a".repeat(32))),
            prefix: None,
        };
        assert!(validate_ref(&both, RefPlace::Env, "x").is_err());
        let kept = ValueRef {
            profiles: Default::default(),
            kind: RefKind::Literal,
            value: None,
            secret_id: Some(format!("lit_{}", "a".repeat(32))),
            prefix: None,
        };
        assert!(validate_ref(&kept, RefPlace::Env, "x").is_ok());
        // A blank slot (a copy whose secret was blanked on the way across) saves and waits.
        let blank = ValueRef {
            profiles: Default::default(),
            kind: RefKind::Literal,
            value: None,
            secret_id: None,
            prefix: None,
        };
        assert!(
            validate_ref(&blank, RefPlace::Env, "x").is_ok(),
            "an empty secret slot saves, so a copied server can land waiting for its token"
        );
        // The chat's person: a header with no value.
        let person = |value: Option<&str>| ValueRef {
            profiles: Default::default(),
            kind: RefKind::ChatPerson,
            value: value.map(Into::into),
            secret_id: None,
            prefix: None,
        };
        assert!(validate_ref(&person(None), RefPlace::Header, "x").is_ok());
        assert!(validate_ref(&person(None), RefPlace::Env, "x").is_err());
        assert!(validate_ref(&person(Some("7")), RefPlace::Header, "x").is_err());
    }

    #[test]
    fn file_references_stay_on_local_drives() {
        let file = |v: &str| ValueRef {
            profiles: Default::default(),
            kind: RefKind::File,
            value: Some(v.into()),
            secret_id: None,
            prefix: None,
        };
        for p in [
            r"\\host\share\tok.txt",
            "//host/share/tok.txt",
            r"\\?\C:\tok.txt",
            r"\\.\pipe\x",
            r"\/host/share",
        ] {
            assert!(validate_ref(&file(p), RefPlace::Env, "x").is_err(), "{p}");
        }
        let local = std::env::temp_dir().join("tok.txt");
        assert!(validate_ref(&file(&local.to_string_lossy()), RefPlace::Env, "x").is_ok());
    }

    #[test]
    fn list_limits() {
        let mut list: Vec<McpServer> = (0..16)
            .map(|i| {
                let mut s = stdio("node.exe");
                s.id = format!("s{i}");
                s
            })
            .collect();
        assert!(validate_list(&list).is_ok());
        let mut extra = stdio("node.exe");
        extra.id = "s99".into();
        list.push(extra);
        assert!(validate_list(&list).is_err());
        let dup = vec![stdio("a"), stdio("b")];
        assert!(validate_list(&dup).is_err());
    }

    #[test]
    fn redirect_rule() {
        let mut s = http("https://example.com/mcp", false);
        s.transport.oauth = Some(OAuthConfig {
            redirect_uri: Some("http://127.0.0.1:0/cb".into()),
            ..Default::default()
        });
        assert!(validate_server(&s).is_ok());
        for bad_uri in [
            "http://127.0.0.1:80/cb",
            "https://127.0.0.1:5000/cb",
            "http://example.com:5000/cb",
            "http://127.0.0.1:5000/cb?x=1",
            "http://localhost:5000/cb",
        ] {
            s.transport.oauth.as_mut().unwrap().redirect_uri = Some(bad_uri.into());
            assert!(validate_server(&s).is_err(), "{bad_uri}");
        }
    }
}
