//! Which person a search is about.
//!
//! The household shares one memory, so who asks never decides what is found; it decides the order
//! when two people each have a record of the same kind (a passport, a flight, a doctor). The words
//! of the query say whose record is wanted:
//!
//! - a named person ("Ada's flight", "Ben passport") is that person, whoever asks;
//! - first-person words (my, mine, me, I, I'm, myself) are the asker;
//! - a query that names two people, or neither and uses no first-person word, is about nobody in
//!   particular and ranks as it always did.
//!
//! A name counts when it is capitalised or possessive, so a person called Will is not found in
//! "will it rain". The result is a [`Focus`]: ranking then prefers records that person owns or
//! that mention them by name (see `rank.rs`), ahead of the generic preference for the asker's own.

/// The person a query is about, and the name to look for in text that mentions them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Focus {
    pub person: i64,
    /// The name as written, lower case (a person's first name, or their whole name).
    pub names: Vec<String>,
}

/// Words that mean the asker.
const FIRST_PERSON: &[&str] = &[
    "my", "mine", "me", "i", "i'm", "im", "i've", "ive", "i'd", "i'll", "myself",
];

fn words(text: &str) -> Vec<(String, bool, bool)> {
    // (lower-cased word, capitalised in the query, possessive)
    let mut out = Vec::new();
    for raw in text.split(|c: char| c.is_whitespace() || ",.;:!?()[]\"".contains(c)) {
        let raw = raw.trim_matches(|c: char| c == '\'' || c == '’');
        if raw.is_empty() {
            continue;
        }
        let normalised = raw.replace('’', "'");
        let possessive = normalised.ends_with("'s");
        let stem = normalised.strip_suffix("'s").unwrap_or(&normalised);
        let capital = stem.chars().next().is_some_and(char::is_uppercase);
        out.push((stem.to_lowercase(), capital, possessive));
    }
    out
}

/// The names a person goes by in text: the first word of their name and the whole of it.
pub fn names_of(name: &str) -> Vec<String> {
    let whole = name.trim().to_lowercase();
    let mut names = Vec::new();
    if let Some(first) = whole.split_whitespace().next().filter(|f| f.len() >= 2) {
        names.push(first.to_owned());
    }
    if whole.contains(' ') {
        names.push(whole);
    }
    names
}

/// Words after which a capitalised first word is only the start of a sentence ("Will the backup
/// run"), not a name.
const SENTENCE_STARTERS: &[&str] = &[
    "the", "a", "an", "it", "they", "we", "you", "he", "she", "i", "this", "that", "there", "my",
    "our", "your", "their", "these", "those", "any", "all", "not", "be", "have", "still",
];

/// The placeholder name a hub gives whoever owned things before people existed; it is not a name
/// anyone writes in a question ("the car owner's manual").
fn is_placeholder(name: &str) -> bool {
    name.trim().eq_ignore_ascii_case("owner")
}

/// Whose record the query wants, if its words say.
pub fn resolve(query: &str, viewer: i64, people: &[(i64, String)]) -> Option<Focus> {
    let ws = words(query);
    let mut named: Vec<i64> = Vec::new();
    for (i, (word, capital, possessive)) in ws.iter().enumerate() {
        if !(*capital || *possessive) {
            continue;
        }
        // A capital on the first word of a question says little: "Will the backup run".
        let starts_sentence = i == 0
            && !*possessive
            && ws
                .get(1)
                .is_some_and(|(next, _, _)| SENTENCE_STARTERS.contains(&next.as_str()));
        if starts_sentence {
            continue;
        }
        for (id, name) in people {
            if !is_placeholder(name)
                && names_of(name).iter().any(|n| n == word)
                && !named.contains(id)
            {
                named.push(*id);
            }
        }
    }
    let person = match named.as_slice() {
        [one] => *one,
        [] if ws
            .iter()
            .any(|(w, _, _)| FIRST_PERSON.contains(&w.as_str())) =>
        {
            viewer
        }
        _ => return None,
    };
    let names = people
        .iter()
        .find(|(id, _)| *id == person)
        .map(|(_, n)| names_of(n))
        .filter(|n| !n.is_empty())?;
    Some(Focus { person, names })
}

/// Whether `text` mentions one of `names` as a word.
pub fn mentions(text: &str, names: &[String]) -> bool {
    let lower = text.to_lowercase();
    let single = lower
        .split(|c: char| !(c.is_alphanumeric() || c == '\''))
        .any(|w| {
            let w = w.strip_suffix("'s").unwrap_or(w);
            names.iter().any(|n| n == w)
        });
    // A whole name of several words ("ada lovell") is never one word of the text.
    single || {
        let flat = lower.split_whitespace().collect::<Vec<_>>().join(" ");
        names.iter().filter(|n| n.contains(' ')).any(|n| {
            flat.match_indices(n.as_str()).any(|(i, _)| {
                let before = flat[..i].chars().next_back();
                let after = flat[i + n.len()..].chars().next();
                !before.is_some_and(char::is_alphanumeric)
                    && !after.is_some_and(char::is_alphanumeric)
            })
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_query_is_about_the_person_it_names_else_the_asker_when_it_says_my() {
        let people = vec![
            (1, "Ada Lovell".to_owned()),
            (2, "Ben".to_owned()),
            (3, "Will".to_owned()),
            (4, "owner".to_owned()),
        ];
        // (query, viewer, who it is about)
        let rows: [(&str, i64, Option<i64>); 20] = [
            ("what's my passport details", 1, Some(1)),
            ("what's my passport details", 2, Some(2)),
            ("what is Ben's passport details", 1, Some(2)),
            ("what is ben’s passport details", 1, Some(2)),
            ("Ada's flight", 2, Some(1)),
            ("Ada Lovell flight", 2, Some(1)),
            ("ada's flight", 2, Some(1)),
            ("Ben appointment", 1, Some(2)),
            // Lower case and not possessive: not a name.
            ("ben appointment", 1, None),
            ("will it rain on the coast", 1, None),
            ("Will's keys", 1, Some(3)),
            // A name beats "my": the record of someone I am asking about.
            ("my sister Ben's doctor", 1, Some(2)),
            // Two people, or nobody, is no focus.
            ("Ada and Ben dinner plans", 3, None),
            ("bin collection days", 1, None),
            ("how do I reset the router", 2, Some(2)),
            ("tell me about mine", 1, Some(1)),
            // A capital that only starts the sentence is not a name, nor is a placeholder owner.
            ("Will the backup run", 1, None),
            ("Will Smith concert tickets", 1, Some(3)),
            ("the car owner's manual", 1, None),
            // A word that merely contains a first-person word is not one.
            ("mystery novel", 1, None),
        ];
        for (query, viewer, want) in rows {
            assert_eq!(
                resolve(query, viewer, &people).map(|f| f.person),
                want,
                "{query} asked by {viewer}"
            );
        }
        let names = names_of("Ada Lovell");
        assert!(mentions("Ada's flight is on Friday", &names));
        assert!(mentions("booked for ada lovell", &names));
        assert!(!mentions("Adam's flight", &names));
        let whole = ["ada lovell".to_owned()];
        assert!(mentions("A flight for  Ada Lovell on Friday", &whole));
        assert!(!mentions("Ada Lovelace's flight", &whole));
        assert!(!mentions("Ada", &whole));
    }
}
