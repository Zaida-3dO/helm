//! Characters that change how text reads without showing: direction overrides and isolates,
//! zero-width and invisible formatting, and filler characters. A label or shortcut an agent sets
//! must not carry them, since they let one string read as another.
//!
//! This is the one definition; callers that screen text for display names, shortcuts and labels
//! use it so the list stays the same everywhere.

/// True for a character that is invisible or reorders the text around it.
pub fn is_hidden_char(c: char) -> bool {
    matches!(
        c as u32,
        0x00AD // soft hyphen
        | 0x061C // Arabic letter mark
        | 0x115F | 0x1160 // Hangul fillers
        | 0x180E // Mongolian vowel separator
        | 0x200B..=0x200F // zero-width space, joiners, direction marks
        | 0x2028..=0x202E // line and paragraph separators, embeddings and overrides
        | 0x2060..=0x206F // word joiner, invisible operators, isolates, deprecated formatting
        | 0x3164 | 0xFFA0 // Hangul fillers
        | 0xFEFF // zero-width no-break space
        | 0xFFF9..=0xFFFB // interlinear annotation
        | 0xE0000..=0xE007F // tag characters
    )
}

/// True when `s` has a control character or a hidden one.
pub fn has_hidden(s: &str) -> bool {
    s.chars().any(|c| c.is_control() || is_hidden_char(c))
}

/// Where a JSON value holds an invisible or direction-changing character, as a path like `a.b[2]`,
/// or `None` when it is clean. Object keys are screened too: a key is a label as much as a value
/// is. Line breaks and tabs pass, since a setting may hold a multi-line prompt.
pub fn find_hidden(v: &serde_json::Value) -> Option<String> {
    fn walk(v: &serde_json::Value, at: &mut String) -> bool {
        match v {
            serde_json::Value::String(s) => s.chars().any(is_hidden_char),
            serde_json::Value::Array(a) => a.iter().enumerate().any(|(i, x)| {
                let n = at.len();
                at.push_str(&format!("[{i}]"));
                let hit = walk(x, at);
                if !hit {
                    at.truncate(n);
                }
                hit
            }),
            serde_json::Value::Object(m) => m.iter().any(|(k, x)| {
                let n = at.len();
                if !at.is_empty() {
                    at.push('.');
                }
                at.extend(k.chars().filter(|c| !c.is_control() && !is_hidden_char(*c)));
                if k.chars().any(is_hidden_char) || walk(x, at) {
                    return true;
                }
                at.truncate(n);
                false
            }),
            _ => false,
        }
    }
    let mut at = String::new();
    walk(v, &mut at).then_some(at)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn hidden_characters_are_found_in_text_and_in_json() {
        for bad in [
            "a\u{202e}b",
            "a\u{200b}b",
            "a\u{2066}b\u{2069}",
            "a\u{feff}",
            "a\u{ad}b",
            "tab\there",
            "a\u{e0041}b",
        ] {
            assert!(has_hidden(bad), "{bad:?}");
        }
        for ok in ["Ctrl+K", "Shortcut: ñ ü 日本語", "plain label", "🙂"] {
            assert!(!has_hidden(ok), "{ok:?}");
        }
        assert_eq!(find_hidden(&json!({"a": ["x", {"b": "y"}], "n": 3})), None);
        assert_eq!(
            find_hidden(&json!({"prompt": "line one\nline two\t"})),
            None
        );
        assert_eq!(
            find_hidden(&json!({"a": ["x", {"b": "y\u{200f}"}]})).as_deref(),
            Some("a[1].b")
        );
        assert_eq!(
            find_hidden(&json!({"id\u{202e}": "ok"})).as_deref(),
            Some("id")
        );
    }
}
