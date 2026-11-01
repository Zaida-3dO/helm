//! The few plain words that say what a running tool call is doing.
//!
//! Wherever Helm shows a running turn — the activity dock in the transcript, the progress
//! message that reaches a phone — a bare tool name (`Bash`, `mcp__<server>__<tool>`) reads as
//! noise. This reads the call's own arguments and answers with a short phrase instead: "Reading
//! summer.md", "Searching HVAC filter", "Gmail · Search threads: winter bills". The summary is
//! built once, when the call starts, and travels with the call: into the transcript row's
//! details and into the live-tool list, so every consumer shows the same words. It only ever
//! repeats what the call's arguments already say; nothing here reads the call's output.

use serde_json::Value;

/// A summary's length budget: it shares its line with an icon, a ticking timer and a Stop
/// button, and the phone's progress message shares one small budget across the whole turn.
const MAX_CHARS: usize = 80;

/// One line, inner whitespace collapsed, cut to the budget with a visible join.
fn phrase(s: &str) -> Option<String> {
    let mut out = String::new();
    let mut space = false;
    for c in s.chars() {
        if c.is_whitespace() {
            if !out.is_empty() {
                space = true;
            }
            continue;
        }
        if space {
            out.push(' ');
        }
        space = false;
        out.push(c);
    }
    if out.is_empty() {
        return None;
    }
    if out.chars().count() <= MAX_CHARS {
        return Some(out);
    }
    let cut: String = out.chars().take(MAX_CHARS - 1).collect();
    Some(format!("{}…", cut.trim_end()))
}

/// The first of these argument fields that carries real text.
fn first_str<'a>(args: &'a Value, keys: &[&str]) -> Option<&'a str> {
    keys.iter().find_map(|k| args.get(*k).and_then(Value::as_str)).filter(|s| !s.trim().is_empty())
}

/// The last segment of a path; either separator is a boundary.
fn file_name(path: &str) -> &str {
    let trimmed = path.trim_end_matches(['/', '\\']);
    match trimmed.rfind(['/', '\\']) {
        Some(i) => &trimmed[i + 1..],
        None => trimmed,
    }
}

/// Splits a tool's own name into words: underscores, dashes and dots separate; a capital after
/// a lowercase letter, or a capital run followed by lowercase, starts a new word
/// (`search_threads` to [search, threads]; `TodoWrite` to [Todo, Write]).
fn words(name: &str) -> Vec<String> {
    let chars: Vec<char> = name.chars().collect();
    let mut out: Vec<String> = Vec::new();
    let mut cur = String::new();
    for (i, &c) in chars.iter().enumerate() {
        if c == '_' || c == '-' || c == ' ' || c == '.' {
            if !cur.is_empty() {
                out.push(std::mem::take(&mut cur));
            }
            continue;
        }
        if c.is_uppercase() && !cur.is_empty() {
            let prev = chars[i - 1];
            let run_ends = chars.get(i + 1).is_some_and(|n| n.is_lowercase());
            if prev.is_lowercase() || prev.is_numeric() || (prev.is_uppercase() && run_ends) {
                out.push(std::mem::take(&mut cur));
            }
        }
        cur.push(c);
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

fn capitalize_first(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(first) => {
            let mut out: String = first.to_uppercase().collect();
            out.push_str(c.as_str());
            out
        }
        None => String::new(),
    }
}

/// A human reading of a tool's own name: "search_threads" to "Search threads", "TodoWrite" to
/// "Todo write". With `title_every` each word stands capitalized, for a server's name
/// ("Home Assistant").
fn readable(name: &str, title_every: bool) -> String {
    let name = name.strip_prefix("app_").unwrap_or(name);
    let parts = words(name);
    if parts.is_empty() {
        return String::new();
    }
    if title_every {
        return parts
            .iter()
            .map(|w| capitalize_first(w))
            .collect::<Vec<_>>()
            .join(" ");
    }
    capitalize_first(&parts.join(" "))
}

/// A fetched page reads as a page visit.
fn fetch_phrase(url: String) -> String {
    format!("Reading {url}")
}

/// A URL's page, as far as a line has room: host, plus its first path segment when it has one.
fn web_page(url: &str) -> Option<String> {
    let after = url.split_once("://").map(|(_, rest)| rest).unwrap_or(url);
    let authority = after.split(['/', '?', '#']).next().unwrap_or("");
    let host = authority.rsplit('@').next().unwrap_or(authority).split(':').next().unwrap_or("");
    if host.is_empty() {
        return None;
    }
    let host = host.strip_prefix("www.").unwrap_or(host);
    let seg = after[authority.len()..]
        .trim_start_matches('/')
        .split(['/', '?', '#'])
        .next()
        .unwrap_or("");
    Some(if seg.is_empty() {
        host.to_owned()
    } else {
        format!("{host}/{seg}")
    })
}

/// Leading words of a hosted connector's underscore-spelled domain name.
fn is_domain_fragment(w: &str) -> bool {
    w.eq_ignore_ascii_case("claude")
        || w.eq_ignore_ascii_case("ai")
        || w.eq_ignore_ascii_case("www")
}

/// `mcp__<server>__<tool>` to "<Server> · <Tool>", optionally with the call's query:
/// "Gmail · Search threads: winter bills". The raw dotted name never reaches a progress line.
fn mcp_friendly(name: &str, args: &Value) -> Option<String> {
    let (server, tool) = name.strip_prefix("mcp__")?.split_once("__")?;
    let mut server_words = words(server);
    // A hosted connector's name arrives with its domain spelled out in underscores; the
    // connector's own name is what follows. Drop the domain fragments.
    while server_words.first().is_some_and(|w| is_domain_fragment(w)) {
        server_words.remove(0);
    }
    // A server's name reads as a name: every word stands capitalized ("Home Assistant").
    let server_disp = server_words
        .iter()
        .map(|w| capitalize_first(w))
        .collect::<Vec<_>>()
        .join(" ");
    let tool_disp = readable(tool, false);
    let base = match (server_disp.is_empty(), tool_disp.is_empty()) {
        (true, true) => return None,
        (true, false) => tool_disp,
        (false, true) => server_disp,
        (false, false) => format!("{server_disp} · {tool_disp}"),
    };
    Some(match first_str(args, &["query", "search_query"]) {
        Some(q) => format!("{base}: {}", q.trim()),
        None => base,
    })
}

/// A short phrase for one tool call, or the empty string when the call has no usable name.
pub(crate) fn tool_summary(name: &str, args: &Value) -> String {
    let name = name.trim();
    if name.is_empty() {
        return String::new();
    }
    // A call that carries its own description already says what it is doing in plain words
    // (Bash and the agent-spawning tools do); quote it rather than re-derive it.
    if let Some(desc) = first_str(args, &["description"]) {
        if let Some(p) = phrase(desc) {
            return p;
        }
    }
    if name.starts_with("mcp__") {
        if let Some(m) = mcp_friendly(name, args) {
            if let Some(p) = phrase(&m) {
                return p;
            }
        }
    }
    let known: Option<String> = match name {
        "Read" => first_str(args, &["file_path"]).map(|p| format!("Reading {}", file_name(p))),
        "LS" => first_str(args, &["path", "file_path"])
            .map(|p| format!("Listing {}", file_name(p))),
        "Write" => first_str(args, &["file_path"]).map(|p| format!("Writing {}", file_name(p))),
        "Edit" | "MultiEdit" | "NotebookEdit" => first_str(args, &["file_path", "notebook_path"])
            .map(|p| format!("Editing {}", file_name(p))),
        "Grep" | "Glob" => first_str(args, &["pattern"]).map(|p| format!("Searching {}", p.trim())),
        "Bash" | "PowerShell" => first_str(args, &["command", "cmd"])
            .and_then(|c| c.lines().find(|l| !l.trim().is_empty()))
            .and_then(|c| phrase(c.trim())),
        "WebSearch" => first_str(args, &["query"]).map(|q| format!("Web search: {}", q.trim())),
        "WebFetch" => first_str(args, &["url"]).and_then(web_page).map(fetch_phrase),
        "TodoWrite" => Some("Updating the plan".to_owned()),
        _ => None,
    };
    if let Some(s) = known {
        if let Some(p) = phrase(&s) {
            return p;
        }
    }
    let bare = readable(name.trim_start_matches("mcp__"), false);
    phrase(&bare).unwrap_or_else(|| name.to_owned())
}
