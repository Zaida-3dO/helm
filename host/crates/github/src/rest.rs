//! The REST read allowlist. The renderer may only GET these routes; a path must match one
//! template segment by segment, each placeholder accepting a narrow shape, and only the listed
//! query keys. Writes are never renderer routes: the checked write ops build their own.

use crate::error::{GhError, GhResult};
use serde_json::Value;

const MAX_PATH: usize = 4096;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Seg {
    Lit(&'static str),
    Owner,
    Repo,
    Number,
    Sha,
    Range,
    /// One or more percent-encoded file path segments; always last.
    FilePath,
}

struct Route {
    template: &'static [Seg],
    /// `page` and `per_page` are accepted.
    list: bool,
    /// `ref` is accepted.
    git_ref: bool,
}

use Seg::*;

const fn r(template: &'static [Seg], list: bool) -> Route {
    Route {
        template,
        list,
        git_ref: false,
    }
}

const R: Seg = Lit("repos");

const ROUTES: &[Route] = &[
    r(&[Lit("user")], false),
    r(&[R, Owner, Repo], false),
    r(&[R, Owner, Repo, Lit("labels")], true),
    r(&[R, Owner, Repo, Lit("pulls"), Number], false),
    r(&[R, Owner, Repo, Lit("pulls"), Number, Lit("files")], true),
    r(
        &[R, Owner, Repo, Lit("pulls"), Number, Lit("commits")],
        true,
    ),
    r(
        &[R, Owner, Repo, Lit("pulls"), Number, Lit("comments")],
        true,
    ),
    r(
        &[R, Owner, Repo, Lit("pulls"), Number, Lit("reviews")],
        true,
    ),
    r(
        &[R, Owner, Repo, Lit("issues"), Number, Lit("comments")],
        true,
    ),
    r(&[R, Owner, Repo, Lit("compare"), Range], true),
    r(
        &[R, Owner, Repo, Lit("commits"), Sha, Lit("check-runs")],
        true,
    ),
    r(&[R, Owner, Repo, Lit("commits"), Sha, Lit("status")], false),
    Route {
        template: &[R, Owner, Repo, Lit("contents"), FilePath],
        list: false,
        git_ref: true,
    },
];

/// An owner or repository name: 1–100 of letters, digits, `-`, `_`, `.`, and not `.` or `..`.
pub fn is_name(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 100
        && s != "."
        && s != ".."
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
}

/// A pull request or issue number: digits, no leading zero, at most 10 digits.
pub fn is_number(s: &str) -> bool {
    !s.is_empty() && s.len() <= 10 && !s.starts_with('0') && s.bytes().all(|b| b.is_ascii_digit())
}

fn is_sha(s: &str) -> bool {
    (7..=64).contains(&s.len()) && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// A full commit sha: exactly 40 lowercase hex characters.
pub fn is_full_sha(s: &str) -> bool {
    s.len() == 40 && is_sha(s)
}

fn is_range(s: &str) -> bool {
    matches!(s.split_once("..."), Some((a, b)) if is_sha(a) && is_sha(b))
}

/// One percent-encoded path segment that cannot decode to a separator, a control character,
/// `.` or `..`.
fn is_file_segment(s: &str) -> bool {
    if s.is_empty() || s.len() > 255 {
        return false;
    }
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'%' => {
                let hex = |c: u8| (c as char).to_digit(16);
                match (
                    b.get(i + 1).and_then(|c| hex(*c)),
                    b.get(i + 2).and_then(|c| hex(*c)),
                ) {
                    (Some(h), Some(l)) => out.push((h * 16 + l) as u8),
                    _ => return false,
                }
                i += 3;
            }
            c if c.is_ascii_alphanumeric() || matches!(c, b'-' | b'.' | b'_' | b'~') => {
                out.push(c);
                i += 1;
            }
            _ => return false,
        }
    }
    let Ok(decoded) = String::from_utf8(out) else {
        return false;
    };
    decoded != "."
        && decoded != ".."
        && !decoded
            .chars()
            .any(|c| c == '/' || c == '\\' || c.is_control())
}

/// A git ref for the contents route: 1–255 of `A-Za-z0-9._/-`, not starting with `/`, `-` or `.`,
/// no `..`.
fn is_ref(s: &str) -> bool {
    (1..=255).contains(&s.len())
        && !s.starts_with(['/', '-', '.'])
        && !s.contains("..")
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'/' | b'-'))
}

fn matches(template: &[Seg], segs: &[&str]) -> bool {
    let mut i = 0;
    for (n, t) in template.iter().enumerate() {
        if *t == FilePath {
            let rest = &segs[i.min(segs.len())..];
            return n == template.len() - 1
                && !rest.is_empty()
                && rest.iter().all(|s| is_file_segment(s));
        }
        let Some(s) = segs.get(i) else {
            return false;
        };
        let ok = match t {
            Lit(l) => s == l,
            Owner | Repo => is_name(s),
            Number => is_number(s),
            Sha => is_sha(s),
            Range => is_range(s),
            FilePath => unreachable!(),
        };
        if !ok {
            return false;
        }
        i += 1;
    }
    i == segs.len()
}

fn query_value(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => n.as_u64().map(|n| n.to_string()),
        _ => None,
    }
}

/// Check a renderer GET and build the endpoint `gh api` is given (no leading slash, query
/// appended in a fixed order).
pub fn endpoint(path: &str, query: Option<&Value>) -> GhResult<String> {
    if path.len() > MAX_PATH || !path.starts_with('/') || path.contains(['?', '#']) {
        return Err(GhError::invalid(
            "A GitHub path must start with / and carry no query or fragment.",
        ));
    }
    let segs: Vec<&str> = path[1..].split('/').collect();
    let route = ROUTES
        .iter()
        .find(|r| matches(r.template, &segs))
        .ok_or_else(|| GhError::invalid("That GitHub route is not one Helm reads."))?;
    let mut pairs: Vec<(String, String)> = Vec::new();
    match query {
        None | Some(Value::Null) => {}
        Some(Value::Object(map)) => {
            for (k, v) in map {
                let val = query_value(v)
                    .ok_or_else(|| GhError::invalid(format!("Query value {k} is not usable.")))?;
                let ok = match k.as_str() {
                    "page" if route.list => {
                        is_number(&val) && val.parse::<u32>().is_ok_and(|n| n <= 1000)
                    }
                    "per_page" if route.list => {
                        is_number(&val) && val.parse::<u32>().is_ok_and(|n| n <= 100)
                    }
                    "ref" if route.git_ref => is_ref(&val),
                    _ => {
                        return Err(GhError::invalid(format!(
                            "Query key {k} is not accepted on that route."
                        )))
                    }
                };
                if !ok {
                    return Err(GhError::invalid(format!(
                        "Query value {k} is out of range."
                    )));
                }
                pairs.push((k.clone(), val));
            }
        }
        Some(_) => return Err(GhError::invalid("query must be an object.")),
    }
    pairs.sort();
    let mut out = path[1..].to_owned();
    for (n, (k, v)) in pairs.iter().enumerate() {
        out.push(if n == 0 { '?' } else { '&' });
        out.push_str(k);
        out.push('=');
        out.push_str(v);
    }
    Ok(out)
}

/// The page number of a `Link: <…>; rel="next"` URL, accepted only when it points back at the
/// same origin and the same API path (or GitHub's `repositories/<id>/…` form of it), and is 1–1000.
/// The renderer never receives a URL to follow.
pub fn next_page(link: Option<&str>, origin: &str, endpoint: &str) -> Option<u32> {
    let link = link?;
    let path = endpoint.split('?').next().unwrap_or(endpoint);
    for part in link.split(',') {
        let (url, params) = part.trim().split_once(';')?;
        if !params.split(';').any(|p| p.trim() == "rel=\"next\"") {
            continue;
        }
        let url = url.trim().strip_prefix('<')?.strip_suffix('>')?;
        let rest = url.strip_prefix(origin)?.strip_prefix('/')?;
        let (p, q) = rest.split_once('?').unwrap_or((rest, ""));
        if !same_path(p, path) {
            return None;
        }
        return q
            .split('&')
            .find_map(|kv| kv.strip_prefix("page="))
            .filter(|n| is_number(n))
            .and_then(|n| n.parse::<u32>().ok())
            .filter(|n| (1..=1000).contains(n));
    }
    None
}

fn same_path(link_path: &str, path: &str) -> bool {
    if link_path == path {
        return true;
    }
    // repositories/<id>/<tail> for repos/<owner>/<repo>/<tail>
    let (Some(l), Some(p)) = (
        link_path.strip_prefix("repositories/"),
        path.strip_prefix("repos/"),
    ) else {
        return false;
    };
    let (id, l_tail) = l.split_once('/').unwrap_or((l, ""));
    let mut p_it = p.splitn(3, '/');
    let (_, _, p_tail) = (p_it.next(), p_it.next(), p_it.next().unwrap_or(""));
    !id.is_empty() && id.bytes().all(|b| b.is_ascii_digit()) && l_tail == p_tail
}
