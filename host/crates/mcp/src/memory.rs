//! Calling one tool of the hub's shared-memory server from the host.
//!
//! Chats reach the hub's memory through their worker. Settings → Skills & rules does too, to list
//! the memories of a layer and move one between layers; those run here, once, as a plain request:
//! initialize, tell the server we are ready, call the tool. The server is a [`ResolvedServer`] (its
//! references already resolved for the person the call acts as), so the plain-http address rule of
//! the connection test applies, and no secret is ever in an error.

use crate::probe::{agent, init_request, initialized_note, pin_target, post, rpc_result, scrub};
use crate::types::{ResolvedServer, TransportKind};
use serde_json::{json, Value};

/// Why a call produced no answer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CallError {
    /// The server cannot be used now: not reachable, not over http, or it refused our token.
    Unreachable(String),
    /// The server was reached and the tool said no.
    Failed(String),
}

impl CallError {
    pub fn message(&self) -> &str {
        match self {
            CallError::Unreachable(m) | CallError::Failed(m) => m,
        }
    }
}

/// Call `tool` on `server` with `args` and give back its structured answer.
pub fn call_tool(server: &ResolvedServer, tool: &str, args: Value) -> Result<Value, CallError> {
    let unreachable = |why: &str| CallError::Unreachable(scrub(why, &server.secrets));
    if let Some(why) = &server.error {
        return Err(unreachable(why));
    }
    if server.transport != TransportKind::Http {
        return Err(unreachable("The memory server is not a URL server."));
    }
    let url = server
        .url
        .as_deref()
        .and_then(|u| url::Url::parse(u).ok())
        .ok_or_else(|| unreachable("The memory server's address is not valid."))?;
    let target = pin_target(&url, server.allow_private_http).map_err(|why| unreachable(&why))?;
    let agent = agent();
    let first = post(&agent, &target, &server.headers, None, &init_request())
        .map_err(|_| unreachable("The hub could not be reached."))?;
    if matches!(first.status, 401 | 403) {
        return Err(unreachable(
            "The hub refused Helm's token, or this machine's token may not act for a person.",
        ));
    }
    if !(200..300).contains(&first.status) {
        return Err(unreachable(&format!(
            "The hub answered {} to the first request.",
            first.status
        )));
    }
    rpc_result(&first.body, 1).map_err(|why| unreachable(&why))?;
    let session = first.session.as_deref();
    let _ = post(
        &agent,
        &target,
        &server.headers,
        session,
        &initialized_note(),
    );
    let request = json!({
        "jsonrpc": "2.0", "id": 3, "method": "tools/call",
        "params": { "name": tool, "arguments": args }
    });
    let reply = post(&agent, &target, &server.headers, session, &request)
        .map_err(|_| unreachable("The hub could not be reached."))?;
    if !(200..300).contains(&reply.status) {
        return Err(CallError::Failed(format!(
            "The hub answered {} to {tool}.",
            reply.status
        )));
    }
    let result = rpc_result(&reply.body, 3)
        .map_err(|why| CallError::Failed(scrub(&why, &server.secrets)))?;
    let text = result["content"][0]["text"].as_str().unwrap_or("");
    if result["isError"].as_bool() == Some(true) {
        let why = if text.is_empty() { "The hub refused that." } else { text };
        return Err(CallError::Failed(scrub(why, &server.secrets)));
    }
    if let Some(structured) = result.get("structuredContent").filter(|v| !v.is_null()) {
        return Ok(structured.clone());
    }
    serde_json::from_str(text)
        .map_err(|_| CallError::Failed("The hub's answer was not understood.".to_owned()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::{Arc, Mutex};

    type Seen = Arc<Mutex<Vec<(Option<String>, Value)>>>;

    /// A stand-in hub on a loopback port that answers `requests` requests, one connection each.
    fn serve(requests: usize) -> (String, Seen, std::thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/mcp", listener.local_addr().unwrap());
        let seen: Seen = Arc::default();
        let log = seen.clone();
        let handle = std::thread::spawn(move || {
            for _ in 0..requests {
                let (mut s, _) = listener.accept().unwrap();
                let mut buf = Vec::new();
                let mut chunk = [0u8; 4096];
                let (head_end, len) = loop {
                    let n = s.read(&mut chunk).unwrap();
                    if n == 0 {
                        return;
                    }
                    buf.extend_from_slice(&chunk[..n]);
                    if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                        let head = String::from_utf8_lossy(&buf[..i]).to_lowercase();
                        let len = head
                            .lines()
                            .find_map(|l| l.strip_prefix("content-length:"))
                            .and_then(|v| v.trim().parse::<usize>().ok())
                            .unwrap_or(0);
                        break (i + 4, len);
                    }
                };
                while buf.len() < head_end + len {
                    let n = s.read(&mut chunk).unwrap();
                    if n == 0 {
                        break;
                    }
                    buf.extend_from_slice(&chunk[..n]);
                }
                let head = String::from_utf8_lossy(&buf[..head_end]).to_lowercase();
                let person = head
                    .lines()
                    .find_map(|l| l.strip_prefix("x-helm-person:"))
                    .map(|v| v.trim().to_owned());
                let body: Value = serde_json::from_slice(&buf[head_end..head_end + len]).unwrap();
                log.lock().unwrap().push((person, body.clone()));
                let id = body["id"].clone();
                let (status, extra, text) = match body["method"].as_str() {
                    Some("initialize") => (
                        "200 OK",
                        "Mcp-Session-Id: s1\r\n",
                        json!({ "jsonrpc": "2.0", "id": id, "result": {
                            "protocolVersion": "2025-03-26", "serverInfo": { "name": "helm-hub" } } })
                        .to_string(),
                    ),
                    Some("tools/call") => {
                        let result = if body["params"]["name"] == "memory_update" {
                            json!({ "content": [{ "type": "text", "text": "That memory is not yours." }],
                                    "isError": true })
                        } else {
                            json!({ "content": [{ "type": "text", "text": "{}" }],
                                    "structuredContent": { "count": 1, "results": [{ "id": 5 }] },
                                    "isError": false })
                        };
                        (
                            "200 OK",
                            "",
                            json!({ "jsonrpc": "2.0", "id": id, "result": result }).to_string(),
                        )
                    }
                    _ => ("202 Accepted", "", String::new()),
                };
                let reply = format!(
                    "HTTP/1.1 {status}\r\nContent-Type: application/json\r\n{extra}Content-Length: {}\r\nConnection: close\r\n\r\n{text}",
                    text.len()
                );
                s.write_all(reply.as_bytes()).unwrap();
            }
        });
        (url, seen, handle)
    }

    fn server(url: &str, person: Option<&str>) -> ResolvedServer {
        let mut headers = BTreeMap::new();
        headers.insert("Authorization".to_owned(), "Bearer tok-secret".to_owned());
        if let Some(p) = person {
            headers.insert("X-Helm-Person".to_owned(), p.to_owned());
        }
        ResolvedServer {
            id: "helm_hub".into(),
            name: "Hub".into(),
            transport: TransportKind::Http,
            command: None,
            args: vec![],
            env: BTreeMap::new(),
            url: Some(url.into()),
            headers,
            allow_private_http: false,
            secrets: vec!["tok-secret".into()],
            guidance: None,
            error: None,
        }
    }

    #[test]
    fn a_tool_call_acts_as_the_person_and_failures_are_told_apart() {
        let (url, seen, handle) = serve(6);
        let ok = call_tool(&server(&url, Some("7")), "memory_list", json!({ "limit": 5 })).unwrap();
        assert_eq!(ok["results"][0]["id"], 5);
        let no = call_tool(&server(&url, Some("7")), "memory_update", json!({ "id": 5 }));
        assert_eq!(
            no,
            Err(CallError::Failed("That memory is not yours.".to_owned()))
        );
        handle.join().unwrap();
        let seen = seen.lock().unwrap();
        assert_eq!(seen.len(), 6);
        assert!(seen.iter().all(|(p, _)| p.as_deref() == Some("7")));
        assert_eq!(seen[2].1["params"]["name"], "memory_list");
        assert_eq!(seen[2].1["params"]["arguments"]["limit"], 5);

        // A hub that is not there, a server with a resolve error, and one that is not a URL.
        let gone = TcpListener::bind("127.0.0.1:0").unwrap();
        let dead = format!("http://{}/mcp", gone.local_addr().unwrap());
        drop(gone);
        let mut broken = server(&url, None);
        broken.error = Some("Header Authorization: the saved secret could not be read.".into());
        let mut program = server(&url, None);
        program.transport = TransportKind::Stdio;
        for s in [server(&dead, None), broken, program] {
            let e = call_tool(&s, "memory_list", json!({})).unwrap_err();
            assert!(matches!(e, CallError::Unreachable(_)), "{e:?}");
            assert!(!e.message().contains("tok-secret"));
        }
    }
}
