//! Whose an item is, for everything that differs between people or between computers (rules,
//! skills, agents, memories, settings, saved items, schedules, chats, MCP servers...): it is
//! **Shared** (shown and used by everyone by default) or **Personal** (shown and applied only for
//! the person it was made for).
//!
//! Personal is a default view, never a permission. It keeps what is irrelevant to someone out of
//! their screens and their chats; it does not stop anyone. Any other person, or any chat of theirs,
//! can still list, read, edit, copy, move or delete a Personal item by naming the person (or
//! the device) in the call, with the same notice and Undo as any change a chat makes directly.
//! There is no third value (nothing "owner only", nothing "private to the owner"): code that
//! decides who sees an item decides it with this type, and a refusal for a missing right has no
//! place in it.
//!
//! Existing stores keep the words they already hold (`everyone` and `me`, a layer or a
//! visibility); [`Scope::parse`] and [`Scope::word`] map them onto the type, so nothing is
//! migrated.

/// Shared with everyone, or Personal to one person (or device).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Scope {
    /// Shown and used by everyone by default.
    Shared,
    /// Shown and applied only for the person or computer it was made for; others reach it by
    /// naming that person or computer.
    Personal,
}

impl Scope {
    /// The scope a word stands for: `everyone` or `shared`, or `me`, `personal` or `private`.
    /// Anything else is no scope.
    pub fn parse(word: &str) -> Option<Self> {
        match word.trim().to_ascii_lowercase().as_str() {
            "everyone" | "shared" => Some(Self::Shared),
            "me" | "personal" | "private" => Some(Self::Personal),
            _ => None,
        }
    }

    /// The word the tools and files use: `everyone` or `me`.
    pub fn word(self) -> &'static str {
        match self {
            Self::Shared => "everyone",
            Self::Personal => "me",
        }
    }

    /// Whether a listing that names nobody shows it to a person it was not made for.
    pub fn shown_by_default(self, made_for_them: bool) -> bool {
        match self {
            Self::Shared => true,
            Self::Personal => made_for_them,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn words_map_onto_two_scopes_and_back() {
        for (word, want) in [
            ("everyone", Some(Scope::Shared)),
            ("shared", Some(Scope::Shared)),
            (" Me ", Some(Scope::Personal)),
            ("private", Some(Scope::Personal)),
            ("personal", Some(Scope::Personal)),
            ("owner", None),
            ("", None),
        ] {
            assert_eq!(Scope::parse(word), want, "{word:?}");
        }
        assert_eq!(Scope::Shared.word(), "everyone");
        assert_eq!(Scope::Personal.word(), "me");
        // The default view hides what was made for someone else; naming them is how it is reached.
        assert!(Scope::Shared.shown_by_default(false));
        assert!(!Scope::Personal.shown_by_default(false));
        assert!(Scope::Personal.shown_by_default(true));
    }
}
