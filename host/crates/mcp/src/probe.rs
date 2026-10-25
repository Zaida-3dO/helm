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
use std::ffi::OsStr;
use std::io::{BufRead, BufReader, Write};
use std::net::ToSocketAddrs;
use std::path::{Path, PathBuf};
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
    target: &Target,
    headers: &BTreeMap<String, String>,
    session: Option<&str>,
    body: &Value,
) -> Result<Reply, String> {
    let mut req = agent
        .post(target.url.as_str())
        .header("Content-Type", "application/json")
        .header("Accept", "application/json, text/event-stream");
    if let Some(h) = &target.host {
        req = req.header("Host", h.as_str());
    }
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

/// Where a request goes: the URL to connect to and, when the URL was pinned to one address, the
/// original `Host` header.
struct Target {
    url: String,
    host: Option<String>,
}

/// The plain-http address rule: every address the host name resolves to must be this computer or
/// (with the server's opt-in) the private network. The request then goes to the one address that
/// was checked, with the original `Host` header, so the address that was checked is the address
/// connected to (no second lookup that could answer differently).
fn pin_target(url: &url::Url, allow_private: bool) -> Result<Target, String> {
    let plain = Target {
        url: url.to_string(),
        host: None,
    };
    if url.scheme() != "http" {
        return Ok(plain);
    }
    let refused = || {
        "Plain http is only allowed to this computer or, with the opt-in, your private network."
            .to_owned()
    };
    let loopback_name = url
        .host_str()
        .is_some_and(|h| h.eq_ignore_ascii_case("localhost"));
    let allowed = |ip: std::net::IpAddr| match class_ip(ip) {
        HostClass::Loopback => true,
        HostClass::Private => allow_private && !loopback_name,
        _ => false,
    };
    match url.host() {
        Some(url::Host::Ipv4(a)) if allowed(a.into()) => Ok(plain),
        Some(url::Host::Ipv6(a)) if allowed(a.into()) => Ok(plain),
        Some(url::Host::Domain(name)) => {
            let port = url.port_or_known_default().unwrap_or(80);
            let addrs: Vec<_> = (name, port)
                .to_socket_addrs()
                .map_err(|_| "The address could not be found.".to_owned())?
                .collect();
            if addrs.is_empty() || !addrs.iter().all(|a| allowed(a.ip())) {
                return Err(refused());
            }
            let mut pinned = url.clone();
            pinned
                .set_ip_host(addrs[0].ip())
                .map_err(|_| "The address could not be used.".to_owned())?;
            let host = match url.port() {
                Some(p) => format!("{name}:{p}"),
                None => name.to_owned(),
            };
            Ok(Target {
                url: pinned.to_string(),
                host: Some(host),
            })
        }
        _ => Err(refused()),
    }
}

fn test_http(r: &ResolvedServer) -> TestOutcome {
    let Some(raw) = r.url.as_deref() else {
        return TestOutcome::fail("The server has no URL.");
    };
    let Ok(url) = url::Url::parse(raw) else {
        return TestOutcome::fail("The URL is not valid.");
    };
    let target = match pin_target(&url, r.allow_private_http) {
        Ok(t) => t,
        Err(why) => return TestOutcome::fail(why),
    };
    let agent = agent();
    let first = match post(&agent, &target, &r.headers, None, &init_request()) {
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
    let _ = post(&agent, &target, &r.headers, session, &initialized_note());
    let tools = post(&agent, &target, &r.headers, session, &tools_request())
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

/// Never handed to a program (as for the claude child, and as the chats' own client does).
const STRIPPED_ENV: [&str; 5] = [
    "ANTHROPIC_API_KEY",
    "ANTHROPIC_AUTH_TOKEN",
    "ANTHROPIC_BASE_URL",
    "CLAUDE_CODE_OAUTH_TOKEN",
    "CLAUDECODE",
];

fn is_shim(p: &Path) -> bool {
    matches!(
        p.extension()
            .and_then(|e| e.to_str())
            .map(str::to_ascii_lowercase)
            .as_deref(),
        Some("cmd" | "bat")
    )
}

/// Finds a command on `path_var` the way CreateProcess would, preferring .exe, then .cmd, .bat.
fn on_path(cmd: &str, path_var: &OsStr) -> Option<PathBuf> {
    let c = Path::new(cmd);
    if c.is_absolute() || cmd.contains(['/', '\\']) {
        return c.exists().then(|| c.to_path_buf());
    }
    let exts: &[&str] = if c.extension().is_some() {
        &[""]
    } else {
        &[".exe", ".cmd", ".bat", ""]
    };
    for dir in std::env::split_paths(path_var) {
        for e in exts {
            let p = dir.join(format!("{cmd}{e}"));
            if p.is_file() {
                return Some(p);
            }
        }
    }
    None
}

/// The script an npm-generated `.cmd` shim runs: npm's own `npx`/`npm`, or a package's bin made
/// by cmd-shim. `None` when the file is not such a shim.
pub fn shim_target(shim: &Path) -> Option<PathBuf> {
    let meta = std::fs::metadata(shim).ok()?;
    if !meta.is_file() || meta.len() > 16 * 1024 {
        return None;
    }
    let text = std::fs::read_to_string(shim).ok()?;
    let dir = shim.parent()?;
    let lower = text.to_lowercase();
    for name in ["npx", "npm"] {
        if lower.contains(&format!("node_modules\\npm\\bin\\{name}-cli.js")) {
            let p = dir
                .join("node_modules")
                .join("npm")
                .join("bin")
                .join(format!("{name}-cli.js"));
            return p.exists().then_some(p);
        }
    }
    // cmd-shim's package bins: "%_prog%"  "%dp0%\node_modules\pkg\bin\x.js" %*
    let mut rest = text.as_str();
    while let Some(at) = rest.find("\"%") {
        rest = &rest[at + 2..];
        let r = rest.strip_prefix('~').unwrap_or(rest);
        let Some(r) = r.strip_prefix("dp0") else {
            continue;
        };
        let r = r.strip_prefix('%').unwrap_or(r);
        let Some(r) = r.strip_prefix('\\') else {
            continue;
        };
        let Some(end) = r.find('"') else { continue };
        let rel = &r[..end];
        let low = rel.to_lowercase();
        if rel.contains('%')
            || !(low.ends_with(".js") || low.ends_with(".cjs") || low.ends_with(".mjs"))
        {
            continue;
        }
        let rel_path = Path::new(rel);
        if rel_path
            .components()
            .any(|c| !matches!(c, std::path::Component::Normal(_)))
        {
            return None; // would leave the shim's own folder
        }
        let target = dir.join(rel_path);
        return target.exists().then_some(target);
    }
    None
}

/// What will actually be started for a program, or the refusal to show. On Windows only real
/// executables run: a `.cmd` or `.bat` is refused, except npm's own shims and package-bin shims,
/// which become `node.exe <the shim's script>`.
pub fn plan_spawn(
    program: &str,
    args: &[String],
    path_var: &OsStr,
    node: Option<&Path>,
    windows: bool,
) -> Result<(PathBuf, Vec<String>), String> {
    if !windows {
        return Ok((PathBuf::from(program), args.to_vec()));
    }
    let refused_named = |name: &str| {
        format!("{name} is a .cmd or .bat script, which Helm does not run. Use the program's .exe, or node.exe with the script's path.")
    };
    let Some(found) = on_path(program, path_var).filter(|p| is_shim(p)) else {
        // A .cmd or .bat name we could not resolve to a script is refused outright: left to the
        // operating system's own search, it could still end up running through cmd.exe.
        if is_shim(Path::new(program)) {
            return Err(refused_named(program));
        }
        return Ok((PathBuf::from(program), args.to_vec()));
    };
    let name = found
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("that file");
    let refused = || refused_named(name);
    let target = shim_target(&found).ok_or_else(refused)?;
    let node =
        node.ok_or_else(|| "Node.js was not found, which this program needs to start.".to_owned())?;
    let mut all = vec![target.to_string_lossy().into_owned()];
    all.extend_from_slice(args);
    Ok((node.to_path_buf(), all))
}

fn test_stdio(r: &ResolvedServer) -> TestOutcome {
    let Some(program) = r.command.as_deref() else {
        return TestOutcome::fail("The server has no program.");
    };
    let node = helm_workers::node::resolve_node(&helm_workers::node::NodeLookup::from_env()).ok();
    let path_var = std::env::var_os("PATH").unwrap_or_default();
    let (exe, args) = match plan_spawn(
        program,
        &r.args,
        &path_var,
        node.as_ref().map(|n| n.path.as_path()),
        cfg!(windows),
    ) {
        Ok(p) => p,
        Err(why) => return TestOutcome::fail(why),
    };
    let mut cmd = Command::new(exe);
    cmd.args(args)
        .envs(&r.env)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for k in STRIPPED_ENV {
        cmd.env_remove(k);
    }
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

    #[test]
    fn shims_are_refused_except_npms_own_which_run_through_node() {
        let d = tempfile::tempdir().unwrap();
        let dir = d.path();
        let w = |name: &str, text: &str| std::fs::write(dir.join(name), text).unwrap();
        std::fs::create_dir_all(dir.join("node_modules/npm/bin")).unwrap();
        std::fs::create_dir_all(dir.join("node_modules/pkg/bin")).unwrap();
        w("node_modules/npm/bin/npx-cli.js", "");
        w("node_modules/pkg/bin/x.js", "");
        w(
            "npx.cmd",
            "\"%_prog%\" \"%dp0%\\node_modules\\npm\\bin\\npx-cli.js\" %*",
        );
        w(
            "pkg.cmd",
            "\"%_prog%\"  \"%dp0%\\node_modules\\pkg\\bin\\x.js\" %*",
        );
        w("escape.cmd", "\"%_prog%\"  \"%dp0%\\..\\evil.js\" %*");
        w("plain.bat", "echo hello");
        w("real.exe", "");
        let path = std::env::join_paths([dir]).unwrap();
        let node = Path::new("C:/node/node.exe");
        let plan =
            |cmd: &str, windows: bool| plan_spawn(cmd, &["-y".into()], &path, Some(node), windows);

        let (exe, args) = plan("npx", true).unwrap();
        assert_eq!(exe, node);
        assert!(
            args[0].ends_with("npx-cli.js") && args[1] == "-y",
            "{args:?}"
        );
        let (_, args) = plan("pkg", true).unwrap();
        assert!(args[0].ends_with("x.js"), "{args:?}");
        for refused in ["plain", "escape", "plain.bat", "missing.cmd", ".\\gone.BAT"] {
            let why = plan(refused, true).unwrap_err();
            assert!(why.contains(".cmd or .bat"), "{refused}: {why}");
        }
        assert_eq!(plan("real", true).unwrap().0, PathBuf::from("real"));
        assert_eq!(
            plan("npx", false).unwrap().0,
            PathBuf::from("npx"),
            "other systems run it as given"
        );
        assert!(plan_spawn("npx", &[], &path, None, true)
            .unwrap_err()
            .contains("Node.js"));
    }

    #[test]
    fn a_name_is_pinned_to_the_address_that_was_checked() {
        let url = |s: &str| url::Url::parse(s).unwrap();
        let t = pin_target(&url("http://localhost:5000/mcp"), false).unwrap();
        assert_eq!(t.host.as_deref(), Some("localhost:5000"));
        assert!(
            t.url.contains("127.0.0.1") || t.url.contains("[::1]"),
            "{}",
            t.url
        );
        assert!(pin_target(&url("http://127.0.0.1:5000/mcp"), false)
            .unwrap()
            .host
            .is_none());
        assert!(pin_target(&url("https://example.test/mcp"), false)
            .unwrap()
            .host
            .is_none());
        assert!(pin_target(&url("http://10.1.2.3/mcp"), false).is_err());
        assert!(pin_target(&url("http://10.1.2.3/mcp"), true).is_ok());
        assert!(pin_target(&url("http://93.184.216.34/mcp"), true).is_err());
    }

    #[test]
    fn a_program_does_not_get_the_agent_keys() {
        if Command::new("node").arg("--version").output().is_err() {
            return;
        }
        let script = "const rl=require('readline').createInterface({input:process.stdin});rl.on('line',l=>{const m=JSON.parse(l);if(m.method==='initialize')console.log(JSON.stringify({jsonrpc:'2.0',id:m.id,result:{serverInfo:{name:String(!!process.env.ANTHROPIC_API_KEY)+'-'+process.env.KEEP_ME}}}));else if(m.method==='tools/list')console.log(JSON.stringify({jsonrpc:'2.0',id:m.id,result:{tools:[]}}));});";
        let (mut d, mut r) = http_draft("http://127.0.0.1:1/mcp", None);
        d.transport.kind = TransportKind::Stdio;
        d.transport.url = None;
        d.transport.command = Some("node".into());
        r.transport = TransportKind::Stdio;
        r.url = None;
        r.command = Some("node".into());
        r.args = vec!["-e".into(), script.into()];
        r.env = BTreeMap::from([
            ("ANTHROPIC_API_KEY".to_owned(), "secret-key".to_owned()),
            ("KEEP_ME".to_owned(), "yes".to_owned()),
        ]);
        let out = test_resolved(&d, &r);
        assert_eq!(
            out.server_name.as_deref(),
            Some("false-yes"),
            "{}",
            out.message
        );
    }
}
