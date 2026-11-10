//! Images in a pull request's text. GitHub serves them from hosts the renderer may not contact,
//! and for a private repository the address in the markdown only works with a sign-in. GitHub's own
//! rendering of the text carries a short-lived signed address for each image instead, so Helm reads
//! that rendering for the pull request being viewed, and fetches only an address found there, from
//! one of GitHub's image hosts, over https, with no credential of any kind. The picture goes to the
//! page as base64.

use crate::client::Client;
use crate::error::{Class, GhError, GhResult};
use crate::graphql;
use crate::rest::is_name;
use crate::transport::{Method, Request};
use crate::writes::check_pr;
use base64::Engine;
use serde::Deserialize;
use serde_json::{json, Value};
use std::time::Duration;

/// The hosts an image may come from.
const HOSTS: [&str; 4] = [
    "private-user-images.githubusercontent.com",
    "user-images.githubusercontent.com",
    "camo.githubusercontent.com",
    "raw.githubusercontent.com",
];
/// Vector images are left out: only pictures that cannot carry script or markup.
const TYPES: [&str; 4] = ["image/png", "image/jpeg", "image/gif", "image/webp"];
const MAX_IMAGE_BYTES: u64 = 8 * 1024 * 1024;
const MAX_URL: usize = 2048;
const TIMEOUT: Duration = Duration::from_secs(20);

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ImageArgs {
    pub owner: String,
    pub repo: String,
    pub number: u64,
    pub url: String,
}

/// Whether `url` is an https address on one of GitHub's image hosts, with no sign-in in it, no
/// port and no fragment.
pub fn image_host_ok(url: &str) -> bool {
    let Some(rest) = url.strip_prefix("https://") else {
        return false;
    };
    if url.len() > MAX_URL || url.chars().any(|c| c.is_control() || c == ' ' || c == '\\') {
        return false;
    }
    let host = rest.split(['/', '?']).next().unwrap_or("");
    !url.contains('#') && HOSTS.contains(&host)
}

fn unescape(s: &str) -> String {
    s.replace("&amp;", "&")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
}

/// The value of a quoted attribute in one tag, when the name stands alone (so `src` is not found
/// inside `data-canonical-src`).
fn attr(tag: &str, name: &str) -> Option<String> {
    let needle = format!("{name}=\"");
    let mut from = 0;
    while let Some(i) = tag[from..].find(&needle) {
        let at = from + i;
        let alone = tag[..at]
            .chars()
            .next_back()
            .is_some_and(char::is_whitespace);
        let start = at + needle.len();
        if alone {
            let end = tag[start..].find('"')?;
            return Some(unescape(&tag[start..start + end]));
        }
        from = start;
    }
    None
}

/// The `(address in the text, address to fetch)` pairs of every image in GitHub's rendering.
pub fn image_sources(html: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut rest = html;
    while let Some(i) = rest.find("<img") {
        rest = &rest[i + 4..];
        let end = rest.find('>').unwrap_or(rest.len());
        let tag = &rest[..end];
        if let Some(src) = attr(tag, "src") {
            let canonical = attr(tag, "data-canonical-src").unwrap_or_else(|| src.clone());
            out.push((canonical, src));
        }
        rest = &rest[end..];
    }
    out
}

/// Every rendered body in the answer of `pullRequestImages`.
fn bodies(pr: &Value, out: &mut Vec<String>) {
    match pr {
        Value::Object(m) => {
            for (k, v) in m {
                if k == "bodyHTML" {
                    if let Some(s) = v.as_str() {
                        out.push(s.to_owned());
                    }
                } else {
                    bodies(v, out);
                }
            }
        }
        Value::Array(a) => a.iter().for_each(|v| bodies(v, out)),
        _ => {}
    }
}

fn unseen() -> GhError {
    GhError::new(
        Class::Refused,
        "That image is not in this pull request, so Helm will not fetch it.",
    )
}

/// One image of the pull request being viewed: `{contentType, data}` with `data` in base64.
pub fn image(c: &Client, a: &ImageArgs) -> GhResult<Value> {
    check_pr(&a.owner, &a.repo, a.number)?;
    if !is_name(&a.owner) || a.url.len() > MAX_URL || !a.url.starts_with("https://") {
        return Err(GhError::invalid("That is not an image address."));
    }
    let doc = graphql::doc("pullRequestImages").expect("compiled document");
    let answer = c.read(&Request {
        method: Method::Post,
        endpoint: "graphql".into(),
        body: Some(graphql::body(
            doc,
            Some(&json!({"owner": a.owner, "repo": a.repo, "number": a.number})),
        )?),
    })?;
    let mut html = Vec::new();
    bodies(&answer.data["repository"]["pullRequest"], &mut html);
    let found = html
        .iter()
        .flat_map(|h| image_sources(h))
        .find(|(canonical, src)| *canonical == a.url || *src == a.url)
        .map(|(_, src)| src)
        .ok_or_else(unseen)?;
    if !image_host_ok(&found) {
        return Err(unseen());
    }
    fetch(&found)
}

fn fetch(url: &str) -> GhResult<Value> {
    let agent = ureq::Agent::config_builder()
        .http_status_as_error(false)
        .max_redirects(0)
        .timeout_global(Some(TIMEOUT))
        .user_agent("helm-home")
        .build()
        .new_agent();
    let failed = || GhError::new(Class::Network, "The image could not be fetched.");
    let mut res = agent.get(url).call().map_err(|_| failed())?;
    if res.status().as_u16() != 200 {
        return Err(failed());
    }
    let kind = res
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .map(|v| {
            v.split(';')
                .next()
                .unwrap_or("")
                .trim()
                .to_ascii_lowercase()
        })
        .unwrap_or_default();
    if !TYPES.contains(&kind.as_str()) {
        return Err(GhError::new(
            Class::Refused,
            "That file is not a picture Helm shows.",
        ));
    }
    let bytes = res
        .body_mut()
        .with_config()
        .limit(MAX_IMAGE_BYTES)
        .read_to_vec()
        .map_err(|e| match e {
            ureq::Error::BodyExceedsLimit(_) => {
                GhError::new(Class::TooLarge, "That image is too large to show.")
            }
            _ => failed(),
        })?;
    Ok(json!({
        "contentType": kind,
        "data": base64::engine::general_purpose::STANDARD.encode(bytes),
    }))
}
