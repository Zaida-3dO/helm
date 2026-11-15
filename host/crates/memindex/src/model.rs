//! The shapes of the registered sources, the settings, and what Settings shows.

use serde::{Deserialize, Serialize};

/// What a source reads.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
#[ts(rename = "MemindexKind")]
pub enum Kind {
    /// A folder of markdown notes, watched.
    Folder,
    /// This PC's Helm chats.
    HelmChats,
    /// Claude Code's own session records.
    ClaudeCode,
    /// Parsed chat logs: one weekly file per doc.
    ChatLogs,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Folder => "folder",
            Kind::HelmChats => "helm_chats",
            Kind::ClaudeCode => "claude_code",
            Kind::ChatLogs => "chat_logs",
        }
    }

    pub fn parse(s: &str) -> Option<Kind> {
        Some(match s {
            "folder" => Kind::Folder,
            "helm_chats" => Kind::HelmChats,
            "claude_code" => Kind::ClaudeCode,
            "chat_logs" => Kind::ChatLogs,
            _ => return None,
        })
    }

    /// Whether the source reads a folder the user names.
    pub fn has_path(self) -> bool {
        matches!(self, Kind::Folder | Kind::ChatLogs)
    }
}

/// Who may find a source's docs: only its person (`me`), or everyone in the household.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
#[ts(rename = "MemindexScope")]
pub enum Scope {
    Me,
    Everyone,
}

impl Scope {
    pub fn as_str(self) -> &'static str {
        match self {
            Scope::Me => "me",
            Scope::Everyone => "everyone",
        }
    }

    pub fn parse(s: &str) -> Option<Scope> {
        match s {
            "me" => Some(Scope::Me),
            "everyone" => Some(Scope::Everyone),
            _ => None,
        }
    }
}

/// One registered source.
#[derive(Clone, Debug, PartialEq)]
pub struct Source {
    pub id: String,
    pub kind: Kind,
    /// The folder, for the kinds that read one.
    pub path: String,
    /// The profile whose material this is; empty is the home profile.
    pub person_id: String,
    pub scope: Scope,
    /// Glob patterns (`*`, `**`, `?`) over the path relative to the folder. Empty include means
    /// every markdown file.
    pub include: Vec<String>,
    pub exclude: Vec<String>,
    pub enabled: bool,
    pub created_at: i64,
}

/// The saved switches.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Settings {
    pub enabled: bool,
    /// The local server (an Ollama) that embeds.
    pub server_id: String,
    pub model: String,
    /// The nightly full scan, `HH:MM` local time.
    pub nightly: String,
    /// Names this install's sources on the hub.
    pub machine_id: String,
}

pub const DEFAULT_MODEL: &str = "nomic-embed-text";
pub const DEFAULT_NIGHTLY: &str = "03:30";

impl Default for Settings {
    fn default() -> Self {
        Settings {
            enabled: false,
            server_id: String::new(),
            model: DEFAULT_MODEL.into(),
            nightly: DEFAULT_NIGHTLY.into(),
            machine_id: String::new(),
        }
    }
}

/// The model's name as the hub keeps it: the tag dropped.
pub fn normalise_model(m: &str) -> String {
    m.trim().split(':').next().unwrap_or("").to_owned()
}

/// A time of day, `HH:MM`, as minutes after midnight.
pub fn parse_nightly(s: &str) -> Option<u32> {
    let (h, m) = s.split_once(':')?;
    let (h, m): (u32, u32) = (h.parse().ok()?, m.parse().ok()?);
    (h < 24 && m < 60 && s.len() == 5).then_some(h * 60 + m)
}

/// One source as Settings shows it.
#[derive(Clone, Debug, PartialEq, Serialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename = "MemindexSource")]
pub struct SourceView {
    pub id: String,
    pub kind: Kind,
    pub path: String,
    pub person_id: String,
    pub scope: Scope,
    pub include: Vec<String>,
    pub exclude: Vec<String>,
    pub enabled: bool,
    /// Docs this PC holds for the source, and how many the hub has taken.
    pub docs: u32,
    pub pushed: u32,
    /// One plain line: what the source is doing or why it is not.
    pub status: String,
    #[ts(type = "number")]
    pub last_scan: i64,
}

/// Everything Settings → Memory shows.
#[derive(Clone, Debug, PartialEq, Serialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename = "MemindexState")]
pub struct StateView {
    pub enabled: bool,
    pub server_id: String,
    pub model: String,
    pub nightly: String,
    /// `ready`, `down` (no line to the hub) or `too_old` (the hub does not index).
    pub hub: String,
    /// One plain line for the whole feature.
    pub status: String,
    /// Chunks the hub is still waiting to get a vector for, when known.
    pub pending: Option<u32>,
    pub sources: Vec<SourceView>,
}
