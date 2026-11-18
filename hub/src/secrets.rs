//! A check for text that looks like a credential, for anything that is about to be stored where
//! others read it or sent off this PC.
//!
//! It errs towards holding text back: a false hit costs one note or passage, a miss leaks a key.

/// Words that name a secret; a name that ends with one of them, followed by `=` or `:` and a
/// value, is a credential line (`api_key = …`, `export GITHUB_TOKEN=…`, `_authToken=…`).
const KEY_NAMES: &[&str] = &[
    "password",
    "passwd",
    "_pass",
    "_pwd",
    "secret",
    "secret_key",
    "secret-key",
    "cookie",
    "sessionid",
    "session_id",
    "authorization",
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
    "sk_live_",
    "sk_test_",
    "rk_live_",
    "xoxe-",
    "xoxs-",
    "hf_",
    "npm_",
    "ya29.",
    "gocspx-",
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

/// A Telegram bot token: 8 to 10 digits, a colon, then a run of 30 or more letters, digits,
/// `_` and `-` (also inside `https://api.telegram.org/bot<token>/`).
fn has_bot_token(lower: &str) -> bool {
    let b = lower.as_bytes();
    b.iter().enumerate().any(|(i, c)| {
        if *c != b':' {
            return false;
        }
        let digits = b[..i]
            .iter()
            .rev()
            .take_while(|d| d.is_ascii_digit())
            .count();
        (8..=10).contains(&digits)
            && b[i + 1..]
                .iter()
                .take_while(|c| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'-'))
                .count()
                >= 30
    })
}

/// A JSON web token: `eyJ…` then `.eyJ…` (a header and a payload, both base64 of a JSON object).
fn has_jwt(lower: &str) -> bool {
    lower.match_indices("eyj").any(|(i, _)| {
        if lower[..i]
            .chars()
            .next_back()
            .is_some_and(|c| c.is_ascii_alphanumeric())
        {
            return false;
        }
        let rest = &lower[i..];
        let head = rest
            .find(|c: char| !(c.is_ascii_alphanumeric() || matches!(c, '_' | '-')))
            .unwrap_or(rest.len());
        head >= 10 && rest[head..].starts_with(".eyj")
    })
}

/// `Basic <base64>`, the HTTP login header: a run of 16 or more token characters with a digit or
/// a base64 sign in it (so a sentence that says "basic configuration" passes).
fn has_basic_login(lower: &str) -> bool {
    lower.match_indices("basic ").any(|(i, _)| {
        if lower[..i]
            .chars()
            .next_back()
            .is_some_and(|c| c.is_ascii_alphanumeric())
        {
            return false;
        }
        let run: String = lower[i + 6..]
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '/' | '='))
            .collect();
        run.len() >= 16
            && run
                .chars()
                .any(|c| c.is_ascii_digit() || matches!(c, '+' | '/' | '='))
    })
}

/// A netrc entry: `machine host login name password value` on one line.
fn is_netrc_line(line: &str) -> bool {
    line.contains("machine ")
        && line.contains("login ")
        && line.match_indices("password ").any(|(i, _)| {
            line[i + 9..]
                .split_whitespace()
                .next()
                .is_some_and(|v| v.len() >= 4)
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

/// Whether the text looks like a credential: a private key block (or the line that opens or
/// closes one, so a key cut into pieces is caught in each), a well-known token, a bearer string,
/// a login inside an address or a header, or a `name = value` line for a password, key or token.
pub fn looks_like_secret(s: &str) -> bool {
    let lower = s.to_ascii_lowercase();
    if (lower.contains("-----begin") || lower.contains("-----end")) && lower.contains("private key")
    {
        return true;
    }
    if PREFIXES.iter().any(|p| has_prefixed_token(&lower, p))
        || has_url_login(&lower)
        || has_bot_token(&lower)
        || has_jwt(&lower)
        || has_basic_login(&lower)
    {
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
    lower.lines().any(|l| is_key_line(l) || is_netrc_line(l))
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
            (format!("-----{} RSA PRIVATE KEY-----", "END"), true),
            (format!("-----{} CERTIFICATE-----", "BEGIN"), false),
            (format!("bot123456789:AA{}", run(9)), true),
            (
                format!("https://api.telegram.org/bot123456789:AA{}/getMe", run(9)),
                true,
            ),
            ("The ratio is 12:30:45 or 123456789 to 5".into(), false),
            (format!("SECRET_KEY={}", run(3)), true),
            (format!("STRIPE_SECRET_KEY: {}", run(3)), true),
            (format!("DB_PASS={}", run(2)), true),
            ("DB_PWD=hunter22".into(), true),
            (format!("sk_live_{}", run(5)), true),
            (format!("rk_live_{}", run(5)), true),
            (format!("hf_{}", run(5)), true),
            (format!("npm_{}", run(5)), true),
            (format!("ya29.{}", run(5)), true),
            (format!("GOCSPX-{}", run(5)), true),
            (format!("xoxe-{}", run(5)), true),
            (format!("eyJ{}.eyJ{}.{}", run(4), run(4), run(6)), true),
            (format!("Authorization: Basic {}", run(6)), true),
            (
                "The basic configuration of the router is done".into(),
                false,
            ),
            (format!("Cookie: session={}", run(4)), true),
            (
                "machine example.org login pat password hunter22".into(),
                true,
            ),
            ("Reset the password manager on Monday".into(), false),
            ("the login page asks for your password first".into(), false),
            ("The session: planning for the quarter".into(), false),
        ];
        for (text, secret) in rows {
            assert_eq!(looks_like_secret(&text), secret, "{text}");
        }
    }
}
