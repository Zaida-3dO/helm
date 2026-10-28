//! Errors as the renderer sees them: `[Class] sentence`. The page maps the class to its own
//! wording and shows the sentence; nothing in a sentence is a token or an address to follow.
//!
//! Every class except `Uncertain` means nothing reached GitHub (or GitHub refused it), so the page
//! keeps what was typed and the user may click again. `Uncertain` means a write may have gone
//! through: the page says so and offers no button that sends again by itself.

use helm_ipc::ApiError;
use std::fmt;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Class {
    /// The request itself is malformed or outside the allowlist.
    Invalid,
    /// The pull request view is off.
    Disabled,
    /// Not allowed, a stale settings revision, or GitHub refused a write (a 4xx).
    Refused,
    /// The GitHub CLI is not signed in, or GitHub refused its sign-in.
    Unauthorized,
    /// The GitHub CLI is not installed or could not start.
    Unavailable,
    /// GitHub's rate limit is reached; requests fail at once until the pause ends.
    RateLimited,
    TooLarge,
    /// No answer (deadline, connection) to a read.
    Network,
    /// An unreadable answer, or GraphQL errors with no data.
    GitHub,
    /// The pull request is closed or merged.
    Closed,
    /// The pull request's head is not the commit the page showed.
    HeadMoved,
    /// The user has a review in progress on GitHub, which Helm does not continue.
    ReviewPending,
    /// A write that may have gone through.
    Uncertain,
}

impl Class {
    pub fn tag(self) -> &'static str {
        match self {
            Self::Invalid => "Invalid",
            Self::Disabled => "Disabled",
            Self::Refused => "Refused",
            Self::Unauthorized => "Unauthorized",
            Self::Unavailable => "Unavailable",
            Self::RateLimited => "RateLimited",
            Self::TooLarge => "TooLarge",
            Self::Network => "Network",
            Self::GitHub => "GitHub",
            Self::Closed => "Closed",
            Self::HeadMoved => "HeadMoved",
            Self::ReviewPending => "ReviewPending",
            Self::Uncertain => "Uncertain",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GhError {
    pub class: Class,
    pub message: String,
}

impl GhError {
    pub fn new(class: Class, message: impl Into<String>) -> Self {
        Self {
            class,
            message: crate::scrub(&message.into()),
        }
    }
    pub fn invalid(message: impl Into<String>) -> Self {
        Self::new(Class::Invalid, message)
    }
}

impl fmt::Display for GhError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "[{}] {}", self.class.tag(), self.message)
    }
}

impl From<GhError> for ApiError {
    fn from(e: GhError) -> Self {
        ApiError::new("github", e.to_string())
    }
}

pub type GhResult<T> = Result<T, GhError>;
