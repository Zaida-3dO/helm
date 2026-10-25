//! Settings → MCP's "Test connection": connect once to a server as it is drafted in the editor,
//! ask for its tools, and hang up. Nothing is saved and no chat is involved.
//!
//! The answer is for the person at the editor, so its text never includes a header value, an
//! environment value or a secret: every secret the draft resolved to is blanked out of any text
//! that came back from the server or the system before it is returned.

use crate::types::*;
use crate::validate::{class_ip, validate_server, HostClass};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Write};
use std::net::ToSocketAddrs;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use ts_rs::TS;

const HTTP_TIMEOUT: Duration = Duration::from_secs(15);
const STDIO_TIMEOUT: Duration = Duration::from_secs(20);
const PROTOCOL: &str = "2025-03-26";
const MAX_BODY: u64 = 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpServerTestArgs {
    /// The server as drafted: a new secret may still be in `value`.
    pub server: McpServer,
}

/// What a connection test found.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct TestOutcome {
    pub ok: bool,
    /// One plain sentence for the editor.
    pub message: String,
    /// The server's own name for itself, when it gave one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub server_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional, type = "number")]
    pub tool_count: Option<u32>,
    /// The server answered but wants a sign-in or a different token.
    pub signin_required: bool,
}

impl TestOutcome {
    fn fail(message: impl Into<String>) -> Self {
        Self {
            ok: false,
            message: message.into(),
            server_name: None,
            tool_count: None,
            signin_required: false,
        }
    }
}

/// Run the test for a draft whose references are already resolved.
pub fn test_resolved(draft: &McpServer, resolved: &ResolvedServer) -> TestOutcome {
    if let Err(e) = validate_server(draft) {
        return TestOutcome::fail(e.0);
    }
    if let Some(why) = &resolved.error {
        return TestOutcome::fail(why.clone());
    }
    let mut out = match resolved.transport {
        TransportKind::Stdio => test_stdio(resolved),
        TransportKind::Http => test_http(resolved),
        TransportKind::Sse => TestOutcome::fail(
            "Test connection works for programs and URL servers; this older streaming kind is not tested.",
        ),
    };
    out.message = scrub(&out.message, &resolved.secrets);
    out
}

/// `text` with every secret blanked out.
pub fn scrub(text: &str, secrets: &[String]) -> String {
    let mut out = text.to_owned();
    for s in secrets.iter().filter(|s| !s.is_empty()) {
        out = out.replace(s.as_str(), "[secret]");
    }
    out
}

fn init_request() -> Value {
    json!({
        "jsonrpc": "2.0", "id": 1, "method": "initialize",
        "params": {
            "protocolVersion": PROTOCOL,
            "capabilities": {},
            "clientInfo": { "name": "helm", "version": env!("CARGO_PKG_VERSION") }
        }
    })
}

fn initialized_note() -> Value {
    json!({ "jsonrpc": "2.0", "method": "notifications/initialized" })
}

fn tools_request() -> Value {
    json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/list", "params": {} })
}

/// What the initialize answer says about the server, as "Name 1.2" text and the bare name.
fn describe_server(init: &Value) -> (Option<String>, String) {
    let info = &init["serverInfo"];
    let name = info["name"]
        .as_str()
        .filter(|n| !n.is_empty())
        .map(str::to_owned);
    let text = match (&name, info["version"].as_str().filter(|v| !v.is_empty())) {
        (Some(n), Some(v)) => format!("{n} {v}"),
        (Some(n), None) => n.clone(),
        _ => "the server".to_owned(),
    };
    (name, text)
}

fn success(init: &Value, tools: Option<u32>) -> TestOutcome {
    let (name, text) = describe_server(init);
    let message = match tools {
        Some(1) => format!("Connected to {text}. It offers 1 tool."),
        Some(n) => format!("Connected to {text}. It offers {n} tools."),
        None => format!("Connected to {text}. It did not list its tools."),
    };
    TestOutcome {
        ok: true,
        message,
        server_name: name,
        tool_count: tools,
        signin_required: false,
    }
}

/// The JSON-RPC answer with `id` from a reply that is one JSON message or a stream of `data:`
/// lines. An error object becomes `Err(its message)`.
pub fn rpc_result(body: &str, id: i64) -> Result<Value, String> {
    let trimmed = body.trim();
    let mut candidates: Vec<Value> = Vec::new();
    if trimmed.starts_with('{') || trimmed.starts_with('[') {
        if let Ok(v) = serde_json::from_str::<Value>(trimmed) {
            candidates.push(v);
        }
    } else {
        for line in trimmed.lines() {
            if let Some(data) = line.strip_prefix("data:") {
                if let Ok(v) = serde_json::from_str::<Value>(data.trim()) {
                    candidates.push(v);
                }
            }
        }
    }
    let flat: Vec<Value> = candidates
        .into_iter()
        .flat_map(|v| match v {
            Value::Array(a) => a,
            other => vec![other],
        })
        .collect();
    let answer = flat
        .into_iter()
        .find(|v| v["id"].as_i64() == Some(id))
        .ok_or_else(|| "The server's reply did not contain an answer.".to_owned())?;
    if let Some(e) = answer.get("error") {
        let m = e["message"]
            .as_str()
            .unwrap_or("the server reported an error");
        return Err(format!("The server refused the request: {m}"));
    }
    Ok(answer["result"].clone())
}

// ---- URL servers ----------------------------------------------------------------------------

struct Reply {
    status: u16,
    body: String,
    session: Option<String>,
}

fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .http_status_as_error(false)
        .max_redirects(0)
        .timeout_global(Some(HTTP_TIMEOUT))
        .user_agent("helm-mcp-test")
        .build()
        .new_agent()
}

fn post(
    agent: &ureq::Agent,
    url: &str,
    headers: &BTreeMap<String, String>,
    session: Option<&str>,
    body: &Value,
) -> Result<Reply, String> {
    let mut req = agent
        .post(url)
        .header("Content-Type", "application/json")
        .header("Accept", "application/json, text/event-stream");
    for (k, v) in headers {
        req = req.header(k.as_str(), v.as_str());
    }
    if let Some(s) = session {
        req = req
            .header("Mcp-Session-Id", s)
            .header("MCP-Protocol-Version", PROTOCOL);
    }
    let mut res = req.send(body.to_string()).map_err(|e| e.to_string())?;
    let status = res.status().as_u16();
    let session = res
        .headers()
        .get("mcp-session-id")
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);
    let body = res
        .body_mut()
        .with_config()
        .limit(MAX_BODY)
        .read_to_string()
        .unwrap_or_default();
    Ok(Reply {
        status,
        body,
        session,
    })
}

/// The plain-http address rule at connect time: the host's addresses must all be this computer,
/// or (with the server's opt-in) the private network.
fn check_plain_http_address(url: &url::Url, allow_private: bool) -> Result<(), String> {
    if url.scheme() != "http" {
        return Ok(());
    }
    let host = url.host_str().unwrap_or_default();
    let port = url.port_or_known_default().unwrap_or(80);
    let addrs: Vec<_> = (host.trim_matches(['[', ']']), port)
        .to_socket_addrs()
        .map_err(|_| "The address could not be found.".to_owned())?
        .collect();
    let loopback_name = host.eq_ignore_ascii_case("localhost");
    let ok = !addrs.is_empty()
        && addrs.iter().all(|a| match class_ip(a.ip()) {
            HostClass::Loopback => true,
            HostClass::Private => allow_private && !loopback_name,
            _ => false,
        });
    if ok {
        Ok(())
    } else {
        Err("Plain http is only allowed to this computer or, with the opt-in, your private network.".into())
    }
}

fn test_http(r: &ResolvedServer) -> TestOutcome {
    let Some(raw) = r.url.as_deref() else {
        return TestOutcome::fail("The server has no URL.");
    };
    let Ok(url) = url::Url::parse(raw) else {
        return TestOutcome::fail("The URL is not valid.");
    };
    if let Err(why) = check_plain_http_address(&url, r.allow_private_http) {
        return TestOutcome::fail(why);
    }
    let agent = agent();
    let first = match post(&agent, raw, &r.headers, None, &init_request()) {
        Ok(x) => x,
        Err(e) => return TestOutcome::fail(format!("Could not connect: {e}")),
    };
    if matches!(first.status, 401 | 403) {
        let mut o = TestOutcome::fail(format!(
            "The server answered {}: it needs a sign-in or a different token. Check the headers.",
            first.status
        ));
        o.signin_required = true;
        return o;
    }
    if !(200..300).contains(&first.status) {
        return TestOutcome::fail(format!(
            "The server answered {} to the first request.",
            first.status
        ));
    }
    let init = match rpc_result(&first.body, 1) {
        Ok(v) => v,
        Err(e) => return TestOutcome::fail(e),
    };
    let session = first.session.as_deref();
    let _ = post(&agent, raw, &r.headers, session, &initialized_note());
    let tools = post(&agent, raw, &r.headers, session, &tools_request())
        .ok()
        .filter(|t| (200..300).contains(&t.status))
        .and_then(|t| rpc_result(&t.body, 2).ok())
        .and_then(|v| v["tools"].as_array().map(|a| a.len() as u32));
    success(&init, tools)
}

// ---- programs on this computer --------------------------------------------------------------

/// The next JSON-RPC answer with `id` on `rx`, before `deadline`.
fn wait_for(rx: &mpsc::Receiver<String>, id: i64, deadline: Instant) -> Result<Value, String> {
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err("The program did not answer in time.".into());
        }
        match rx.recv_timeout(left) {
            Ok(line) => {
                if let Ok(v) = serde_json::from_str::<Value>(line.trim()) {
                    if v["id"].as_i64() == Some(id) {
                        return rpc_result(&v.to_string(), id);
                    }
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                return Err("The program did not answer in time.".into())
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                return Err("The program ended before it answered.".into())
            }
        }
    }
}

fn test_stdio(r: &ResolvedServer) -> TestOutcome {
    let Some(program) = r.command.as_deref() else {
        return TestOutcome::fail("The server has no program.");
    };
    let mut cmd = Command::new(program);
    cmd.args(&r.args)
        .envs(&r.env)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            return TestOutcome::fail(format!(
                "Could not start the program: {e}. Use the program's .exe, not a .cmd or .bat shim."
            ))
        }
    };
    let (tx, rx) = mpsc::channel::<String>();
    if let Some(out) = child.stdout.take() {
        std::thread::spawn(move || {
            for line in BufReader::new(out).lines().map_while(Result::ok) {
                if tx.send(line).is_err() {
                    break;
                }
            }
        });
    }
    let errors = Arc::new(Mutex::new(String::new()));
    if let Some(err) = child.stderr.take() {
        let sink = errors.clone();
        std::thread::spawn(move || {
            for line in BufReader::new(err).lines().map_while(Result::ok) {
                let mut s = sink.lock().unwrap_or_else(|p| p.into_inner());
                if s.len() < 600 {
                    s.push_str(&line);
                    s.push(' ');
                }
            }
        });
    }
    let outcome = (|| {
        let mut stdin = child.stdin.take().ok_or("The program has no input.")?;
        let send = |stdin: &mut std::process::ChildStdin, v: &Value| {
            writeln!(stdin, "{v}")
                .and_then(|_| stdin.flush())
                .map_err(|_| "The program closed its input.".to_owned())
        };
        let deadline = Instant::now() + STDIO_TIMEOUT;
        send(&mut stdin, &init_request())?;
        let init = wait_for(&rx, 1, deadline)?;
        send(&mut stdin, &initialized_note())?;
        send(&mut stdin, &tools_request())?;
        let tools = wait_for(&rx, 2, Instant::now() + Duration::from_secs(10))
            .ok()
            .and_then(|v| v["tools"].as_array().map(|a| a.len() as u32));
        Ok::<_, String>(success(&init, tools))
    })();
    let _ = child.kill();
    let _ = child.wait();
    match outcome {
        Ok(o) => o,
        Err(why) => {
            let said = errors
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .trim()
                .to_owned();
            let said = said.chars().take(300).collect::<String>();
            TestOutcome::fail(if said.is_empty() {
                why
            } else {
                format!("{why} It said: {said}")
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;
    use std::net::TcpListener;

    #[test]
    fn answers_are_found_in_json_or_event_streams_and_errors_read_plainly() {
        let json_body =
            r#"{"jsonrpc":"2.0","id":1,"result":{"serverInfo":{"name":"hub","version":"2"}}}"#;
        assert_eq!(
            rpc_result(json_body, 1).unwrap()["serverInfo"]["name"],
            "hub"
        );
        let sse = "event: message\ndata: {\"jsonrpc\":\"2.0\",\"method\":\"ping\"}\n\ndata: {\"jsonrpc\":\"2.0\",\"id\":2,\"result\":{\"tools\":[1,2]}}\n\n";
        assert_eq!(
            rpc_result(sse, 2).unwrap()["tools"]
                .as_array()
                .unwrap()
                .len(),
            2
        );
        let err = r#"{"jsonrpc":"2.0","id":1,"error":{"code":-1,"message":"nope"}}"#;
        assert!(rpc_result(err, 1).unwrap_err().contains("nope"));
        assert!(rpc_result("<html>", 1).is_err());
        assert_eq!(
            scrub("token abc123 failed", &["abc123".into(), String::new()]),
            "token [secret] failed"
        );
        let (name, text) = describe_server(&json!({"serverInfo": {"name": "hub", "version": "2"}}));
        assert_eq!((name.as_deref(), text.as_str()), (Some("hub"), "hub 2"));
        assert_eq!(
            success(&json!({}), Some(1)).message,
            "Connected to the server. It offers 1 tool."
        );
    }

    /// A one-connection-at-a-time fake MCP server on loopback; `auth` is the header it wants.
    fn serve(auth: Option<&'static str>) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut s) = stream else { break };
                // Read the headers, then as many body bytes as Content-Length promises.
                let mut raw = Vec::new();
                let mut chunk = [0u8; 4096];
                loop {
                    let n = s.read(&mut chunk).unwrap_or(0);
                    if n == 0 {
                        break;
                    }
                    raw.extend_from_slice(&chunk[..n]);
                    let text = String::from_utf8_lossy(&raw).to_lowercase();
                    if let Some(end) = text.find("\r\n\r\n") {
                        let want = text
                            .split("content-length:")
                            .nth(1)
                            .and_then(|r| r.split("\r\n").next())
                            .and_then(|n| n.trim().parse::<usize>().ok())
                            .unwrap_or(0);
                        if raw.len() >= end + 4 + want {
                            break;
                        }
                    }
                }
                let req = String::from_utf8_lossy(&raw).to_string();
                let authorised = auth.is_none_or(|a| {
                    req.to_lowercase()
                        .contains(&format!("authorization: {a}").to_lowercase())
                });
                let (status, body) = if !authorised {
                    ("401 Unauthorized", String::new())
                } else if req.contains("\"initialize\"") {
                    ("200 OK", json!({"jsonrpc":"2.0","id":1,"result":{"serverInfo":{"name":"fake","version":"1.0"}}}).to_string())
                } else if req.contains("tools/list") {
                    ("200 OK", json!({"jsonrpc":"2.0","id":2,"result":{"tools":[{"name":"a"},{"name":"b"},{"name":"c"}]}}).to_string())
                } else {
                    ("202 Accepted", String::new())
                };
                let _ = write!(
                    s,
                    "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = s.flush();
                let _ = s.shutdown(std::net::Shutdown::Write);
                // Let the client finish reading before the socket closes.
                let mut rest = Vec::new();
                let _ = s.set_read_timeout(Some(Duration::from_millis(200)));
                let _ = s.read_to_end(&mut rest);
            }
        });
        format!("http://127.0.0.1:{}/mcp", addr.port())
    }

    fn http_draft(url: &str, token: Option<&str>) -> (McpServer, ResolvedServer) {
        let mut headers = BTreeMap::new();
        let mut refs = BTreeMap::new();
        if let Some(t) = token {
            headers.insert("Authorization".to_owned(), format!("Bearer {t}"));
            refs.insert(
                "Authorization".to_owned(),
                ValueRef {
                    kind: RefKind::Literal,
                    value: Some(t.to_owned()),
                    secret_id: None,
                    prefix: Some("Bearer ".into()),
                },
            );
        }
        let draft = McpServer {
            id: "fake".into(),
            name: "Fake".into(),
            enabled: true,
            transport: Transport {
                kind: TransportKind::Http,
                command: None,
                args: vec![],
                env: BTreeMap::new(),
                url: Some(url.to_owned()),
                headers: refs,
                oauth: None,
            },
            allow_private_http: false,
            preset: None,
        };
        let resolved = ResolvedServer {
            id: "fake".into(),
            name: "Fake".into(),
            transport: TransportKind::Http,
            command: None,
            args: vec![],
            env: BTreeMap::new(),
            url: Some(url.to_owned()),
            headers,
            allow_private_http: false,
            secrets: token.map(|t| vec![t.to_owned()]).unwrap_or_default(),
            guidance: None,
            error: None,
        };
        (draft, resolved)
    }

    #[test]
    fn a_url_server_is_greeted_listed_and_a_refused_token_asks_for_a_sign_in() {
        let url = serve(Some("Bearer good-token"));
        let (d, r) = http_draft(&url, Some("good-token"));
        let ok = test_resolved(&d, &r);
        assert!(ok.ok, "{}", ok.message);
        assert_eq!(ok.tool_count, Some(3));
        assert_eq!(ok.server_name.as_deref(), Some("fake"));

        let (d, r) = http_draft(&url, Some("wrong-token"));
        let refused = test_resolved(&d, &r);
        assert!(
            !refused.ok && refused.signin_required,
            "{}",
            refused.message
        );
        assert!(!refused.message.contains("wrong-token"));

        let mut broken = r.clone();
        broken.error = Some("Header Authorization: the variable X is not set.".into());
        assert!(test_resolved(&d, &broken).message.contains("not set"));

        // Plain http to a public address is refused before any connection.
        let (d, r) = http_draft("http://93.184.216.34/mcp", None);
        assert!(!test_resolved(&d, &r).ok);
    }
    #[test]
    fn a_program_server_is_started_asked_and_stopped() {
        if Command::new("node").arg("--version").output().is_err() {
            return; // no Node here: nothing to run the fake program with
        }
        let script = "const rl=require('readline').createInterface({input:process.stdin});rl.on('line',l=>{const m=JSON.parse(l);const r=(x)=>console.log(JSON.stringify({jsonrpc:'2.0',id:m.id,result:x}));if(m.method==='initialize')r({serverInfo:{name:'prog',version:'3'}});else if(m.method==='tools/list')r({tools:[{name:'x'}]});});";
        let mut r = http_draft("http://127.0.0.1:1/mcp", None).1;
        r.transport = TransportKind::Stdio;
        r.url = None;
        r.command = Some("node".into());
        r.args = vec!["-e".into(), script.into()];
        let mut d = http_draft("http://127.0.0.1:1/mcp", None).0;
        d.transport.kind = TransportKind::Stdio;
        d.transport.url = None;
        d.transport.command = Some("node".into());
        d.transport.args = r.args.clone();
        let out = test_resolved(&d, &r);
        assert!(out.ok, "{}", out.message);
        assert_eq!(
            (out.server_name.as_deref(), out.tool_count),
            (Some("prog"), Some(1))
        );
        r.command = Some("definitely-not-a-program".into());
        d.transport.command = r.command.clone();
        let out = test_resolved(&d, &r);
        assert!(
            !out.ok && out.message.contains("Could not start"),
            "{}",
            out.message
        );
    }
}
