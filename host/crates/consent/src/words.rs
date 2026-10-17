//! The user's-words check for one message.
//!
//! Pure text rules; [`crate::store`] decides which messages are eligible (typed by the user, in
//! the chat, within the episode, not yet spent). A message passes when it is one short line with
//! no question, quote, code fence, newline or doubt-shaped word, exactly one of its segments is
//! the step's fixed form, and every other segment is a plain acknowledgement. Refs, paths, URLs,
//! `#N`, shas and backticked spans are read as opaque tokens first, so none of them can supply or
//! hide a word.

/// Longest message that can count, in bytes.
pub const MAX_MESSAGE_BYTES: usize = 200;
/// The matched sentence's length bounds, in characters.
pub const QUOTE_CHARS: std::ops::RangeInclusive<usize> = 8..=500;

const QUESTION_MARKS: &[char] = &['?', '？', '¿', '؟', '⸮', '\u{037E}', '՞', '፧', '꘏'];
const QUOTE_MARKERS: &[char] = &['>', '»', '|', '›', '＞'];

/// Words that make a message something other than a plain go-ahead: negations and take-backs,
/// conditions and time, the future, doubt, and reported speech.
const FORBIDDEN: &[&str] = &[
    "not",
    "no",
    "never",
    "nor",
    "cannot",
    "dont",
    "don't",
    "abort",
    "halt",
    "stop",
    "cancel",
    "undo",
    "revert",
    "wait",
    "hold",
    "hang",
    "if",
    "unless",
    "when",
    "whenever",
    "once",
    "after",
    "before",
    "until",
    "assuming",
    "provided",
    "later",
    "tomorrow",
    "soon",
    "tonight",
    "next",
    "will",
    "shall",
    "gonna",
    "eventually",
    "maybe",
    "perhaps",
    "probably",
    "possibly",
    "might",
    "could",
    "would",
    "should",
    "guess",
    "think",
    "unsure",
    "said",
    "says",
    "say",
    "told",
    "tell",
    "asked",
    "wants",
    "wanted",
];

/// Segments that may sit beside the fixed form.
const ACKS: &[&str] = &[
    "yes",
    "yep",
    "yeah",
    "ok",
    "okay",
    "sure",
    "please",
    "thanks",
    "thank you",
    "go ahead",
    "do it",
    "approved",
    "fine",
    "great",
    "cool",
    "sounds good",
    "alright",
    "all right",
    "confirmed",
    "go",
];

const TOKEN_OPEN: char = '\u{E000}';
const TOKEN_CLOSE: char = '\u{E001}';

fn is_token_word(w: &str) -> bool {
    let lower = w.to_ascii_lowercase();
    lower.starts_with("http://")
        || lower.starts_with("https://")
        || w.contains('/')
        || w.contains('\\')
        // File names and dotted refs (`.mcp.json`, `v1.2`): an inner dot is not a sentence end.
        || (w.contains('.') && w.chars().any(|c| c.is_alphanumeric()))
        || (w.len() > 1 && w.starts_with('#') && w[1..].chars().all(|c| c.is_ascii_digit()))
        || ((7..=40).contains(&w.len()) && w.chars().all(|c| c.is_ascii_hexdigit()) && w.chars().any(|c| c.is_ascii_digit()))
}

/// Replace backticked spans and token-shaped words with placeholders; returns the scan text and
/// the token values in order.
fn tokenize(text: &str) -> (String, Vec<String>) {
    let mut tokens = Vec::new();
    let mut out = String::new();
    // Backticked spans first.
    let mut rest = text;
    let mut plain = String::new();
    while let Some(start) = rest.find('`') {
        let after = &rest[start + 1..];
        match after.find('`') {
            Some(end) => {
                plain.push_str(&rest[..start]);
                plain.push(TOKEN_OPEN);
                plain.push_str(&tokens.len().to_string());
                plain.push(TOKEN_CLOSE);
                tokens.push(after[..end].to_owned());
                rest = &after[end + 1..];
            }
            None => break,
        }
    }
    plain.push_str(rest);
    // Then token-shaped words (trailing sentence punctuation stays outside the token).
    for (i, word) in plain.split(' ').enumerate() {
        if i > 0 {
            out.push(' ');
        }
        let core = word.trim_end_matches(['.', ',', '!', ';']);
        if !core.contains(TOKEN_OPEN) && is_token_word(core) {
            out.push(TOKEN_OPEN);
            out.push_str(&tokens.len().to_string());
            out.push(TOKEN_CLOSE);
            out.push_str(&word[core.len()..]);
            tokens.push(core.to_owned());
        } else {
            out.push_str(word);
        }
    }
    (out, tokens)
}

fn restore(seg: &str, tokens: &[String]) -> String {
    let mut out = String::new();
    let mut it = seg.split(TOKEN_OPEN);
    out.push_str(it.next().unwrap_or(""));
    for part in it {
        if let Some((n, tail)) = part.split_once(TOKEN_CLOSE) {
            if let Some(t) = n.parse::<usize>().ok().and_then(|i| tokens.get(i)) {
                out.push_str(t);
            }
            out.push_str(tail);
        }
    }
    out
}

fn normalise(s: &str) -> String {
    s.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .trim_end_matches(['.', '!'])
        .to_lowercase()
}

fn words_of(seg: &str) -> impl Iterator<Item = String> + '_ {
    seg.split(|c: char| !(c.is_alphanumeric() || c == '\''))
        .filter(|w| !w.is_empty())
        .map(|w| w.to_lowercase())
}

/// Check one message against the step's fixed form. `Ok` carries the matched sentence; `Err` says
/// why the message does not count.
pub fn check(message: &str, fixed_form: &str) -> Result<String, &'static str> {
    if message.len() > MAX_MESSAGE_BYTES {
        return Err("the message is longer than 200 bytes");
    }
    if message.contains(['\n', '\r']) {
        return Err("the message is more than one line");
    }
    if message.contains("```") {
        return Err("the message has a code fence");
    }
    if message.trim_start().starts_with(QUOTE_MARKERS) {
        return Err("the message is a quote");
    }
    // Checked on the raw text: a question mark anywhere refuses, even inside a token.
    if message.contains(QUESTION_MARKS) {
        return Err("the message asks a question");
    }
    let (scan, tokens) = tokenize(message);
    let want = normalise(fixed_form);
    let mut matched: Option<String> = None;
    for seg in scan.split(['.', '!', ',', '。']) {
        let seg = seg.trim();
        if seg.is_empty() {
            continue;
        }
        let restored = restore(seg, &tokens);
        if normalise(&restored) == want {
            if matched.is_some() {
                return Err("the message names the step more than once");
            }
            matched = Some(restored.trim().to_owned());
            continue;
        }
        let words: Vec<String> = words_of(seg).collect();
        if words
            .iter()
            .any(|w| FORBIDDEN.contains(&w.as_str()) || w.ends_with("n't"))
        {
            return Err("the message has a negation, condition, doubt or reported speech");
        }
        if !ACKS.contains(&normalise(seg).as_str()) {
            return Err("the message says more than the step and an acknowledgement");
        }
    }
    let m = matched.ok_or("the message does not contain the step's exact sentence")?;
    if !QUOTE_CHARS.contains(&m.chars().count()) {
        return Err("the matched sentence is not 8-500 characters");
    }
    Ok(m)
}

#[cfg(test)]
mod tests {
    use super::check;

    const FORM: &str = "Approve the MCP server notes from .mcp.json.";

    #[test]
    fn the_fixed_form_alone_or_with_acks_counts() {
        assert!(check("Approve the MCP server notes from .mcp.json.", FORM).is_ok());
        assert!(check("approve the  MCP server notes from .mcp.json", FORM).is_ok());
        assert!(check(
            "Yes, approve the MCP server notes from .mcp.json. Thanks!",
            FORM
        )
        .is_ok());
        assert!(check("ok. Approve the MCP server notes from .mcp.json", FORM).is_ok());
    }

    #[test]
    fn anything_else_does_not() {
        let cases = [
            ("Don't approve the MCP server notes from .mcp.json.", "negation"),
            ("Approve the MCP server notes from .mcp.json?", "question"),
            ("Approve the MCP server notes from .mcp.json？", "question"),
            ("> Approve the MCP server notes from .mcp.json.", "quote"),
            ("Approve the MCP server notes from .mcp.json.\nok", "one line"),
            ("```Approve the MCP server notes from .mcp.json.```", "code fence"),
            ("Approve the MCP server notes from .mcp.json. Not yet.", "negation"),
            ("Approve the MCP server notes from .mcp.json. Wait.", "negation"),
            ("Approve the MCP server notes from .mcp.json later", "negation"),
            ("Approve the MCP server notes from .mcp.json. And delete it.", "more than"),
            ("He said approve the MCP server notes from .mcp.json.", "reported speech"),
            ("Approve the MCP server other from .mcp.json.", "more than"),
            ("Approve the MCP server notes from .mcp.json. Approve the MCP server notes from .mcp.json.", "more than once"),
            ("Ok thanks", "more than"),
        ];
        for (msg, why) in cases {
            let e = check(msg, FORM).expect_err(msg);
            assert!(e.contains(why), "{msg}: {e}");
        }
        let long = format!(
            "Approve the MCP server notes from .mcp.json. {}",
            "ok. ".repeat(50)
        );
        assert!(check(&long, FORM).unwrap_err().contains("200 bytes"));
    }

    #[test]
    fn tokens_cannot_split_or_supply_words() {
        let form = "Approve the MCP server notes from C:/x/no.mcp.json.";
        // The '.' and the word "no" inside the path are part of a token, not a sentence break.
        assert!(check("Approve the MCP server notes from C:/x/no.mcp.json.", form).is_ok());
        // A backticked span cannot hide an extra instruction from the segment check: it is a
        // token inside a segment that is not an acknowledgement.
        assert!(check(
            "Approve the MCP server notes from C:/x/no.mcp.json. `yes`",
            form
        )
        .is_err());
    }

    #[test]
    fn short_sentences_do_not_count() {
        assert!(check("Go x.", "Go x.").unwrap_err().contains("8-500"));
    }
}
