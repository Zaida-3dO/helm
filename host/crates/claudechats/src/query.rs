//! Turning what the user typed into an FTS5 expression. This follows the same rules the chat
//! search uses: at most twelve words, each a quoted prefix term, all of them required.

/// Words beyond this many are ignored.
pub const MAX_WORDS: usize = 12;

/// The query's usable words: whitespace-separated, at most [`MAX_WORDS`], and each containing at
/// least one letter or digit (a lone "-" or "&" has no token to match and would only confuse FTS5).
pub fn words(query: &str) -> Vec<String> {
    query
        .split_whitespace()
        .take(MAX_WORDS)
        .filter(|w| w.chars().any(char::is_alphanumeric))
        .map(str::to_owned)
        .collect()
}

/// `"one"* AND "two"*`: each word a quoted prefix term, with embedded quotes doubled. `None` when
/// there is nothing to search for.
pub fn fts_expression(words: &[String]) -> Option<String> {
    if words.is_empty() {
        return None;
    }
    Some(
        words
            .iter()
            .map(|w| format!("\"{}\"*", w.replace('"', "\"\"")))
            .collect::<Vec<_>>()
            .join(" AND "),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn words_are_capped_and_filtered() {
        let q = (1..=15)
            .map(|i| format!("w{i}"))
            .collect::<Vec<_>>()
            .join(" ");
        assert_eq!(words(&q).len(), 12, "only the first 12 words count");
        assert_eq!(words("  fix   login  "), ["fix", "login"]);
        assert_eq!(words("- & ..."), Vec::<String>::new());
    }

    #[test]
    fn terms_are_quoted_prefixes_anded_and_cannot_escape() {
        assert_eq!(
            fts_expression(&words("fix log")).unwrap(),
            "\"fix\"* AND \"log\"*"
        );
        assert_eq!(fts_expression(&[]), None);
        assert_eq!(
            fts_expression(&words("a\"b NEAR OR*")).unwrap(),
            "\"a\"\"b\"* AND \"NEAR\"* AND \"OR*\"*"
        );
    }
}
