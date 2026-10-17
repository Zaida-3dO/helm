//! Ranking and snippets (chats section 15 item 3).

const WEEK_MS: f64 = 7.0 * 24.0 * 60.0 * 60.0 * 1000.0;

/// Text score = relevance / (1 + 0.15 * ln(1 + age / 7 days)), with age measured from the chat's
/// last update. `relevance` is SQLite's bm25 negated (bm25 is lower-is-better and negative), so a
/// bigger score is better. An update time in the future counts as age zero.
pub fn score(relevance: f64, updated_at_ms: i64, now_ms: i64) -> f64 {
    let age = (now_ms - updated_at_ms).max(0) as f64;
    relevance / (1.0 + 0.15 * (1.0 + age / WEEK_MS).ln())
}

const BEFORE: usize = 60;
const AFTER: usize = 100;
const ELLIPSIS: char = '\u{2026}';

/// About 160 characters of `text` around the first occurrence of any of `words` (a prefix match,
/// case-insensitive), whitespace collapsed, with an ellipsis where it was cut. With no occurrence
/// (the match was on a token the plain scan misses) it is the start of the text.
pub fn snippet(text: &str, words: &[String]) -> String {
    let flat: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let chars: Vec<char> = flat.chars().collect();
    let lower: Vec<char> = flat.to_lowercase().chars().collect();
    // Lower-casing can change the length for a few scripts; fall back to the start then.
    let at = if lower.len() == chars.len() {
        words
            .iter()
            .filter_map(|w| find(&lower, &w.to_lowercase().chars().collect::<Vec<_>>()))
            .min()
    } else {
        None
    }
    .unwrap_or(0);
    let start = at.saturating_sub(BEFORE);
    let end = (at + AFTER).min(chars.len());
    let mut out: String = chars[start..end].iter().collect();
    if start > 0 {
        out.insert(0, ELLIPSIS);
    }
    if end < chars.len() {
        out.push(ELLIPSIS);
    }
    out
}

fn find(hay: &[char], needle: &[char]) -> Option<usize> {
    if needle.is_empty() || needle.len() > hay.len() {
        return None;
    }
    (0..=hay.len() - needle.len()).find(|&i| hay[i..i + needle.len()] == *needle)
}

#[cfg(test)]
mod tests {
    use super::*;

    const DAY: i64 = 24 * 60 * 60 * 1000;

    #[test]
    fn an_older_chat_scores_lower_but_only_gently() {
        let now = 1_000 * DAY;
        let fresh = score(10.0, now, now);
        let week = score(10.0, now - 7 * DAY, now);
        let year = score(10.0, now - 365 * DAY, now);
        assert_eq!(fresh, 10.0);
        assert!(week < fresh && year < week);
        assert!(year > 5.0, "a year old keeps over half its score: {year}");
    }

    #[test]
    fn the_formula_is_the_specs() {
        let now = 100 * DAY;
        let s = score(8.0, now - 7 * DAY, now);
        let expected = 8.0 / (1.0 + 0.15 * 2.0_f64.ln());
        assert!((s - expected).abs() < 1e-9);
    }

    #[test]
    fn a_future_update_time_is_age_zero() {
        assert_eq!(score(3.0, 10_000, 1_000), 3.0);
    }

    #[test]
    fn the_snippet_centres_on_the_first_matching_word() {
        let text = format!(
            "{} needle in the middle {}",
            "alpha ".repeat(40),
            "omega ".repeat(40)
        );
        let s = snippet(&text, &["NEEDLE".into()]);
        assert!(s.contains("needle in the middle"), "{s}");
        assert!(s.starts_with(ELLIPSIS) && s.ends_with(ELLIPSIS), "{s}");
        assert!(s.chars().count() <= 162);
    }

    #[test]
    fn a_prefix_matches_and_short_text_is_whole() {
        assert_eq!(
            snippet("fix the Login page", &["log".into()]),
            "fix the Login page"
        );
        assert_eq!(snippet("a\n\n  b", &["zzz".into()]), "a b");
    }
}
