//! Lean speaker chats: a room speaker's chat with a thin system prompt and few tools.
//!
//! A small model answering aloud has to be quick, and a full Helm chat starts with a long system
//! prompt and a long list of tools. When the speaker settings say `lean`, a speaker chat of that
//! profile is launched with:
//!
//! - **a prompt of its own**: the built-in speaker prompt [`DEFAULT_PROMPT`], or the person's own
//!   replacement (`leanPrompt` in the policy file, edited in Settings → Hub) — nothing else: not
//!   Claude Code's own prompt, not the rules, the knowledge layers or the project's notes, and
//!   not the channel prompt (the built-in text already carries the spoken style);
//! - **tools that are few**: Claude Code's built-in tools off, its own MCP servers off, the host
//!   tools named by `leanTools` in the policy file (default [`DEFAULT_TOOLS`]) listed, every other
//!   host tool hidden from the chat's list but still reachable on demand through the escape-hatch
//!   pair the worker always serves (`app_list_tools` to find, `app_mcp_call` to call), and Helm's
//!   MCP client reaching only the servers the list names ([`LeanServers`]).
//!
//! Which chats: those that start in the profile's speaker folder (`speaker/<profileId>`). The
//! setting is read at each launch, so a change applies to the next worker a chat starts. The
//! model and effort of such a chat are the speaker settings' own, unchanged.

use crate::channel::{Channel, ChannelSection};
use crate::speaker;
use helm_core::db::Database;
use helm_hosttools::McpServerFilter;
use helm_mcp::types::McpServer;
use helm_mcp::{config::McpStore, presets::connected_memory_server};
use helm_workers::contrib::{LaunchContributor, LaunchCtx, LaunchExtras, LeanLaunch};
use std::path::PathBuf;
use std::sync::Arc;

/// The built-in system prompt for a lean speaker chat, tuned for a home speaker: spoken style,
/// what the speaker is for, and one line each on finding more tools and pulling up past chats.
/// Some 910 characters, roughly 240 tokens; `leanPrompt` in the policy file replaces it whole.
pub const DEFAULT_PROMPT: &str = "You are Helm, the voice of this house, answering out loud \
through a room speaker. Answer in one to three short plain sentences, about 40 words unless \
asked for more: the answer first, then stop. No markdown, lists, links or code; say numbers, \
times and amounts as a person would. Never ask something that needs a screen: offer to send it \
to the phone instead. Your tools do home control (lights, heating, media), quick facts, timers \
and reminders. For anything personal (money, health, private messages) say little aloud and \
offer to pass it on or send it to the phone. You can use more of Helm's tools than you were \
given: find them with app_list_tools and call one with app_mcp_call. If this message names an \
ended talk, its chat id is in that line: read what was said there with app_read_chat, and find \
older closed chats with app_list_chats after setting archived to true. End with a question only \
when you need the answer.";

/// The preset a Home Assistant server is made from, and the name it is given by default.
pub const HOME_ASSISTANT_PRESET: &str = "home_assistant";
const HOME_ASSISTANT_NAME: &str = "home assistant";
/// The capability name the shared-memory server answers to in a lean tool list (its search tool;
/// the server's own id or name may be used too).
pub const MEMORY_ENTRY: &str = "memory_search";

/// What a lean speaker chat may use until the settings say otherwise: the Home Assistant server,
/// the shared memories (when they are connected), reminders and timers, finding and reading past
/// chats, and sending a message to the person's phone. Server-ish entries pick MCP servers; `app_`
/// entries pick host tools; each entry may match either way.
pub const DEFAULT_TOOLS: [&str; 6] = [
    HOME_ASSISTANT_NAME,
    MEMORY_ENTRY,
    "app_schedule",
    "app_list_chats",
    "app_read_chat",
    "app_telegram_send",
];

/// The host-tool entries of a lean list (`app_…`); the rest pick MCP servers.
fn host_entries(list: &[String]) -> Vec<String> {
    list.iter()
        .filter(|t| t.trim_start().starts_with(helm_hosttools::spec::PREFIX))
        .map(|t| t.trim().to_owned())
        .collect()
}

/// Entries of the policy's list, or the defaults when it names none.
fn effective_tools(policy: &speaker::PolicyFile) -> Vec<String> {
    policy
        .lean_tools
        .clone()
        .unwrap_or_else(|| DEFAULT_TOOLS.iter().map(|t| (*t).to_owned()).collect())
}

/// Case- and separator-insensitive name compare, so "home assistant" finds the preset's
/// `home_assistant` and a saved server named "Home Assistant".
fn same_name(a: &str, b: &str) -> bool {
    let norm = |t: &str| {
        t.to_lowercase()
            .chars()
            .filter(|c| !matches!(c, ' ' | '_' | '-' | '.'))
            .collect::<String>()
    };
    norm(a) == norm(b)
}

/// The policy of a lean speaker chat found by its working folder; `None` for any other chat.
fn lean_policy(section: &ChannelSection, chat_id: &str, cwd: &str) -> Option<speaker::PolicyFile> {
    let (Channel::Speaker, Some(person)) = section.resolve(chat_id, cwd)? else {
        return None;
    };
    let file = speaker::load_file(&speaker::paths(&section.profile_dir, &person).policy);
    file.lean.then_some(file)
}

/// The launch contributor: marks a lean speaker chat and gives it its prompt and host tools.
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
        let Some(file) = lean_policy(&self.section, ctx.chat_id, ctx.cwd) else {
            return LaunchExtras::default();
        };
        let prompt = {
            let own = file.lean_prompt.trim();
            if own.is_empty() {
                DEFAULT_PROMPT.to_owned()
            } else {
                own.to_owned()
            }
        };
        LaunchExtras {
            lean: Some(LeanLaunch {
                system_prompt: prompt,
                host_tools: host_entries(&effective_tools(&file)),
            }),
            ..LaunchExtras::default()
        }
    }
}

/// The enabled servers a tool list lets through: an entry matches a server's preset, name or id
/// (spacers and case ignored), and the connected shared-memory server also answers to the
/// capability name [`MEMORY_ENTRY`].
pub fn allowed_servers(servers: &[McpServer], entries: &[String]) -> Vec<String> {
    let memory_id = connected_memory_server(servers);
    servers
        .iter()
        .filter(|s| s.enabled)
        .filter(|s| {
            entries.iter().any(|e| {
                let e = e.trim();
                if memory_id.as_deref() == Some(s.id.as_str()) && same_name(e, MEMORY_ENTRY) {
                    return true;
                }
                s.preset.as_deref().is_some_and(|p| same_name(e, p))
                    || same_name(e, &s.name)
                    || same_name(e, &s.id)
            })
        })
        .map(|s| s.id.clone())
        .collect()
}

/// The servers of Helm's MCP client a chat may reach. An ordinary chat gets whatever the filter
/// before this one says; a lean speaker chat gets only the servers its list names — by default
/// the Home Assistant server and, when connected, the shared-memory server.
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

    /// The enabled servers a lean chat may reach, from its tool list.
    fn allowed_for(&self, file: &speaker::PolicyFile) -> Vec<String> {
        allowed_servers(
            &self.store.load().unwrap_or_default(),
            &effective_tools(file),
        )
    }
}

impl McpServerFilter for LeanServers {
    fn allowed(&self, chat_id: &str) -> Option<Vec<String>> {
        let lean = self
            .cwd_of(chat_id)
            .and_then(|cwd| lean_policy(&self.section, chat_id, &cwd));
        if let Some(file) = lean {
            return Some(self.allowed_for(&file));
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

    fn db_at(dir: &std::path::Path) -> Arc<Database> {
        let mut reg = helm_core::db::MigrationRegistry::new();
        helm_chats::register_migrations(&mut reg);
        helm_consent::register_migrations(&mut reg);
        helm_profiles::register_migrations(&mut reg);
        helm_devices::register_migrations(&mut reg);
        Arc::new(Database::open(&dir.join("t.sqlite3"), &reg).unwrap())
    }

    /// Which chats run lean: only a speaker chat of a profile whose settings say so, found by the
    /// folder it starts in; the prompt is the built-in one and nothing else.
    #[test]
    fn only_a_speaker_chat_of_a_lean_profile_runs_lean() {
        let dir = tempfile::tempdir().unwrap();
        let profile_dir = dir.path().to_path_buf();
        let db = db_at(dir.path());
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
        assert_eq!(got.system_prompt, DEFAULT_PROMPT);
        // The default set keeps the phone-message tool and nothing else of the host surface.
        assert_eq!(
            got.host_tools,
            [
                "app_schedule",
                "app_list_chats",
                "app_read_chat",
                "app_telegram_send"
            ]
        );
        // A chat anywhere else, in the household's folder, or in another channel, is as before.
        for cwd in [
            at("voice"),
            at(&format!("telegram/{home}")),
            at("speaker/household"),
            "C:/work/other".to_owned(),
        ] {
            assert_eq!(launch(&cwd), None, "{cwd}");
        }
        // A saved prompt of the person's own replaces the built-in one whole.
        speaker::save_policy(
            &paths.policy,
            &speaker::PolicyFile {
                lean: true,
                lean_prompt: " Hush. ".into(),
                ..Default::default()
            },
        )
        .unwrap();
        let got = launch(&folder).unwrap();
        assert_eq!(got.system_prompt, "Hush.");
    }

    /// The tool set is the setting's list: default entries, a custom list (host-tool entries go
    /// to the chat's list, server entries to the MCP filter), and an empty list as a real choice.
    #[test]
    fn the_lean_tool_set_comes_from_the_setting() {
        let dir = tempfile::tempdir().unwrap();
        let profile_dir = dir.path().to_path_buf();
        let db = db_at(dir.path());
        let home = db
            .snapshot(|tx| helm_profiles::store::home(tx))
            .unwrap()
            .unwrap()
            .id;
        let lean = LeanSpeaker::new(db.clone(), profile_dir.clone());
        let paths = speaker::paths(&profile_dir, &home);
        std::fs::create_dir_all(paths.policy.parent().unwrap()).unwrap();
        let folder = format!("{}/{}/{}", profile_dir.display(), "speaker", home);
        let tools_of = || {
            lean.contribute(&LaunchCtx {
                chat_id: "c-1",
                cwd: &folder,
            })
            .lean
            .expect("lean")
            .host_tools
        };

        // Default: the app's set, host-tool entries only; server-ish entries name no tool.
        speaker::save_policy(
            &paths.policy,
            &speaker::PolicyFile {
                lean: true,
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(
            tools_of(),
            [
                "app_schedule",
                "app_list_chats",
                "app_read_chat",
                "app_telegram_send"
            ]
        );
        // A custom list wins whole; unknown and server-ish entries simply name no host tool.
        speaker::save_policy(
            &paths.policy,
            &speaker::PolicyFile {
                lean: true,
                lean_tools: Some(vec![
                    "home assistant".into(),
                    "app_telegram_send".into(),
                    "app_attention".into(),
                ]),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(tools_of(), ["app_telegram_send", "app_attention"]);
        // An empty list is the real choice: no host tools of its own. The escape-hatch pair the
        // worker always serves still stands.
        speaker::save_policy(
            &paths.policy,
            &speaker::PolicyFile {
                lean: true,
                lean_tools: Some(vec![]),
                ..Default::default()
            },
        )
        .unwrap();
        assert!(tools_of().is_empty());
    }

    /// Which MCP servers a lean chat reaches: by default the Home Assistant server and the
    /// connected memory server; then what the list names — by capability, name or id. A chat that
    /// is not lean leaves the filter before it alone.
    #[test]
    fn lean_servers_follow_the_list() {
        use helm_mcp::types::{Transport, TransportKind};
        use std::collections::BTreeMap;
        let http = |url: &str| McpServer {
            id: String::new(),
            name: String::new(),
            enabled: true,
            transport: Transport {
                kind: TransportKind::Http,
                command: None,
                args: vec![],
                env: BTreeMap::new(),
                url: Some(url.into()),
                headers: BTreeMap::new(),
                oauth: None,
            },
            allow_private_http: false,
            preset: None,
        };
        let servers = vec![
            McpServer {
                id: "ha".into(),
                name: "Home Assistant".into(),
                preset: Some(HOME_ASSISTANT_PRESET.into()),
                ..http("http://127.0.0.1:8123/mcp")
            },
            McpServer {
                id: "helm_hub".into(),
                name: "Helm hub memory".into(),
                preset: Some("helm_hub".into()),
                ..http("http://127.0.0.1/mcp")
            },
            McpServer {
                id: "off_one".into(),
                name: "Home Assistant old".into(),
                enabled: false,
                preset: Some(HOME_ASSISTANT_PRESET.into()),
                ..http("http://127.0.0.1:8124/mcp")
            },
            McpServer {
                id: "mail".into(),
                name: "My mail".into(),
                ..http("http://127.0.0.1/mail")
            },
        ];
        let defaults: Vec<String> = DEFAULT_TOOLS.iter().map(|t| (*t).to_owned()).collect();
        // Default entries: the Home Assistant server (by preset/name) and the connected memory
        // server (`memory_search` capability); a disabled or unlisted server never reaches in.
        assert_eq!(allowed_servers(&servers, &defaults), ["ha", "helm_hub"]);
        let of = |names: &[&str]| {
            allowed_servers(
                &servers,
                &names.iter().map(|n| (*n).to_owned()).collect::<Vec<_>>(),
            )
        };
        // Entries match a server's id or saved name too; spacers and case are ignored.
        assert_eq!(of(&["HA", "Helm hub memory"]), ["ha", "helm_hub"]);
        // Dropping `memory_search` shuts the memory server out unless it is named outright.
        assert_eq!(of(&["home assistant"]), ["ha"]);
        assert_eq!(of(&["memory_search"]), ["helm_hub"]);
        // A nameless entry matches nothing; an empty list reaches no server at all.
        assert!(of(&["nope", "  "]).is_empty());
        assert_eq!(of(&[]), Vec::<String>::new());
    }

    /// An ordinary (non-lean) chat is left to the filter before `LeanServers`.
    #[test]
    fn a_chat_that_is_not_lean_leaves_the_inner_filter_alone() {
        struct InnerFilter;
        impl McpServerFilter for InnerFilter {
            fn allowed(&self, _chat_id: &str) -> Option<Vec<String>> {
                Some(vec!["any".into()])
            }
            fn any(&self) -> bool {
                true
            }
        }
        let dir = tempfile::tempdir().unwrap();
        let profile_dir = dir.path().to_path_buf();
        let db = db_at(dir.path());
        let store = Arc::new(McpStore::new(
            dir.path(),
            helm_core::credentials::CredentialStore::new(
                &profile_dir.join("credentials"),
                "testhash",
                Arc::new(helm_platform::keychain::MemoryKeychain::default()),
            ),
        ));
        let servers = LeanServers {
            inner: Some(Arc::new(InnerFilter)),
            db: db.clone(),
            store,
            section: ChannelSection {
                db: db.clone(),
                profile_dir,
            },
        };
        // Chat `c-9` has no row here at all, so it is certainly not a lean speaker chat.
        assert_eq!(servers.allowed("c-9"), Some(vec!["any".into()]));
        assert!(
            servers.any(),
            "the inner filter says one list fits every chat"
        );
    }

    /// A chat's id rides in the pointer line of a new talk — and only when the rooms match. The
    /// row for this is in the speaker tests; here the lean prompt says which tools to use with it.
    #[test]
    fn the_prompt_names_the_escape_hatch_and_the_past_chat_tools() {
        for words in [
            "app_list_tools",
            "app_mcp_call",
            "app_read_chat",
            "app_list_chats",
        ] {
            assert!(DEFAULT_PROMPT.contains(words), "{words} missing");
        }
    }
}
