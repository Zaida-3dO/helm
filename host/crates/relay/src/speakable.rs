//! Turning an agent's answer into something that is pleasant to hear.
//!
//! The voice chat's answers in conversation mode are read aloud by the phone, so they pass through
//! [`speakable`] first: code, tables, links, addresses, paths and long ids are taken out or
//! replaced by a word, markup is flattened to plain text, the text is split into sentences (each
//! at most [`MAX_SENTENCE_CHARS`] long), and the whole is cut to about [`MAX_WORDS`] words with an
//! offer of the rest. The full answer stays in the chat, so asking for more just works.
//!
//! The function is pure. [`Removed`] counts what had to be taken out, which tells how well the
//! agent follows the spoken-answer notes.

/// More words than this and the answer is cut.
pub const MAX_WORDS: usize = 150;
/// A cut answer keeps whole sentences while the running total stays within this.
pub const KEEP_WORDS: usize = 140;
/// A longer sentence is split at its last comma or space.
pub const MAX_SENTENCE_CHARS: usize = 400;
/// The most sentences one answer is sent as.
pub const MAX_SENTENCES: usize = 40;
/// What a cut answer ends with, as two sentences.
pub const OFFER: [&str; 2] = ["There's more.", "Want the rest?"];

const CODE_NOTE: &str = "I've left the code in the chat.";
const TABLE_NOTE: &str = "There's a table in the chat.";

/// What was taken out of the text, by kind (one count per block, link, address, path or id).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Removed {
    pub code: usize,
    pub tables: usize,
    pub links: usize,
    pub urls: usize,
    pub paths: usize,
    pub ids: usize,
    pub markup: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Speakable {
    pub sentences: Vec<String>,
    /// The cap cut the answer; its last sentences are the offer of the rest.
    pub truncated: bool,
    pub removed: Removed,
}

/// The cleanup, split and cap, in that order.
pub fn speakable(text: &str) -> Speakable {
    let mut removed = Removed::default();
    let mut sentences: Vec<String> = vec![];
    // Each paragraph, heading, list item and note is cleaned and split on its own, so the end of
    // one always ends a sentence.
    for unit in blocks(text, &mut removed) {
        let unit = inline(&unit, &mut removed);
        let unit = words_pass(&unit, &mut removed);
        let unit = tidy(&unit);
        for s in split_sentences(&unit) {
            sentences.extend(split_long(&s));
        }
    }
    let truncated = cap(&mut sentences);
    Speakable {
        sentences,
        truncated,
        removed,
    }
}

// ---- blocks: code, tables, headings, quotes and lists -------------------------------------

/// Block markup to plain paragraphs, headings and list items, each ending its own sentence.
fn blocks(text: &str, r: &mut Removed) -> Vec<String> {
    let lines: Vec<&str> = text.lines().collect();
    let mut units: Vec<String> = vec![];
    let mut para = String::new();
    let mut fence: Option<char> = None;
    let mut told_code = false;
    let mut told_table = false;
    let mut prev_blank = true;
    let mut in_indented = false;
    let mut i = 0;
    while i < lines.len() {
        let raw = lines[i].trim_end();
        let mut t = raw.trim_start();
        i += 1;
        if let Some(ch) = fence {
            if t.starts_with(&ch.to_string().repeat(3)) {
                fence = None;
            }
            continue;
        }
        if t.starts_with("```") || t.starts_with("~~~") {
            flush(&mut para, &mut units);
            fence = t.chars().next();
            r.code += 1;
            if !std::mem::replace(&mut told_code, true) {
                units.push(CODE_NOTE.to_owned());
            }
            prev_blank = false;
            continue;
        }
        if t.is_empty() {
            flush(&mut para, &mut units);
            prev_blank = true;
            in_indented = false;
            continue;
        }
        // Indented code: only after a blank line (or more code), and never a nested list item.
        // A paragraph that carries on a list item after a blank line is taken for code too.
        let indented = raw.starts_with("    ") || raw.starts_with('\t');
        if indented && (prev_blank || in_indented) && list_item(t).is_none() {
            if !in_indented {
                r.code += 1;
                if !std::mem::replace(&mut told_code, true) {
                    flush(&mut para, &mut units);
                    units.push(CODE_NOTE.to_owned());
                }
            }
            in_indented = true;
            prev_blank = false;
            continue;
        }
        in_indented = false;
        prev_blank = false;
        let next = lines.get(i).map(|l| l.trim());
        if t.contains('|') && (next.is_some_and(is_separator_row) || is_pipe_row(t)) {
            flush(&mut para, &mut units);
            r.tables += 1;
            if !std::mem::replace(&mut told_table, true) {
                units.push(TABLE_NOTE.to_owned());
            }
            while i < lines.len() && lines[i].contains('|') && !lines[i].trim().is_empty() {
                i += 1;
            }
            continue;
        }
        if is_rule(t) {
            flush(&mut para, &mut units);
            r.markup += 1;
            continue;
        }
        let mut quoted = false;
        while let Some(rest) = t.strip_prefix('>') {
            t = rest.trim_start();
            quoted = true;
        }
        if quoted {
            r.markup += 1;
            if t.is_empty() {
                continue;
            }
        }
        let hashes = t.chars().take_while(|c| *c == '#').count();
        if (1..=6).contains(&hashes) && t[hashes..].starts_with(' ') {
            flush(&mut para, &mut units);
            r.markup += 1;
            let head = t[hashes..].trim().trim_end_matches('#').trim();
            if !head.is_empty() {
                units.push(terminate(head));
            }
            continue;
        }
        if let Some(item) = list_item(t) {
            flush(&mut para, &mut units);
            r.markup += 1;
            let item = item.trim();
            if !item.is_empty() {
                units.push(terminate(item));
            }
            continue;
        }
        if !para.is_empty() {
            para.push(' ');
        }
        para.push_str(t);
    }
    flush(&mut para, &mut units);
    units
}

fn flush(para: &mut String, units: &mut Vec<String>) {
    let p = para.trim();
    if !p.is_empty() {
        units.push(terminate(p));
    }
    para.clear();
}

/// The text with a full stop if it has no end punctuation (a trailing colon, semicolon or comma
/// becomes one).
fn terminate(s: &str) -> String {
    let s = s.trim();
    let Some(last) = s.chars().last() else {
        return String::new();
    };
    match last {
        '.' | '?' | '!' | '…' => s.to_owned(),
        ':' | ';' | ',' | '-' | '—' => {
            let mut out = s[..s.len() - last.len_utf8()].trim_end().to_owned();
            out.push('.');
            out
        }
        c if c.is_alphanumeric() || matches!(c, ')' | '"' | '\'' | ']' | '`' | '*' | '_') => {
            format!("{s}.")
        }
        _ => s.to_owned(),
    }
}

/// The text after a list marker (`-`, `*`, `+`, `1.`, `1)`, with an optional checkbox).
fn list_item(t: &str) -> Option<&str> {
    let rest = if let Some(r) = ["- ", "* ", "+ "].iter().find_map(|m| t.strip_prefix(m)) {
        r
    } else {
        let digits = t.chars().take_while(char::is_ascii_digit).count();
        if !(1..=3).contains(&digits) {
            return None;
        }
        let after = &t[digits..];
        after
            .strip_prefix(". ")
            .or_else(|| after.strip_prefix(") "))?
    };
    let rest = rest.trim_start();
    for b in ["[ ] ", "[x] ", "[X] "] {
        if let Some(r) = rest.strip_prefix(b) {
            return Some(r);
        }
    }
    Some(rest)
}

fn is_rule(t: &str) -> bool {
    let c: Vec<char> = t.chars().filter(|c| !c.is_whitespace()).collect();
    c.len() >= 3 && matches!(c[0], '-' | '*' | '_') && c.iter().all(|x| *x == c[0])
}

fn is_separator_row(s: &str) -> bool {
    s.contains('|')
        && s.contains('-')
        && s.chars().all(|c| matches!(c, '|' | '-' | ':' | ' ' | '\t'))
}

fn is_pipe_row(t: &str) -> bool {
    t.starts_with('|') && t.ends_with('|') && t.len() > 1
}

// ---- inline: links, code spans, emphasis, tags --------------------------------------------

fn inline(s: &str, r: &mut Removed) -> String {
    let s = links(s, r);
    let s = html(&s, r);
    let s = emphasis(&s, r);
    unescape(&s)
}

/// `[text](url)` and `![alt](url)` become their text.
fn links(s: &str, r: &mut Removed) -> String {
    let c: Vec<char> = s.chars().collect();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < c.len() {
        if c[i] == '!' && c.get(i + 1) == Some(&'[') {
            if let Some((text, end)) = link_at(&c, i + 1) {
                r.links += 1;
                out.push_str(&text);
                i = end;
                continue;
            }
        } else if c[i] == '[' {
            if let Some((text, end)) = link_at(&c, i) {
                r.links += 1;
                if text.trim().is_empty() {
                    out.push_str("a link");
                } else {
                    out.push_str(&text);
                }
                i = end;
                continue;
            }
        }
        out.push(c[i]);
        i += 1;
    }
    out
}

/// A link starting at `[` at `at`: its text and the index just past its `)`.
fn link_at(c: &[char], at: usize) -> Option<(String, usize)> {
    let close = (at + 1..c.len()).find(|&j| c[j] == ']' || c[j] == '[')?;
    if c[close] != ']' || c.get(close + 1) != Some(&'(') {
        return None;
    }
    let end = (close + 2..c.len()).find(|&j| c[j] == ')' || c[j].is_whitespace())?;
    if c[end] != ')' {
        return None;
    }
    Some((c[at + 1..close].iter().collect(), end + 1))
}

/// Tags such as `<b>`, `</p>` and `<br/>` go; a bare `<` in prose stays.
fn html(s: &str, r: &mut Removed) -> String {
    let c: Vec<char> = s.chars().collect();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < c.len() {
        if c[i] == '<'
            && c.get(i + 1)
                .is_some_and(|n| n.is_ascii_alphabetic() || *n == '/')
        {
            let close = (i + 1..c.len().min(i + 120)).find(|&j| c[j] == '>' || c[j] == '<');
            if let Some(j) = close.filter(|&j| c[j] == '>') {
                r.markup += 1;
                out.push(' ');
                i = j + 1;
                continue;
            }
        }
        out.push(c[i]);
        i += 1;
    }
    out
}

/// Backticks, `*`, `**`, `~~` and emphasis underscores go; underscores inside a word stay.
fn emphasis(s: &str, r: &mut Removed) -> String {
    let c: Vec<char> = s.chars().collect();
    let mut out = String::with_capacity(s.len());
    let mut changed = false;
    for (i, &ch) in c.iter().enumerate() {
        let drop = match ch {
            '`' | '*' => true,
            '~' => c.get(i + 1) == Some(&'~') || (i > 0 && c[i - 1] == '~'),
            '_' => {
                let before = i > 0 && c[i - 1].is_alphanumeric();
                let after = c.get(i + 1).is_some_and(|n| n.is_alphanumeric());
                !(before && after)
            }
            _ => false,
        };
        if drop {
            changed = true;
        } else {
            out.push(ch);
        }
    }
    if changed {
        r.markup += 1;
    }
    out
}

fn unescape(s: &str) -> String {
    s.replace("&nbsp;", " ")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&amp;", "&")
}

// ---- words: addresses, paths and ids ------------------------------------------------------

/// What a word is, for speech.
enum Kind {
    Plain,
    Url,
    /// A path or file name; `Some(name)` for a bare all-capitals name such as `README.md`.
    Path(Option<String>),
    Id,
    /// Digits that follow "PR": only an id there.
    Number,
}

const DETERMINERS: &[&str] = &["the", "a", "an", "this", "that", "your", "my", "our", "its"];
/// A word that names a thing with an id: "PR 12" becomes "the PR".
const NOUNS: &[&str] = &["pr", "issue", "commit"];

fn words_pass(s: &str, r: &mut Removed) -> String {
    let mut out: Vec<String> = vec![];
    for word in s.split_whitespace() {
        let (lead, core, trail) = peel(word);
        if core.is_empty() {
            out.push(word.to_owned());
            continue;
        }
        let prev = out
            .last()
            .map(|w| peel(w).1.to_lowercase())
            .unwrap_or_default();
        let after_noun = NOUNS.contains(&prev.as_str());
        let kind = classify(core, prev == "pr");
        match kind {
            Kind::Plain => out.push(word.to_owned()),
            Kind::Url => {
                r.urls += 1;
                out.push(format!("{lead}a link{trail}"));
            }
            Kind::Path(name) => {
                r.paths += 1;
                let article = DETERMINERS.contains(&prev.as_str());
                let said = match (name, article) {
                    (Some(n), true) => n,
                    (Some(n), false) => format!("the {n}"),
                    (None, true) => "file".to_owned(),
                    (None, false) => "a file".to_owned(),
                };
                // The pieces of one path that has spaces in it say "a file" once.
                if (prev == "file" || prev == "a file") && said.ends_with("file") && lead.is_empty()
                {
                    if let Some(last) = out.last_mut() {
                        last.push_str(trail);
                    }
                } else {
                    out.push(format!("{lead}{said}{trail}"));
                }
            }
            Kind::Id | Kind::Number => {
                r.ids += 1;
                // The sentence's own punctuation moves back to the word before.
                let keep: String = trail
                    .chars()
                    .filter(|c| matches!(*c, '.' | ',' | ';' | ':' | '!' | '?' | '…'))
                    .collect();
                if after_noun {
                    // "PR #12" is "the PR"; "the PR #12" is "the PR".
                    let before_noun: Option<String> = out
                        .len()
                        .checked_sub(2)
                        .map(|k| peel(&out[k]).1.to_lowercase());
                    if let Some(last) = out.last_mut() {
                        if !before_noun.is_some_and(|b| DETERMINERS.contains(&b.as_str())) {
                            *last = format!("the {last}");
                        }
                    }
                }
                if let Some(last) = out.last_mut() {
                    last.push_str(&keep);
                }
            }
        }
    }
    out.join(" ")
}

/// A word split into leading punctuation, the core and trailing punctuation.
fn peel(word: &str) -> (&str, &str, &str) {
    let lead_len = word
        .char_indices()
        .find(|(_, c)| !matches!(*c, '(' | '[' | '{' | '<' | '"' | '\'' | '“' | '‘'))
        .map(|(i, _)| i)
        .unwrap_or(word.len());
    let rest = &word[lead_len..];
    let core_len = rest
        .char_indices()
        .rev()
        .find(|(_, c)| {
            !matches!(
                *c,
                ')' | ']'
                    | '}'
                    | '>'
                    | '"'
                    | '\''
                    | '”'
                    | '’'
                    | '.'
                    | ','
                    | ';'
                    | ':'
                    | '!'
                    | '?'
                    | '…'
            )
        })
        .map(|(i, c)| i + c.len_utf8())
        .unwrap_or(0);
    (&word[..lead_len], &rest[..core_len], &rest[core_len..])
}

fn classify(core: &str, after_pr: bool) -> Kind {
    let lower = core.to_ascii_lowercase();
    if lower.starts_with("http://")
        || lower.starts_with("https://")
        || lower.starts_with("ftp://")
        || lower.starts_with("www.")
    {
        return Kind::Url;
    }
    if core.contains('@') && !core.contains('/') && !core.contains('\\') {
        return Kind::Plain;
    }
    if after_pr && core.len() <= 6 && core.chars().all(|c| c.is_ascii_digit()) {
        return Kind::Number;
    }
    if is_id(core) {
        return Kind::Id;
    }
    match path_kind(core) {
        Some(name) => Kind::Path(name),
        None => Kind::Plain,
    }
}

fn is_hex(core: &str) -> bool {
    core.chars().all(|c| c.is_ascii_hexdigit())
}

fn is_id(core: &str) -> bool {
    let n = core.chars().count();
    // #1234
    if let Some(d) = core.strip_prefix('#') {
        return (2..=7).contains(&d.len()) && d.chars().all(|c| c.is_ascii_digit());
    }
    // 8-4-4-4-12 hex.
    if n == 36 {
        let parts: Vec<&str> = core.split('-').collect();
        if parts.len() == 5
            && parts
                .iter()
                .zip([8, 4, 4, 4, 12])
                .all(|(p, len)| p.len() == len && is_hex(p))
        {
            return true;
        }
    }
    // A hash: seven or more hex digits with both a digit and a letter.
    if (7..=64).contains(&n)
        && is_hex(core)
        && core.chars().any(|c| c.is_ascii_digit())
        && core.chars().any(|c| c.is_ascii_alphabetic())
    {
        return true;
    }
    // A token: a long mixed-case run with digits.
    n >= 16
        && core
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '+' | '='))
        && core.chars().any(|c| c.is_ascii_digit())
        && core.chars().any(|c| c.is_ascii_uppercase())
        && core.chars().any(|c| c.is_ascii_lowercase())
}

/// File extensions that make a bare `name.ext` a file.
const EXTENSIONS: &[&str] = &[
    "rs", "kt", "kts", "ts", "tsx", "js", "jsx", "mjs", "json", "toml", "yaml", "yml", "md", "py",
    "java", "gradle", "xml", "html", "css", "sh", "ps1", "txt", "lock", "sql", "cfg", "ini", "log",
    "csv", "exe", "dll", "bat", "cmd", "conf",
];

/// `Some(None)`: a path or file; `Some(Some(name))`: a bare all-capitals file name such as
/// `README.md` (said as "the README"); `None`: not a path.
fn path_kind(core: &str) -> Option<Option<String>> {
    let b = core.as_bytes();
    if b.len() < 2 {
        return None;
    }
    let drive =
        b.len() >= 3 && b[0].is_ascii_alphabetic() && b[1] == b':' && matches!(b[2], b'\\' | b'/');
    if drive
        || core.starts_with("\\\\")
        || ["./", "../", "~/", ".\\", "..\\"]
            .iter()
            .any(|p| core.starts_with(p))
    {
        return Some(None);
    }
    let seps = core.chars().filter(|c| matches!(*c, '/' | '\\')).count();
    let segments: Vec<&str> = core.split(['/', '\\']).collect();
    let last = *segments.last().unwrap_or(&"");
    if core.starts_with('/') {
        return (seps >= 2 || has_extension(last)).then_some(None);
    }
    if seps >= 1 {
        if segments.iter().any(|s| s.is_empty()) && !core.ends_with('/') {
            return None;
        }
        let words_only = segments
            .iter()
            .all(|s| s.chars().all(|c| c.is_ascii_alphabetic()) && s.len() <= 4);
        let numbers_only = segments
            .iter()
            .all(|s| s.chars().all(|c| c.is_ascii_digit()));
        let ok = has_extension(last) || (seps >= 2 && !words_only && !numbers_only);
        return ok.then_some(None);
    }
    if has_extension(last) {
        let (base, _) = last.rsplit_once('.')?;
        if base.len() >= 3 && base.chars().all(|c| c.is_ascii_uppercase()) {
            return Some(Some(base.to_owned()));
        }
        return Some(None);
    }
    None
}

fn has_extension(seg: &str) -> bool {
    let Some((base, ext)) = seg.rsplit_once('.') else {
        return false;
    };
    let ext = ext.to_ascii_lowercase();
    !base.is_empty()
        && EXTENSIONS.contains(&ext.as_str())
        && base
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'))
        // Node.js, Vue.js and the like are products, not files.
        && !(ext == "js" && base.chars().next().is_some_and(|c| c.is_ascii_uppercase()))
}

// ---- tidy, split and cap ------------------------------------------------------------------

/// Single spaces, no space before punctuation, runs of the same punctuation made one.
fn tidy(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut prev: Option<char> = None;
    let mut dots = 0usize;
    // A space taken off before punctuation comes back if a word follows with none.
    let mut respace = false;
    for ch in s.chars() {
        if ch == '.' {
            if dots == 0 && out.ends_with(' ') {
                out.pop();
                respace = true;
            }
            dots += 1;
            prev = Some('.');
            continue;
        }
        end_dots(&mut out, &mut dots);
        match ch {
            c if c.is_whitespace() => {
                respace = false;
                if !out.is_empty() && !out.ends_with(' ') {
                    out.push(' ');
                }
            }
            ',' | ';' | ':' | '!' | '?' => {
                if out.ends_with(' ') {
                    out.pop();
                    respace = true;
                }
                if prev != Some(ch) {
                    out.push(ch);
                }
            }
            c => {
                if respace && c.is_alphanumeric() {
                    out.push(' ');
                }
                respace = false;
                out.push(c);
            }
        }
        prev = Some(ch);
    }
    end_dots(&mut out, &mut dots);
    out.trim().to_owned()
}

/// A run of full stops: one or two become one, more become an ellipsis.
fn end_dots(out: &mut String, dots: &mut usize) {
    match *dots {
        0 => {}
        1 | 2 => out.push('.'),
        _ => out.push('…'),
    }
    *dots = 0;
}

/// Sentences are cut at `.`, `?` or `!` followed by a space or the end. A full stop after an
/// abbreviation, an initial, or before a lower-case word does not end one; decimals and
/// versions have no space after their dots.
fn split_sentences(text: &str) -> Vec<String> {
    let c: Vec<char> = text.chars().collect();
    let mut out: Vec<String> = vec![];
    let mut start = 0;
    let mut i = 0;
    while i < c.len() {
        if matches!(c[i], '.' | '?' | '!') {
            let mut j = i;
            while j + 1 < c.len() && matches!(c[j + 1], '.' | '?' | '!') {
                j += 1;
            }
            let mut k = j;
            while k + 1 < c.len() && matches!(c[k + 1], '"' | '\'' | ')' | ']' | '”' | '’') {
                k += 1;
            }
            let ends = k + 1 >= c.len() || c[k + 1].is_whitespace();
            if ends {
                let boundary = if c[i] == '.' && j == i {
                    dot_ends_sentence(&c, i, k)
                } else {
                    true
                };
                if boundary {
                    push_sentence(&mut out, &c[start..=k]);
                    start = k + 1;
                }
            }
            i = k + 1;
            continue;
        }
        i += 1;
    }
    if start < c.len() {
        push_sentence(&mut out, &c[start..]);
    }
    out
}

fn push_sentence(out: &mut Vec<String>, chars: &[char]) {
    let s: String = chars.iter().collect();
    let s = s.trim();
    if s.chars().any(|c| c.is_alphanumeric()) {
        out.push(s.to_owned());
    }
}

const ABBREVIATIONS: &[&str] = &[
    "e.g.", "i.e.", "mr.", "mrs.", "ms.", "dr.", "prof.", "vs.", "etc.", "approx.", "st.", "inc.",
    "ltd.", "cf.", "al.", "fig.",
];

/// Whether the single full stop at `dot` (the sentence's mark ends at `end`) ends a sentence.
fn dot_ends_sentence(c: &[char], dot: usize, end: usize) -> bool {
    let from = (0..=dot)
        .rev()
        .find(|&j| c[j].is_whitespace())
        .map(|j| j + 1)
        .unwrap_or(0);
    let word: String = c[from..=dot]
        .iter()
        .collect::<String>()
        .trim_start_matches(['(', '"', '\'', '“'])
        .to_lowercase();
    let next = c[end + 1..].iter().find(|x| !x.is_whitespace());
    if ABBREVIATIONS.contains(&word.as_str()) {
        // "etc." can close a sentence when a new one clearly starts.
        return word == "etc." && next.is_some_and(|n| n.is_uppercase());
    }
    let initial = word.chars().count() == 2 && c[from].is_uppercase();
    if initial {
        return false;
    }
    !next.is_some_and(|n| n.is_lowercase())
}

/// A sentence over [`MAX_SENTENCE_CHARS`] is cut at its last comma or space within the limit.
fn split_long(s: &str) -> Vec<String> {
    let mut out = vec![];
    let mut rest: Vec<char> = s.chars().collect();
    while rest.len() > MAX_SENTENCE_CHARS {
        let window = &rest[..MAX_SENTENCE_CHARS];
        let cut = window
            .iter()
            .rposition(|c| *c == ',')
            .filter(|&p| p > 0)
            .map(|p| p + 1)
            .or_else(|| window.iter().rposition(|c| *c == ' ').filter(|&p| p > 0))
            .unwrap_or(MAX_SENTENCE_CHARS);
        let head: String = rest[..cut].iter().collect();
        out.push(head.trim().to_owned());
        rest = rest[cut..]
            .iter()
            .skip_while(|c| c.is_whitespace())
            .copied()
            .collect();
    }
    let tail: String = rest.iter().collect();
    if !tail.trim().is_empty() {
        out.push(tail.trim().to_owned());
    }
    out
}

fn word_count(s: &str) -> usize {
    s.split_whitespace().count()
}

/// Cut the answer to about [`MAX_WORDS`] words and [`MAX_SENTENCES`] sentences, ending with the
/// offer of the rest. Whether it was cut.
fn cap(sentences: &mut Vec<String>) -> bool {
    let total: usize = sentences.iter().map(|s| word_count(s)).sum();
    let mut cut = false;
    if total > MAX_WORDS {
        cut = true;
        let mut kept: Vec<String> = vec![];
        let mut run = 0;
        for s in sentences.iter() {
            let w = word_count(s);
            if run + w > KEEP_WORDS {
                break;
            }
            run += w;
            kept.push(s.clone());
        }
        if kept.is_empty() {
            kept.push(cut_words(&sentences[0]));
        }
        *sentences = kept;
    }
    if sentences.len() > MAX_SENTENCES - OFFER.len() && (cut || sentences.len() > MAX_SENTENCES) {
        sentences.truncate(MAX_SENTENCES - OFFER.len());
        cut = true;
    }
    if cut {
        sentences.extend(OFFER.iter().map(|s| (*s).to_owned()));
    }
    cut
}

/// The first [`KEEP_WORDS`] words of a sentence that is too long on its own, ended at the last
/// comma among them when there is one.
fn cut_words(s: &str) -> String {
    let words: Vec<&str> = s.split_whitespace().take(KEEP_WORDS).collect();
    let end = words
        .iter()
        .rposition(|w| w.ends_with(','))
        .map(|p| p + 1)
        .unwrap_or(words.len());
    let text = words[..end].join(" ");
    let text = text.trim_end_matches([',', ';', ':']);
    format!("{text}.")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn say(text: &str) -> Vec<String> {
        speakable(text).sentences
    }

    #[test]
    fn each_pass_cleans_what_it_should() {
        let cases: &[(&str, &[&str])] = &[
            // Plain text and the split.
            (
                "It works. Want the details?",
                &["It works.", "Want the details?"],
            ),
            ("No end punctuation", &["No end punctuation."]),
            // Code, tables and what stands in for them.
            (
                "Here you go.\n```rust\nfn main() {}\n```\nThat is it.",
                &["Here you go.", CODE_NOTE, "That is it."],
            ),
            (
                "Results:\n\n| a | b |\n|---|---|\n| 1 | 2 |\n\nDone.",
                &["Results.", TABLE_NOTE, "Done."],
            ),
            (
                "Look.\n\n    let x = 1;\n    let y = 2;\n\nOk.",
                &["Look.", CODE_NOTE, "Ok."],
            ),
            // Headings, emphasis, quotes, rules, lists.
            (
                "# Title\nSome **bold** and _soft_ words",
                &["Title.", "Some bold and soft words."],
            ),
            ("> quoted line\n\n---\n\nafter", &["quoted line.", "after."]),
            (
                "Steps:\n1. Open it\n2. Press go;\n- done",
                &["Steps.", "Open it.", "Press go.", "done."],
            ),
            // Links, addresses, emails.
            (
                "See [the docs](https://example.com/a) now.",
                &["See the docs now."],
            ),
            (
                "Open https://example.com/a?b=1, then www.example.org.",
                &["Open a link, then a link."],
            ),
            (
                "Mail me at a@b.com please.",
                &["Mail me at a@b.com please."],
            ),
            // Paths and file names.
            (
                r"It is in D:\Docs\notes.txt now.",
                &["It is in a file now."],
            ),
            (
                "Edit src/ui/panel.tsx and ./run.sh.",
                &["Edit a file and a file."],
            ),
            (
                "Look at the README.md and main.rs.",
                &["Look at the README and a file."],
            ),
            ("Look at your Cargo.toml.", &["Look at your file."]),
            (
                "Built with Node.js, and/or Vue.js, 10/11/2026, he/she.",
                &["Built with Node.js, and/or Vue.js, 10/11/2026, he/she."],
            ),
            // Ids.
            ("Fixed in abc1234, see (0f3a9c2d).", &["Fixed in, see."]),
            ("Merged PR #1234 and PR 77.", &["Merged the PR and the PR."]),
            ("Merged the PR #1234.", &["Merged the PR."]),
            (
                "Run 7f3c2a10-1b2c-4d5e-8f90-a1b2c3d4e5f6 now.",
                &["Run now."],
            ),
            ("Token Zk3Qv9Lm2Xp7Rt4Ws1 is set.", &["Token is set."]),
            (
                "Step 12 of 3.5 in 0.2.0 took 1234567 ms, a deadbeef.",
                &["Step 12 of 3.5 in 0.2.0 took 1234567 ms, a deadbeef."],
            ),
            // Splitting rules.
            (
                "Use e.g. a cat vs. a dog. Then Mr. Smith left.",
                &["Use e.g. a cat vs. a dog.", "Then Mr. Smith left."],
            ),
            (
                "Version 0.2.0 is out. Pi is 3.14. J. Smith agreed",
                &["Version 0.2.0 is out.", "Pi is 3.14.", "J. Smith agreed."],
            ),
            ("Wait... what?! Really", &["Wait… what?!", "Really."]),
            // Whitespace and repeated punctuation.
            (
                "Too   many   spaces ,really !!",
                &["Too many spaces, really!"],
            ),
            // Emoji stay.
            ("All good 🎉", &["All good 🎉"]),
            // Nothing to say.
            ("```\ncode\n```", &[CODE_NOTE]),
            ("", &[]),
        ];
        for (input, want) in cases {
            assert_eq!(say(input), *want, "{input:?}");
        }
    }

    #[test]
    fn counts_what_was_removed() {
        let r = speakable(
            "```\nx\n```\n| a |\n|---|\n[t](https://a.b) https://c.d src/a/b.rs abc1234 **b**",
        )
        .removed;
        assert_eq!(
            r,
            Removed {
                code: 1,
                tables: 1,
                links: 1,
                urls: 1,
                paths: 1,
                ids: 1,
                markup: 1
            }
        );
    }

    fn run(word: &str, n: usize) -> String {
        vec![word; n].join(" ")
    }

    #[test]
    fn the_cap_edges() {
        // Exactly at the cap is left alone; one more is cut at the last sentence that fits.
        for (n, cut) in [(149, false), (150, false), (151, true)] {
            let text = format!("{}. {}.", run("Ab", 70), run("Ab", n - 70));
            let s = speakable(&text);
            assert_eq!(s.truncated, cut, "{n} words");
            if cut {
                let offer: Vec<&str> = s.sentences[1..].iter().map(String::as_str).collect();
                assert_eq!(s.sentences.len(), 3, "{:?}", s.sentences);
                assert_eq!(offer, OFFER);
            } else {
                assert_eq!(s.sentences.len(), 2);
            }
        }
        // A sentence that alone is over the limit is cut at its last comma among 140 words.
        let long = format!("{}, {}.", run("a", 99), run("a", 99));
        let s = speakable(&long);
        assert!(s.truncated);
        assert_eq!(s.sentences.len(), 3);
        assert!(s.sentences[0].ends_with("a."));
        assert_eq!(s.sentences[0].split_whitespace().count(), 99);
        // No comma: cut at 140 words.
        let s = speakable(&format!("{}.", run("a", 198)));
        assert_eq!(s.sentences[0].split_whitespace().count(), 140);
        // A sentence over 400 characters is split at its last comma, or space.
        let wide = format!("{}, {}.", "a".repeat(300), run("b", 100));
        let parts = say(&wide);
        assert!(
            parts
                .iter()
                .all(|p| p.chars().count() <= MAX_SENTENCE_CHARS),
            "{parts:?}"
        );
        assert_eq!(parts[0], format!("{},", "a".repeat(300)));
        // Many short sentences are limited to 40, the last two being the offer.
        let s = speakable(&"Ok. ".repeat(60));
        assert!(s.truncated);
        assert_eq!(s.sentences.len(), MAX_SENTENCES);
    }
}
