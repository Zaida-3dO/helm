//! A check for text that looks like a credential, for anything that is about to be stored where
//! others read it or sent off this PC.
//!
//! It errs towards holding text back: a false hit costs one note or passage, a miss leaks a key.

/// Words that name a secret; a name that ends with one of them, followed by `=` or `:` and a
/// value, is a credential line (`api_key = …`, `export GITHUB_TOKEN=…`, `_authToken=…`).
const KEY_NAMES: &[&str] = &[
    "password",
    "passwd",
    "secret",
    "api_key",
    "apikey",
    "api-key",
    "token",
    "access_key",
    "private_key",
    "credentials",
];

/// Token shapes that carry a long run of letters and digits straight after them.
const PREFIXES: &[&str] = &[
    "ghp_",
    "gho_",
    "ghs_",
    "ghu_",
    "github_pat_",
    "xoxb-",
    "xoxp-",
    "xoxa-",
    "glpat-",
    "sk-ant-",
    "sk-proj-",
    "sk-",
    "akia",
    "asia",
    "aiza",
];

fn is_token_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | '+' | '/' | '=')
}

fn has_prefixed_token(lower: &str, p: &str) -> bool {
    lower.match_indices(p).any(|(i, _)| {
        // A prefix inside a longer word (`task-…`) is not a token.
        let before = lower[..i].chars().next_back();
        if before.is_some_and(|c| c.is_ascii_alphanumeric()) {
            return false;
        }
        lower[i + p.len()..]
            .chars()
            .take_while(|c| is_token_char(*c))
            .count()
            >= if p == "sk-" { 20 } else { 12 }
    })
}

/// `scheme://user:password@host`, the shape of a stored git or package credential.
fn has_url_login(lower: &str) -> bool {
    lower.match_indices("://").any(|(i, _)| {
        let rest = &lower[i + 3..];
        let end = rest
            .find(|c: char| c == '/' || c.is_whitespace())
            .unwrap_or(rest.len());
        let authority = &rest[..end];
        authority
            .split_once('@')
            .is_some_and(|(user, _)| user.split_once(':').is_some_and(|(_, pw)| pw.len() >= 3))
    })
}

fn is_name_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.')
}

/// A `name = value` or `name: value` for a password, key or token. At the start of a line a
/// value of five characters is enough; inside prose it must be one run of eight or more with no
/// spaces, so a sentence that merely mentions a password is let through.
fn is_key_line(line: &str) -> bool {
    const QUOTES: [char; 3] = ['"', '\'', '`'];
    line.match_indices(['=', ':']).any(|(i, _)| {
        let before = line[..i].trim_end_matches(QUOTES).trim_end();
        let name = before
            .rsplit(|c: char| !is_name_char(c))
            .next()
            .unwrap_or("");
        if name.is_empty() || !KEY_NAMES.iter().any(|k| name.ends_with(k)) {
            return false;
        }
        let mut head = before
            .trim_start_matches(['-', '*', '>', '{', ',', ' ', '\t'])
            .trim_start_matches(QUOTES);
        for p in ["export ", "set ", "$env:"] {
            head = head.strip_prefix(p).unwrap_or(head).trim_start();
        }
        let value = line[i + 1..].trim().trim_matches(QUOTES).trim_matches(',');
        let value = value.trim_matches(QUOTES);
        if head == name {
            value.len() >= 5
        } else {
            value.len() >= 8 && !value.contains(char::is_whitespace)
        }
    })
}

/// Whether the text looks like a credential: a private key block, a well-known token, a bearer
/// string, a login inside an address, or a `name = value` line for a password, key or token.
pub fn looks_like_secret(s: &str) -> bool {
    let lower = s.to_ascii_lowercase();
    if lower.contains("-----begin") && lower.contains("private key") {
        return true;
    }
    if PREFIXES.iter().any(|p| has_prefixed_token(&lower, p)) || has_url_login(&lower) {
        return true;
    }
    if lower.match_indices("bearer ").any(|(i, _)| {
        lower[i + 7..]
            .chars()
            .take_while(|c| is_token_char(*c))
            .count()
            >= 20
    }) {
        return true;
    }
    lower.lines().any(is_key_line)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn credential_shapes_are_caught_and_plain_prose_passes() {
        let run = |n: usize| "a1B2".repeat(n);
        let rows: Vec<(String, bool)> = vec![
            ("Run `npm test` before pushing.".into(), false),
            ("The token budget is tracked in usage.md".into(), false),
            (
                "Ask about the task-management-system-rollout plan".into(),
                false,
            ),
            ("const tokenizer = new Tokenizer();".into(), false),
            ("Remember the secret: it is in the vault".into(), false),
            (format!("curl -H token={}", run(3)), true),
            ("see https://example.com/docs/path".into(), false),
            (format!("ghp_{}", run(5)), true),
            (format!("my key is sk-ant-{} ok", run(4)), true),
            (format!("export GITHUB_TOKEN={}", run(3)), true),
            (format!("//registry.example/:_authToken={}", run(3)), true),
            (format!("  \"api_key\": \"{}\",", run(3)), true),
            ("password: hunter2hunter2".into(), true),
            ("api_key=abc123456".into(), true),
            (format!("Authorization: Bearer {}", run(6)), true),
            ("https://user:hunter22@host.example/repo.git".into(), true),
            (format!("-----{} RSA PRIVATE KEY-----", "BEGIN"), true),
            (format!("-----{} OPENSSH PRIVATE KEY-----", "BEGIN"), true),
        ];
        for (text, secret) in rows {
            assert_eq!(looks_like_secret(&text), secret, "{text}");
        }
    }
}
