//! The Brave Search client: one GET, the answer cleaned into plain text, and every refusal put
//! in words an agent or the user can act on.
//!
//! The key travels in one request header to one address and nowhere else: redirects are off, the
//! address is Brave's (a debug build may point it at a loopback test server, see
//! [`resolve_endpoint`]), and no message built here contains the key, the request or the raw
//! answer.

use serde::Serialize;
use serde_json::Value;
use std::time::Duration;

pub const DEFAULT_ENDPOINT: &str = "https://api.search.brave.com/res/v1/web/search";
const PATH: &str = "/res/v1/web/search";
/// Debug builds only: a loopback address standing in for Brave.
pub const TEST_URL_ENV: &str = "HELM_TEST_BRAVE_URL";

const TIMEOUT: Duration = Duration::from_secs(15);
const MAX_BODY: u64 = 2 * 1024 * 1024;

pub const MAX_QUERY_CHARS: usize = 400;
pub const DEFAULT_COUNT: u8 = 8;
pub const MAX_COUNT: u8 = 20;
/// Past day, week, month, year.
pub const FRESHNESS: [&str; 4] = ["pd", "pw", "pm", "py"];

const MAX_TITLE: usize = 200;
const MAX_SNIPPET: usize = 500;
const MAX_URL: usize = 1_000;
const MAX_AGE: usize = 60;

/// One search result as an agent sees it. All three texts are other people's words.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Hit {
    pub title: String,
    pub url: String,
    pub snippet: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub age: Option<String>,
}

/// Why a search did not happen, in plain words. Never carries the key, the query or Brave's text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Refusal(pub String);

impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// The address searches go to. A release build always uses Brave's. A debug build honours
/// [`TEST_URL_ENV`] too, but only for a plain-http loopback address, so the key can never be sent
/// to a stranger by an environment variable.
pub fn endpoint() -> String {
    if cfg!(debug_assertions) {
        resolve_endpoint(std::env::var(TEST_URL_ENV).ok().as_deref())
    } else {
        DEFAULT_ENDPOINT.to_owned()
    }
}

/// The override `raw` if it is `http://` on this computer (a bare address gets Brave's path),
/// else Brave's own address.
pub fn resolve_endpoint(raw: Option<&str>) -> String {
    let Some(rest) = raw.map(str::trim).and_then(|r| r.strip_prefix("http://")) else {
        return DEFAULT_ENDPOINT.to_owned();
    };
    let (authority, path) = rest.split_at(rest.find('/').unwrap_or(rest.len()));
    let host = match authority.strip_prefix('[').and_then(|r| r.find(']')) {
        Some(end) => &authority[..end + 2],
        None => authority.rsplit_once(':').map_or(authority, |(h, _)| h),
    };
    if authority.contains('@') || !matches!(host, "127.0.0.1" | "localhost" | "[::1]") {
        return DEFAULT_ENDPOINT.to_owned();
    }
    let path = if path.is_empty() || path == "/" {
        PATH
    } else {
        path
    };
    format!("http://{authority}{path}")
}

/// Text from the web as one short plain line: tags dropped, entities decoded, control and
/// invisible direction characters removed, runs of space collapsed, cut at `max` characters.
pub fn plain(raw: &str, max: usize) -> String {
    let mut text = String::with_capacity(raw.len().min(max * 2));
    let mut in_tag = false;
    let mut chars = raw.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            // A "<" starts a tag only before a letter, "/" or "!": "a < b" stays text.
            '<' if !in_tag
                && chars
                    .peek()
                    .is_some_and(|n| n.is_alphabetic() || "/!".contains(*n)) =>
            {
                in_tag = true
            }
            '>' if in_tag => {
                in_tag = false;
                text.push(' ');
            }
            _ if in_tag => {}
            _ => text.push(c),
        }
    }
    let decoded = decode_entities(&text);
    let cleaned: String = decoded
        .chars()
        .filter_map(|c| match c {
            '\u{200b}'..='\u{200f}'
            | '\u{202a}'..='\u{202e}'
            | '\u{2060}'..='\u{2064}'
            | '\u{2066}'..='\u{2069}'
            | '\u{feff}' => None,
            c if c.is_control() || c.is_whitespace() => Some(' '),
            c => Some(c),
        })
        .collect();
    let mut out = String::new();
    for word in cleaned.split_whitespace() {
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(word);
    }
    if out.chars().count() > max {
        let mut cut: String = out.chars().take(max.saturating_sub(1)).collect();
        cut.truncate(cut.trim_end().len());
        cut.push('\u{2026}');
        return cut;
    }
    out
}

fn decode_entities(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find('&') {
        out.push_str(&rest[..i]);
        rest = &rest[i..];
        let decoded = rest.find(';').filter(|&end| end <= 10).and_then(|end| {
            let name = &rest[1..end];
            let c = match name {
                "amp" => Some('&'),
                "lt" => Some('<'),
                "gt" => Some('>'),
                "quot" => Some('"'),
                "apos" => Some('\''),
                "nbsp" => Some(' '),
                _ => name.strip_prefix('#').and_then(|n| {
                    let code = match n.strip_prefix(['x', 'X']) {
                        Some(hex) => u32::from_str_radix(hex, 16).ok(),
                        None => n.parse().ok(),
                    };
                    code.and_then(char::from_u32)
                }),
            };
            c.map(|c| (c, end))
        });
        match decoded {
            Some((c, end)) => {
                out.push(c);
                rest = &rest[end + 1..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// A result address Helm will pass on: http or https, one line, no spaces.
fn safe_url(raw: &str) -> Option<String> {
    let u = raw.trim();
    let ok = (u.starts_with("https://") || u.starts_with("http://"))
        && u.len() <= MAX_URL
        && !u.chars().any(|c| c.is_control() || c.is_whitespace());
    ok.then(|| u.to_owned())
}

/// The results in a 200 answer; `None` when it is not a Brave answer at all.
fn parse(body: &str) -> Option<Vec<Hit>> {
    let v: Value = serde_json::from_str(body).ok()?;
    if !v.is_object() {
        return None;
    }
    let results = v["web"]["results"].as_array().cloned().unwrap_or_default();
    Some(
        results
            .iter()
            .filter_map(|r| {
                let url = safe_url(r["url"].as_str()?)?;
                let age = r["age"]
                    .as_str()
                    .map(|a| plain(a, MAX_AGE))
                    .filter(|a| !a.is_empty());
                Some(Hit {
                    title: plain(r["title"].as_str().unwrap_or(""), MAX_TITLE),
                    url,
                    snippet: plain(r["description"].as_str().unwrap_or(""), MAX_SNIPPET),
                    age,
                })
            })
            .collect(),
    )
}

fn refuse(msg: &str) -> Refusal {
    Refusal(msg.to_owned())
}

/// What a non-200 status means, in plain words.
fn refusal_for(status: u16, body: &str) -> Refusal {
    let quota = serde_json::from_str::<Value>(body)
        .ok()
        .and_then(|v| v["error"]["code"].as_str().map(|c| c.to_ascii_uppercase()))
        .is_some_and(|c| c.contains("QUOTA"));
    match status {
        401 | 403 => refuse(
            "Brave did not accept the search key (the key is wrong, expired or switched off). \
             The user can replace it in Settings > Web search.",
        ),
        402 => refuse(
            "Brave refused the search (HTTP 402): this key's search allowance has run out. \
             No more searches until it renews or the plan is changed.",
        ),
        422 => refuse("Brave refused that query. Try shorter, plainer wording."),
        429 if quota => refuse(
            "Brave refused the search (HTTP 429): this key's monthly allowance is used up, which \
             is the free plan's limit. No more searches until it renews or the plan is raised.",
        ),
        429 => refuse(
            "Brave refused the search (HTTP 429): too many searches too quickly, which is the \
             free plan's limit. Wait a few seconds before searching again, and search less.",
        ),
        500..=599 => refuse(&format!(
            "Brave had a problem on its side (HTTP {status}). Try again later."
        )),
        _ => refuse(&format!(
            "Brave answered with something unexpected (HTTP {status})."
        )),
    }
}

/// One search. `count` and `freshness` are already checked by the caller.
pub fn search(
    endpoint: &str,
    key: &str,
    query: &str,
    count: u8,
    freshness: Option<&str>,
) -> Result<Vec<Hit>, Refusal> {
    let agent = ureq::Agent::config_builder()
        .http_status_as_error(false)
        .max_redirects(0)
        .timeout_global(Some(TIMEOUT))
        .user_agent("helm")
        .build()
        .new_agent();
    let mut req = agent
        .get(endpoint)
        .header("Accept", "application/json")
        .header("X-Subscription-Token", key)
        .query("q", query)
        .query("count", count.to_string())
        .query("result_filter", "web")
        .query("text_decorations", "false");
    if let Some(f) = freshness {
        req = req.query("freshness", f);
    }
    let mut res = match req.call() {
        Ok(r) => r,
        Err(ureq::Error::Timeout(_)) => return Err(refuse("Brave did not answer in time.")),
        Err(_) => {
            return Err(refuse(
                "Could not reach Brave. Check the internet connection.",
            ))
        }
    };
    let status = res.status().as_u16();
    let body = res
        .body_mut()
        .with_config()
        .limit(MAX_BODY)
        .read_to_string()
        .unwrap_or_default();
    if status == 200 {
        return parse(&body).ok_or_else(|| refuse("Brave's answer could not be read."));
    }
    Err(refusal_for(status, &body))
}
