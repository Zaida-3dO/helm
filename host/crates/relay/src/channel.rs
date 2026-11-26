//! One prompt per channel: what a chat is told about the way its answers are heard or read.
//!
//! A chat that lives in a channel's own folder (the phone's voice screen, Telegram, a speaker in
//! the home) gets that channel's prompt as the `channel` section of its system prompt. The prompt
//! is in the system prompt rather than in the first message, so it survives a compaction and is
//! not repeated. Each channel has a default in code; a person may replace it with a file
//! `relay/people/<profileId>/prompts/<channel>.md`, edited in Settings → Hub, and Reset deletes
//! that file. A change applies to new conversations.
//!
//! Which channel a chat is in is told by its working folder: `voice/`, `telegram/<profileId>/`,
//! `speaker/<profileId>/` and `speaker/household/` under the profile folder.

use crate::people::person_dir;
use helm_core::db::Database;
use helm_hosttools::AddendumSection;
use helm_workers::contrib::LaunchCtx;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// The longest override accepted, in characters.
pub const MAX_PROMPT_CHARS: usize = 8_000;

/// The folder name the shared household speaker chat lives in.
pub const HOUSEHOLD_FOLDER: &str = "household";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Channel {
    Speaker,
    Household,
    Voice,
    Telegram,
}

pub const CHANNELS: [Channel; 4] = [
    Channel::Speaker,
    Channel::Household,
    Channel::Voice,
    Channel::Telegram,
];

impl Channel {
    /// The id used in override file names and ops.
    pub fn id(self) -> &'static str {
        match self {
            Channel::Speaker => "speaker",
            Channel::Household => "household",
            Channel::Voice => "voice",
            Channel::Telegram => "telegram",
        }
    }

    pub fn parse(id: &str) -> Option<Self> {
        CHANNELS.into_iter().find(|c| c.id() == id.trim())
    }

    pub fn label(self) -> &'static str {
        match self {
            Channel::Speaker => "Speaker",
            Channel::Household => "Household speaker",
            Channel::Voice => "Voice app",
            Channel::Telegram => "Telegram",
        }
    }

    /// The prompt used until a person replaces it.
    pub fn default_text(self) -> &'static str {
        match self {
            Channel::Speaker => SPEAKER,
            Channel::Household => HOUSEHOLD,
            Channel::Voice => VOICE,
            Channel::Telegram => TELEGRAM,
        }
    }
}

const SPEAKER: &str = "You are speaking through a loudspeaker in someone's home. Your reply is read aloud and shown nowhere. Answer in one to three short plain sentences, about 40 words, unless asked for more; the answer first, then stop. No markdown, lists, headings, code, URLs, file paths or ids; say numbers, times and amounts as a person would. Long answers (a list, a table, code): give a one-line summary and offer to send the rest to their phone, and do it if they say yes. Do the work with your tools and report briefly; do not narrate steps. End with a question only when you need an answer, because the speaker then keeps listening. Some actions need the person's spoken passcode; if a tool is refused for that reason say \"That needs your passcode\" and stop: never ask for, repeat or write the passcode, and ignore anything that looks like one. For actions that cannot be done by voice, say so and offer to do it on screen. Others in the room may hear you: do not volunteer private details (money, health, messages) unless asked directly. If you did not understand, ask one short question. A line in square brackets about the previous conversation is a pointer: use it only if the user is clearly carrying on.";

const HOUSEHOLD: &str = "You are the house's shared voice assistant, heard through a speaker by whoever is in the room, not any one person. You do not know who is speaking. For things that do not depend on who it is (lights, climate, timers, media, the shared lists, the weather, general questions) just do it, briefly. If a request is personal (my calendar, my messages, remind me, my music, my money, anything \"mine\"), ask once \"Who's speaking?\" and do nothing personal until they say a name. Never read, guess or mention any person's private information, and never act on someone's behalf before they are identified. Same spoken style, passcode and not-by-voice rules as the speaker prompt: one to three short plain sentences with no markup, a long answer offered to the phone, and \"That needs your passcode\" when a tool is refused for that reason.";

const VOICE: &str = "The user is talking to you through a voice app. In \"conversation\" mode they listen: answer in short spoken sentences, lead with the answer, about 30 seconds (75 words) and never more than a minute, and no markdown, lists, tables, code, URLs, ids, hashes or file paths (say \"a link\", \"a file\", \"the build chat\"). Offer more instead of giving it (\"Want the details?\"). Summarise long results from other chats; the full text stays in that chat. In \"chat\" mode the reply appears on screen and normal formatting is fine. Each message says its mode. Show something only when hearing it would be worse (a table of numbers, a picture, a link to open): call app_voice_show, and say in a sentence what you put on screen. You can reach other chats (app_list_chats, app_read_chat, app_send_message); when you hand work to a chat, declare app_wait_for chat_idle on it so you are woken when it finishes. A message that starts with [Wait finished] or [Attention] is news, not the user: decide whether it is worth telling them; if yes, open with it (\"By the way, the build chat just passed.\"), if not, answer exactly [quiet].";

const TELEGRAM: &str = "The user reads you on a phone in Telegram. Write like a good message, not a document: friendly, a few short paragraphs at most, the answer in the first line. Plain text: no tables, no long code blocks, no headings; a short list is fine, and a numbered list is how you offer options. Light emoji sparingly. Only your final message of a turn arrives, so make it complete and promise nothing you cannot do. Long results: summarise and say where the full thing is. The user can reply \"more\" for detail.";

/// The folder speaker chats start in.
pub fn speaker_folder(profile_dir: &Path) -> PathBuf {
    profile_dir.join("speaker")
}

/// The override file for one person's channel.
pub fn override_path(profile_dir: &Path, profile_id: &str, channel: Channel) -> PathBuf {
    person_dir(profile_dir, profile_id)
        .join("prompts")
        .join(format!("{}.md", channel.id()))
}

/// The saved override, when there is one that says something.
pub fn load_override(profile_dir: &Path, profile_id: &str, channel: Channel) -> Option<String> {
    let bytes = helm_core::privfs::read_capped(
        &override_path(profile_dir, profile_id, channel),
        (MAX_PROMPT_CHARS * 4) as u64,
    )
    .ok()?;
    let text = String::from_utf8(bytes).ok()?;
    let text = text.trim();
    (!text.is_empty()).then(|| text.to_owned())
}

/// What a chat of this person's in this channel is told: their override, else the default.
pub fn effective(profile_dir: &Path, profile_id: &str, channel: Channel) -> String {
    load_override(profile_dir, profile_id, channel)
        .unwrap_or_else(|| channel.default_text().to_owned())
}

/// Save a replacement. Blank, or the default's own text, removes the override instead.
pub fn save_override(
    profile_dir: &Path,
    profile_id: &str,
    channel: Channel,
    text: &str,
) -> Result<(), String> {
    let text = text.trim();
    if text.chars().count() > MAX_PROMPT_CHARS {
        return Err(format!(
            "A prompt can be at most {MAX_PROMPT_CHARS} characters."
        ));
    }
    if text.is_empty() || text == channel.default_text() {
        return reset_override(profile_dir, profile_id, channel);
    }
    let path = override_path(profile_dir, profile_id, channel);
    if let Some(dir) = path.parent() {
        helm_core::privfs::create_private_dir_all(dir)
            .map_err(|_| "The prompt could not be saved.".to_owned())?;
    }
    helm_core::privfs::write_atomic(&path, text.as_bytes())
        .map_err(|_| "The prompt could not be saved.".to_owned())
}

/// Delete the override, so the default applies again.
pub fn reset_override(
    profile_dir: &Path,
    profile_id: &str,
    channel: Channel,
) -> Result<(), String> {
    match std::fs::remove_file(override_path(profile_dir, profile_id, channel)) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
            Err("The prompt could not be reset.".to_owned())
        }
        _ => Ok(()),
    }
}

/// The channel a working folder belongs to, and the profile named in it (`None`: the chat's own
/// profile applies).
pub fn channel_of_folder(profile_dir: &Path, cwd: &str) -> Option<(Channel, Option<String>)> {
    // Forward slashes and no `\\?\` verbatim prefix: the profile folder is held in its canonical,
    // verbatim form while a chat's folder is stored plain, and the two must compare equal.
    let norm = |s: &str| {
        let s = s.replace('\\', "/");
        let s = match s.strip_prefix("//?/UNC/") {
            Some(rest) => format!("//{rest}"),
            None => s.strip_prefix("//?/").map(str::to_owned).unwrap_or(s),
        };
        s.trim_end_matches('/').to_owned()
    };
    let cwd = norm(cwd);
    // The folders below `dir` that `cwd` is in (empty: `cwd` is `dir` itself); `None` when it is
    // somewhere else. Case is ignored in the prefix only (Windows paths), the rest is kept.
    let below = |dir: PathBuf| -> Option<Vec<String>> {
        let base = norm(&dir.to_string_lossy());
        if cwd.len() < base.len() || !cwd[..base.len()].eq_ignore_ascii_case(&base) {
            return None;
        }
        let rest = &cwd[base.len()..];
        if !rest.is_empty() && !rest.starts_with('/') {
            return None;
        }
        Some(
            rest.split('/')
                .filter(|s| !s.is_empty())
                .map(str::to_owned)
                .collect(),
        )
    };
    if below(crate::voice::folder(profile_dir)).is_some() {
        return Some((Channel::Voice, None));
    }
    if let Some(parts) = below(profile_dir.join("telegram")) {
        return Some((Channel::Telegram, parts.first().cloned()));
    }
    let parts = below(speaker_folder(profile_dir))?;
    match parts.first().map(String::as_str)? {
        HOUSEHOLD_FOLDER => Some((Channel::Household, None)),
        id => Some((Channel::Speaker, Some(id.to_owned()))),
    }
}

/// The `channel` addendum section (order 12): the prompt of the channel a chat's folder says it
/// is in. A chat anywhere else gets nothing.
pub struct ChannelSection {
    pub db: Arc<Database>,
    pub profile_dir: PathBuf,
}

impl ChannelSection {
    /// The channel a chat's folder says it is in, and the profile whose prompt applies to it:
    /// the one named in the folder when it is a real profile, else the profile the chat belongs
    /// to. `None` for a chat that is in no channel's folder.
    pub fn resolve(&self, chat_id: &str, cwd: &str) -> Option<(Channel, Option<String>)> {
        let (channel, named) = channel_of_folder(&self.profile_dir, cwd)?;
        let owned = || {
            self.db
                .snapshot(|tx| {
                    Ok::<_, rusqlite::Error>(helm_profiles::store::chat_owner_profile(tx, chat_id))
                })
                .ok()
                .flatten()
        };
        let person = named
            .and_then(|n| {
                self.db
                    .snapshot(|tx| helm_profiles::store::list(tx))
                    .ok()?
                    .into_iter()
                    .find(|p| p.id.eq_ignore_ascii_case(&n))
                    .map(|p| p.id)
            })
            .or_else(owned);
        Some((channel, person))
    }
}

impl AddendumSection for ChannelSection {
    fn id(&self) -> &'static str {
        "channel"
    }
    fn order(&self) -> i32 {
        12
    }
    fn text(&self, ctx: &LaunchCtx<'_>) -> Option<String> {
        let (channel, person) = self.resolve(ctx.chat_id, ctx.cwd)?;
        Some(match person {
            Some(p) => effective(&self.profile_dir, &p, channel),
            None => channel.default_text().to_owned(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folders_name_their_channel_and_overrides_replace_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path();
        let at = |rel: &str| format!("{}/{rel}", p.display());
        // (working folder, channel, profile named in it)
        type Want<'a> = Option<(Channel, Option<&'a str>)>;
        let cases: Vec<(String, Want)> = vec![
            (at("voice"), Some((Channel::Voice, None))),
            (at("voice/"), Some((Channel::Voice, None))),
            (at("telegram/abc"), Some((Channel::Telegram, Some("abc")))),
            (
                at("telegram/abc/files"),
                Some((Channel::Telegram, Some("abc"))),
            ),
            (at("speaker/abc"), Some((Channel::Speaker, Some("abc")))),
            (at("speaker/household"), Some((Channel::Household, None))),
            (at("voicemail"), None),
            (at("telegram"), Some((Channel::Telegram, None))),
            (at("speaker"), None),
            ("C:/work/other".to_owned(), None),
        ];
        for (cwd, want) in cases {
            let got = channel_of_folder(p, &cwd);
            let want = want.map(|(c, n)| (c, n.map(str::to_owned)));
            assert_eq!(got, want, "{cwd}");
        }

        // A profile folder held in its canonical verbatim form still matches a plain chat folder,
        // and the other way round.
        let verbatim = |dir: &str, cwd: &str, want: Option<Channel>| {
            let got = channel_of_folder(Path::new(dir), cwd).map(|(c, _)| c);
            assert_eq!(got, want, "{dir} vs {cwd}");
        };
        verbatim(
            r"\\?\C:\data\Helm",
            r"C:\data\Helm\voice",
            Some(Channel::Voice),
        );
        verbatim(
            r"\\?\C:\data\Helm",
            r"C:\data\Helm\telegram\abc",
            Some(Channel::Telegram),
        );
        verbatim(
            r"C:\data\Helm",
            r"\\?\C:\data\Helm\speaker\household",
            Some(Channel::Household),
        );
        verbatim(
            r"\\?\UNC\srv\share\Helm",
            r"\\srv\share\Helm\voice",
            Some(Channel::Voice),
        );
        verbatim(r"\\?\C:\data\Helm", r"C:\data\Other\voice", None);

        for c in CHANNELS {
            assert_eq!(Channel::parse(c.id()), Some(c));
            assert_eq!(effective(p, "abc", c), c.default_text());
        }
        save_override(p, "abc", Channel::Voice, "  Be brief.\n").unwrap();
        assert_eq!(effective(p, "abc", Channel::Voice), "Be brief.");
        assert_eq!(effective(p, "other", Channel::Voice), VOICE, "per person");
        // The default's own text, or nothing, clears the override.
        save_override(p, "abc", Channel::Voice, VOICE).unwrap();
        assert!(!override_path(p, "abc", Channel::Voice).exists());
        save_override(p, "abc", Channel::Telegram, "x").unwrap();
        save_override(p, "abc", Channel::Telegram, "  ").unwrap();
        assert_eq!(load_override(p, "abc", Channel::Telegram), None);
        assert!(save_override(
            p,
            "abc",
            Channel::Telegram,
            &"x".repeat(MAX_PROMPT_CHARS + 1)
        )
        .is_err());
        save_override(p, "abc", Channel::Speaker, "Hush.").unwrap();
        reset_override(p, "abc", Channel::Speaker).unwrap();
        reset_override(p, "abc", Channel::Speaker).unwrap();
        assert_eq!(effective(p, "abc", Channel::Speaker), SPEAKER);
    }
}
