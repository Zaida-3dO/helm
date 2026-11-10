//! Pairing this Helm with the hub by a one-time code.
//!
//! The person running the hub makes a code for one person on one computer; here it is typed in
//! once. The hub answers with a token of its own for this computer (and who it belongs to), so
//! nothing has to be copied between machines by hand. One op does the whole setup:
//!
//! 1. exchange the code for a token (`POST /pair`, plain http, private addresses only, like the
//!    line itself);
//! 2. seal the addresses, the token and the names the hub gave in the credential store, and wake
//!    the line so it connects with them;
//! 3. add (or refresh) the shared-memory server in Settings → MCP with the same token;
//! 4. link the home profile to the hub person (making the home profile from the person's name
//!    when there is none yet).
//!
//! The token is never returned, logged or shown again. A code works once: when the hub has
//! heard it and refused it, the other address is not tried.
//!
//! Op: `hub_pair {address, fallback?, code}` → `{connection, personId, personName, machineId,
//! machineName, memoryServer, profileId}`.

use crate::config::{self, RelayConfig, MIN_TOKEN};
use helm_ipc::{ApiError, AppEvent, OpClass, OpCtx, OpRegistry};
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::time::Duration;

/// The id of the shared-memory server pairing sets up: the hub preset's own id.
pub const MEMORY_SERVER_ID: &str = "helm_hub";
const TIMEOUT: Duration = Duration::from_secs(10);
const MAX_REPLY: u64 = 16 * 1024;

pub fn register_ops(reg: &mut OpRegistry) {
    reg.blocking("hub_pair", OpClass::Maintenance, hub_pair);
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PairArgs {
    address: String,
    #[serde(default)]
    fallback: Option<String>,
    code: String,
}

/// What the hub answers a good code with.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Paired {
    pub token: String,
    pub person_id: String,
    #[serde(default)]
    pub person_name: String,
    #[serde(default)]
    pub machine_id: String,
    #[serde(default)]
    pub machine_name: String,
}

impl std::fmt::Debug for Paired {
    // Never print the token, even in a test failure.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Paired")
            .field("person", &self.person_name)
            .field("machine", &self.machine_name)
            .finish()
    }
}

#[derive(Debug, PartialEq)]
pub enum PairError {
    /// The hub could not be reached: another address may be tried.
    Unreachable,
    /// The hub answered and said no (or something unusable): the code is spent either way.
    Refused(String),
}

/// A code as typed: spaces and dashes dropped, capitals, eight letters and digits.
pub fn normalize_code(raw: &str) -> Result<String, String> {
    let code: String = raw
        .chars()
        .filter(|c| !c.is_whitespace() && *c != '-')
        .collect::<String>()
        .to_ascii_uppercase();
    if code.len() == 8 && code.chars().all(|c| c.is_ascii_alphanumeric()) {
        Ok(code)
    } else {
        Err("A pairing code is 8 letters and numbers.".into())
    }
}

/// The `host:port` of a normalised line address (`ws://host:port/relay`).
fn authority(line_url: &str) -> Option<&str> {
    let rest = line_url.strip_prefix("ws://")?;
    Some(rest.split('/').next().unwrap_or(rest))
}

fn pair_url(line_url: &str) -> Option<String> {
    Some(format!("http://{}/pair", authority(line_url)?))
}

fn memory_url(line_url: &str) -> Option<String> {
    Some(format!("http://{}/mcp", authority(line_url)?))
}

/// Exchange `code` for a token at the hub whose line address is `line_url`.
pub fn exchange(line_url: &str, code: &str) -> Result<Paired, PairError> {
    let url = pair_url(line_url).ok_or(PairError::Unreachable)?;
    let agent = ureq::Agent::config_builder()
        .proxy(None)
        .max_redirects(0)
        .http_status_as_error(false)
        .timeout_global(Some(TIMEOUT))
        .user_agent("helm")
        .build()
        .new_agent();
    let body = json!({ "code": code }).to_string();
    let mut res = agent
        .post(url.as_str())
        .header("content-type", "application/json")
        .send(body.as_str())
        .map_err(|_| PairError::Unreachable)?;
    let status = res.status().as_u16();
    match status {
        200 => {}
        429 => {
            return Err(PairError::Refused(
                "The hub says too many tries: wait a minute and try again.".into(),
            ))
        }
        400..=499 => {
            return Err(PairError::Refused(
                "That code is not valid, or it has expired or been used. Ask for a new one.".into(),
            ))
        }
        _ => {
            return Err(PairError::Refused(format!(
                "The hub could not pair this computer (HTTP {status})."
            )))
        }
    }
    let text = res
        .body_mut()
        .with_config()
        .limit(MAX_REPLY)
        .read_to_string()
        .map_err(|_| PairError::Refused("The hub's answer could not be read.".into()))?;
    let paired: Paired = serde_json::from_str(&text)
        .map_err(|_| PairError::Refused("The hub's answer was not what Helm expected.".into()))?;
    let t = &paired.token;
    if t.len() < MIN_TOKEN || t.chars().any(char::is_whitespace) || paired.person_id.is_empty() {
        return Err(PairError::Refused(
            "The hub's answer was not what Helm expected.".into(),
        ));
    }
    Ok(paired)
}

/// Try each address in turn until the hub answers.
fn exchange_any(urls: &[String], code: &str) -> Result<Paired, PairError> {
    for url in urls.iter().filter(|u| !u.is_empty()) {
        match exchange(url, code) {
            Err(PairError::Unreachable) => continue,
            other => return other,
        }
    }
    Err(PairError::Unreachable)
}

/// The shared-memory server for Settings → MCP: the hub's `/mcp` with the paired token.
pub fn memory_server(line_url: &str, token: &str) -> Option<helm_mcp::McpServer> {
    use helm_mcp::{McpServer, RefKind, Transport, TransportKind, ValueRef};
    let mut headers = BTreeMap::new();
    headers.insert(
        "Authorization".to_owned(),
        ValueRef {
            kind: RefKind::Literal,
            value: Some(token.to_owned()),
            secret_id: None,
            prefix: Some("Bearer ".into()),
        },
    );
    Some(McpServer {
        id: MEMORY_SERVER_ID.into(),
        name: "Helm hub memory".into(),
        enabled: true,
        transport: Transport {
            kind: TransportKind::Http,
            command: None,
            args: vec![],
            env: BTreeMap::new(),
            url: Some(memory_url(line_url)?),
            headers,
            oauth: None,
        },
        // The hub is on the private network, reached over plain http.
        allow_private_http: true,
        preset: Some("helm_hub".into()),
    })
}

fn hub_pair(ctx: &OpCtx, args: Value) -> Result<Value, ApiError> {
    // Errors never echo the input: it holds the code.
    let a: PairArgs = serde_json::from_value(args)
        .map_err(|_| ApiError::bad_args("Expected address, code, and optionally fallback."))?;
    let lan = config::normalize_url(&a.address).map_err(ApiError::bad_args)?;
    let fallback = match a
        .fallback
        .as_deref()
        .map(str::trim)
        .filter(|f| !f.is_empty())
    {
        Some(f) => config::normalize_url(f).map_err(ApiError::bad_args)?,
        None => String::new(),
    };
    let code = normalize_code(&a.code).map_err(ApiError::bad_args)?;
    let paired = exchange_any(&[lan.clone(), fallback.clone()], &code).map_err(|e| match e {
        PairError::Unreachable => ApiError::new(
            "unavailable",
            "The hub could not be reached. Check the address, and that this computer is on the home network or Tailscale.",
        ),
        PairError::Refused(m) => ApiError::new("refused", m),
    })?;

    let creds = crate::ctx_credentials(ctx)?;
    let cfg = RelayConfig {
        lan_url: lan.clone(),
        fallback_url: fallback,
        token: paired.token.clone(),
        person_id: paired.person_id.clone(),
        person_name: paired.person_name.clone(),
        machine_id: paired.machine_id.clone(),
        machine_name: paired.machine_name.clone(),
    };
    config::save_paired(&creds, &cfg).map_err(ApiError::internal)?;
    if let Some(c) = ctx.services.get::<crate::client::Client>() {
        c.reconfigured.notify_waiters();
    }

    // The rest is best effort: the line is paired either way, and each part says how it went.
    let memory_server = match (
        ctx.services.get::<helm_mcp::McpStore>(),
        memory_server(&lan, &paired.token),
    ) {
        (Some(store), Some(server)) => store
            .mutate(None, |list| {
                list.retain(|s| s.id != MEMORY_SERVER_ID);
                list.push(server);
                Ok(())
            })
            .map_err(|e| log::warn!("pairing: the memory server was not added: {e}"))
            .is_ok(),
        _ => false,
    };
    let profile_id = ctx
        .services
        .get::<helm_core::db::Database>()
        .and_then(|db| {
            db.write(|tx| {
                helm_profiles::store::link_home_to_person(
                    tx,
                    &paired.person_id,
                    &paired.person_name,
                    helm_core::now_ms(),
                )
            })
            .map_err(|e| log::warn!("pairing: the profile was not linked: {}", e.message()))
            .ok()
        })
        .map(|p| p.id);
    if profile_id.is_some() {
        ctx.events.emit(AppEvent::new("profiles_changed"));
    }

    Ok(json!({
        "connection": config::view(&cfg, crate::line_state(ctx)),
        "personId": paired.person_id,
        "personName": paired.person_name,
        "machineId": paired.machine_id,
        "machineName": paired.machine_name,
        "memoryServer": memory_server,
        "profileId": profile_id,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use helm_platform::keychain::MemoryKeychain;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::Arc;

    const TOKEN: &str = "paired-secret-token-0123456789abcdef";

    /// A hub that answers one request: returns its line address and the request text it saw.
    fn fake_hub(status: &str, body: String) -> (String, std::thread::JoinHandle<String>) {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = l.local_addr().unwrap();
        let status = status.to_owned();
        let t = std::thread::spawn(move || {
            let (mut s, _) = l.accept().unwrap();
            s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
            let mut seen = Vec::new();
            let mut buf = [0u8; 4096];
            // Read until the JSON body has arrived.
            while !String::from_utf8_lossy(&seen).contains('}') {
                let n = s.read(&mut buf).unwrap();
                if n == 0 {
                    break;
                }
                seen.extend_from_slice(&buf[..n]);
            }
            let reply = format!(
                "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            s.write_all(reply.as_bytes()).unwrap();
            String::from_utf8_lossy(&seen).into_owned()
        });
        (format!("ws://{addr}/relay"), t)
    }

    #[test]
    fn a_code_is_typed_loosely_and_exchanged_once() {
        for (typed, want) in [
            ("abcd-1234", Some("ABCD1234")),
            (" ab cd 12 34 ", Some("ABCD1234")),
            ("ABCD123", None),
            ("ABCD12345", None),
            ("ABCD 12!4", None),
        ] {
            assert_eq!(normalize_code(typed).ok().as_deref(), want, "{typed}");
        }

        // Good: the code goes to /pair, the answer is read, and nothing prints the token.
        let body = json!({"token": TOKEN, "personId": "p1", "personName": "Pat",
                          "machineId": "m1", "machineName": "Laptop"})
        .to_string();
        let (url, hub) = fake_hub("200 OK", body);
        let paired = exchange(&url, "ABCD1234").unwrap();
        let seen = hub.join().unwrap();
        assert!(seen.starts_with("POST /pair "), "{seen}");
        assert!(seen.contains("\"code\":\"ABCD1234\""), "{seen}");
        assert_eq!(
            (paired.person_name.as_str(), paired.machine_name.as_str()),
            ("Pat", "Laptop")
        );
        assert!(!format!("{paired:?}").contains(TOKEN));

        // Refusals say what to do, a spent code is not retried on the other address, and a hub
        // that is not there lets the next address be tried.
        for (status, body, part) in [
            ("404 Not Found", "{}", "not valid"),
            ("410 Gone", "{}", "not valid"),
            ("429 Too Many Requests", "{}", "too many"),
            (
                "200 OK",
                "{\"token\": \"short\", \"personId\": \"p\"}",
                "not what Helm expected",
            ),
            ("200 OK", "nonsense", "not what Helm expected"),
            ("500 Internal Server Error", "{}", "HTTP 500"),
        ] {
            let (url, hub) = fake_hub(status, body.to_owned());
            let dead = {
                let l = TcpListener::bind("127.0.0.1:0").unwrap();
                format!("ws://{}/relay", l.local_addr().unwrap())
            };
            match exchange_any(&[dead.clone(), url.clone()], "ABCD1234") {
                Err(PairError::Refused(m)) => assert!(m.contains(part), "{status}: {m}"),
                other => panic!("{status}: {other:?}"),
            }
            hub.join().unwrap();
        }
        assert_eq!(
            exchange_any(
                &["ws://127.0.0.1:1/relay".into(), String::new()],
                "ABCD1234"
            )
            .err(),
            Some(PairError::Unreachable)
        );
    }

    #[test]
    fn pairing_seals_the_token_and_names_and_sets_up_the_memory_server() {
        let dir = tempfile::tempdir().unwrap();
        let creds = helm_core::credentials::CredentialStore::new(
            &dir.path().join("credentials"),
            "h",
            Arc::new(MemoryKeychain::default()),
        );
        let cfg = RelayConfig {
            lan_url: "ws://100.64.0.9:8000/relay".into(),
            token: TOKEN.into(),
            person_id: "p1".into(),
            person_name: "Pat".into(),
            machine_id: "m1".into(),
            machine_name: "Laptop".into(),
            ..Default::default()
        };
        config::save_paired(&creds, &cfg).unwrap();
        let loaded = config::load(&creds);
        assert_eq!(loaded, cfg);
        // What Settings may see: the names and booleans, never the token or the address.
        let v = serde_json::to_string(&config::view(&loaded, "online")).unwrap();
        assert!(v.contains("\"machineName\":\"Laptop\"") && v.contains("\"hasToken\":true"));
        assert!(!v.contains(TOKEN) && !v.contains("100.64"), "{v}");
        // Typing a different token by hand drops the identity that belonged to the paired one.
        let c = config::save(
            &creds,
            config::RelaySaveArgs {
                token: Some("another-token-0123456789abcdef".into()),
                ..Default::default()
            },
        )
        .unwrap();
        assert!(c.person_name.is_empty() && c.machine_name.is_empty());

        // The memory server points at the hub's /mcp, sends the token as a bearer header, and
        // passes the MCP crate's own checks (plain http to a Tailscale address is opted in).
        let s = memory_server("ws://100.64.0.9:8000/relay", TOKEN).unwrap();
        assert_eq!(
            s.transport.url.as_deref(),
            Some("http://100.64.0.9:8000/mcp")
        );
        assert_eq!(s.preset.as_deref(), Some("helm_hub"));
        assert!(helm_mcp::validate::validate_server(&s).is_ok());
    }
}
