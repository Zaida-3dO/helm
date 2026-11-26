//! Lean speaker chats: a room speaker's chat with a thin system prompt and few tools.
//!
//! A small model answering aloud has to be quick, and a full Helm chat starts with a long system
//! prompt and a long list of tools. When the speaker settings say `lean`, a speaker chat of that
//! profile is launched with:
//!
//! - **a prompt of its own**: a short persona and the speaker's channel prompt (the one Settings →
//!   Hub edits), nothing else: not Claude Code's own prompt, not the rules, the knowledge layers
//!   or the project's notes;
//! - **tools that are few**: Claude Code's built-in tools off, its own MCP servers off, the one
//!   host tool that sends a message to the person's phone ([`HOST_TOOLS`]), and Helm's MCP client
//!   reaching the Home Assistant server alone ([`LeanServers`]).
//!
//! Which chats: those that start in the profile's speaker folder (`speaker/<profileId>`). The
//! setting is read at each launch, so a change applies to the next worker a chat starts. The
//! model and effort of such a chat are the speaker settings' own, unchanged.

use crate::channel::{Channel, ChannelSection};
use crate::speaker;
use helm_core::db::Database;
use helm_hosttools::{AddendumSection, McpServerFilter};
use helm_mcp::McpStore;
use helm_workers::contrib::{LaunchContributor, LaunchCtx, LaunchExtras, LeanLaunch};
use std::path::PathBuf;
use std::sync::Arc;

/// Who a lean speaker chat is, ahead of the channel prompt.
pub const PERSONA: &str = "You are Helm, the voice of this house, heard through a speaker. \
Use your tools to act in the house and answer from what they return. Be brief and plain.";

/// The host tools a lean speaker chat keeps: the one that sends a message to the person's phone.
pub const HOST_TOOLS: [&str; 1] = ["app_telegram_send"];

/// The preset a Home Assistant server is made from, and the name it is given by default.
pub const HOME_ASSISTANT_PRESET: &str = "home_assistant";
const HOME_ASSISTANT_NAME: &str = "home assistant";

/// Whether a chat that runs in `cwd` is a speaker chat of a profile whose settings say lean.
fn is_lean(section: &ChannelSection, chat_id: &str, cwd: &str) -> bool {
    let Some((Channel::Speaker, Some(person))) = section.resolve(chat_id, cwd) else {
        return false;
    };
    speaker::load_file(&speaker::paths(&section.profile_dir, &person).policy).lean
}

/// The launch contributor: marks a lean speaker chat and gives it its prompt.
pub struct LeanSpeaker {
    section: ChannelSection,
}

impl LeanSpeaker {
    pub fn new(db: Arc<Database>, profile_dir: PathBuf) -> Self {
        LeanSpeaker {
            section: ChannelSection { db, profile_dir },
        }
    }
}

impl LaunchContributor for LeanSpeaker {
    fn contribute(&self, ctx: &LaunchCtx<'_>) -> LaunchExtras {
        if !is_lean(&self.section, ctx.chat_id, ctx.cwd) {
            return LaunchExtras::default();
        }
        // The same channel prompt every speaker chat gets: the person's own, else the default.
        let channel = self
            .section
            .text(ctx)
            .unwrap_or_else(|| Channel::Speaker.default_text().to_owned());
        LaunchExtras {
            lean: Some(LeanLaunch {
                system_prompt: format!("{PERSONA}\n\n{}", channel.trim()),
                host_tools: HOST_TOOLS.iter().map(|t| (*t).to_owned()).collect(),
            }),
            ..LaunchExtras::default()
        }
    }
}

/// The servers of Helm's MCP client a chat may reach. An ordinary chat gets whatever the filter
/// before this one says; a lean speaker chat gets the Home Assistant server and nothing else.
pub struct LeanServers {
    pub inner: Option<Arc<dyn McpServerFilter>>,
    pub db: Arc<Database>,
    pub store: Arc<McpStore>,
    pub section: ChannelSection,
}

impl LeanServers {
    /// The folder a chat runs in, from its project.
    fn cwd_of(&self, chat_id: &str) -> Option<String> {
        self.db
            .snapshot(|tx| {
                tx.query_row(
                    "SELECT p.path FROM chats c JOIN projects p ON p.id = c.project_id WHERE c.id = ?1",
                    [chat_id],
                    |r| r.get::<_, Option<String>>(0),
                )
            })
            .ok()
            .flatten()
    }

    /// The ids of the Home Assistant servers Helm has configured.
    fn home_assistant(&self) -> Vec<String> {
        self.store
            .load()
            .unwrap_or_default()
            .into_iter()
            .filter(|s| {
                s.preset.as_deref() == Some(HOME_ASSISTANT_PRESET)
                    || s.name.trim().eq_ignore_ascii_case(HOME_ASSISTANT_NAME)
            })
            .map(|s| s.id)
            .collect()
    }
}

impl McpServerFilter for LeanServers {
    fn allowed(&self, chat_id: &str) -> Option<Vec<String>> {
        let lean = self
            .cwd_of(chat_id)
            .is_some_and(|cwd| is_lean(&self.section, chat_id, &cwd));
        if lean {
            return Some(self.home_assistant());
        }
        self.inner.as_ref().and_then(|f| f.allowed(chat_id))
    }

    fn any(&self) -> bool {
        // A lean profile narrows the list for its speaker chats, so one list for every chat no
        // longer fits.
        let lean_profile = self
            .db
            .snapshot(|tx| helm_profiles::store::list(tx))
            .unwrap_or_default()
            .iter()
            .any(|p| {
                speaker::load_file(&speaker::paths(&self.section.profile_dir, &p.id).policy).lean
            });
        lean_profile || self.inner.as_ref().is_some_and(|f| f.any())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Which chats run lean: only a speaker chat of a profile whose settings say so, found by the
    /// folder it starts in; the prompt is the persona and the channel prompt and nothing else.
    #[test]
    fn only_a_speaker_chat_of_a_lean_profile_runs_lean() {
        let dir = tempfile::tempdir().unwrap();
        let profile_dir = dir.path().to_path_buf();
        let mut reg = helm_core::db::MigrationRegistry::new();
        helm_chats::register_migrations(&mut reg);
        helm_consent::register_migrations(&mut reg);
        helm_profiles::register_migrations(&mut reg);
        helm_devices::register_migrations(&mut reg);
        let db = Arc::new(Database::open(&dir.path().join("t.sqlite3"), &reg).unwrap());
        let home = db
            .snapshot(|tx| helm_profiles::store::home(tx))
            .unwrap()
            .unwrap()
            .id;
        let lean = LeanSpeaker::new(db.clone(), profile_dir.clone());
        let at = |rel: &str| format!("{}/{rel}", profile_dir.display());
        let launch = |cwd: &str| {
            lean.contribute(&LaunchCtx {
                chat_id: "c-1",
                cwd,
            })
            .lean
        };
        let folder = at(&format!("speaker/{home}"));

        // Not lean until the settings say so, whatever the folder.
        assert_eq!(launch(&folder), None);
        let paths = speaker::paths(&profile_dir, &home);
        std::fs::create_dir_all(paths.policy.parent().unwrap()).unwrap();
        speaker::save_policy(
            &paths.policy,
            &speaker::PolicyFile {
                lean: true,
                ..Default::default()
            },
        )
        .unwrap();
        let got = launch(&folder).expect("a speaker chat of a lean profile");
        assert!(
            got.system_prompt.starts_with(PERSONA),
            "{}",
            got.system_prompt
        );
        assert!(
            got.system_prompt.ends_with(Channel::Speaker.default_text()),
            "the channel prompt follows the persona"
        );
        assert_eq!(got.host_tools, ["app_telegram_send"]);
        // A chat anywhere else, in the household's folder, or in another channel, is as before.
        for cwd in [
            at("voice"),
            at(&format!("telegram/{home}")),
            at("speaker/household"),
            "C:/work/other".to_owned(),
        ] {
            assert_eq!(launch(&cwd), None, "{cwd}");
        }
        // A saved prompt of the person's own replaces the default after the persona.
        crate::channel::save_override(&profile_dir, &home, Channel::Speaker, "Hush.").unwrap();
        let got = launch(&folder).unwrap();
        assert_eq!(got.system_prompt, format!("{PERSONA}\n\nHush."));
    }
}
