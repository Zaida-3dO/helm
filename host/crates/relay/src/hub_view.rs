//! The `hubConnection` key of `app_get_settings`: whether this PC is connected to the hub, for
//! chats.
//!
//! A chat that is setting a PC up needs to know if it is paired, whether the line is up and who the
//! hub says this computer is. The value is `{paired, state, personName, machineName, hasFallback,
//! memoryServer}`: `state` is the line's (`off`, `connecting`, `online` or `offline`) and
//! `memoryServer` is true while the shared-memory server pairing adds is present and enabled. The
//! names are empty when the connection was typed by hand rather than made with a pairing code.
//!
//! Never the hub's address, the token or any id: the address is network layout, and a transcript
//! can leave this PC. The key is read only; changing it is refused with the way to pair.

use crate::client::LineHandle;
use crate::config::{self, RelayConfig};
use crate::pair::MEMORY_SERVER_ID;
use helm_core::credentials::CredentialStore;
use helm_core::profile::Profile;
use helm_hosttools::{SettingsExtension, StagedPart};
use helm_ipc::{AppEvent, Services};
use helm_platform::Platform;
use rusqlite::Transaction;
use serde_json::{json, Value};
use std::sync::Arc;

pub const KEY: &str = "hubConnection";
/// Names come from the hub; keep what a chat sees short and on one line.
const MAX_NAME: usize = 64;

const READ_ONLY_ANSWER: &str =
    "hubConnection is read only: it reports the connection. To pair, call app_stage_settings with changes {\"hubPairing\": {...}} and the user approves it.";

type StateFn = Box<dyn Fn() -> &'static str + Send + Sync>;
type MemoryFn = Box<dyn Fn() -> bool + Send + Sync>;

struct HubView {
    creds: CredentialStore,
    line_state: StateFn,
    memory_server: MemoryFn,
}

/// The extension to register as a [`helm_hosttools::SettingsExtensions`] entry, or `None` while the
/// profile or platform is missing (nothing then says where the connection is kept).
pub fn settings_extension(services: &Services) -> Option<Arc<dyn SettingsExtension>> {
    let profile = services.get::<Profile>()?;
    let platform = services.get::<Platform>()?;
    let line = services.get::<LineHandle>();
    let mcp = services.get::<helm_mcp::McpStore>();
    Some(Arc::new(HubView {
        creds: crate::credentials(&profile, &platform),
        line_state: Box::new(move || line.as_ref().map_or("offline", |l| l.state().as_str())),
        memory_server: Box::new(move || {
            mcp.as_ref().is_some_and(|m| {
                m.load()
                    .unwrap_or_default()
                    .iter()
                    .any(|s| s.id == MEMORY_SERVER_ID && s.enabled)
            })
        }),
    }))
}

fn short(name: &str) -> String {
    name.chars()
        .filter(|c| !c.is_control())
        .take(MAX_NAME)
        .collect()
}

impl HubView {
    fn value(&self, c: &RelayConfig) -> Value {
        let v = config::view(c, (self.line_state)());
        json!({
            "paired": c.usable(),
            "state": v.state,
            "personName": short(&c.person_name),
            "machineName": short(&c.machine_name),
            "hasFallback": v.has_fallback,
            "memoryServer": (self.memory_server)(),
        })
    }
}

impl SettingsExtension for HubView {
    fn staged_parts(&self, _key: &str) -> &'static [StagedPart] {
        &[]
    }

    fn keys(&self) -> Vec<&'static str> {
        vec![KEY]
    }

    fn describe(&self, key: &str) -> Option<Value> {
        (key == KEY).then(|| {
            json!({
                "type": "object",
                "readOnly": true,
                "default": {
                    "paired": false,
                    "state": "off",
                    "personName": "",
                    "machineName": "",
                    "hasFallback": false,
                    "memoryServer": false,
                },
                "about": "Read only: whether this PC is connected to the home hub. paired: a hub address and token are set. state: the line to the hub (off, connecting, online or offline). personName and machineName: who the hub says this PC is (empty when the connection was typed in by hand). hasFallback: a second address is set. memoryServer: the hub's shared-memory MCP server is added and enabled. configProblem: present only when a saved connection exists but could not be read, with the reason. The address, the token and any id are never shown. To pair, use app_stage_settings (the user approves it) or ask the user to use Settings > Hub.",
                "hasEffect": true,
            })
        })
    }

    fn read(&self, _tx: &Transaction<'_>, _key: &str) -> Value {
        let (c, problem) = config::load_checked(&self.creds);
        let mut v = self.value(&c);
        if let Some(p) = problem {
            v["configProblem"] = json!(p.reason());
        }
        v
    }

    fn apply(
        &self,
        _tx: &Transaction<'_>,
        _key: &str,
        _value: &Value,
    ) -> Result<(Value, Value), String> {
        Err(READ_ONLY_ANSWER.into())
    }

    fn restore(&self, _tx: &Transaction<'_>, _key: &str, _value: &Value) -> Result<(), String> {
        Err(READ_ONLY_ANSWER.into())
    }

    fn changed_event(&self, _key: &str) -> AppEvent {
        AppEvent::new("relay_connection")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use helm_platform::keychain::MemoryKeychain;

    const TOKEN: &str = "hub-view-secret-token-0123456789abcdef";
    const LAN: &str = "ws://192.168.7.21:8000/relay";
    const FALLBACK: &str = "ws://100.64.7.21:8000/relay";

    fn view(
        dir: &std::path::Path,
        cfg: Option<RelayConfig>,
        state: &'static str,
        memory: bool,
    ) -> HubView {
        let creds = CredentialStore::new(
            &dir.join("credentials"),
            "h",
            Arc::new(MemoryKeychain::default()),
        );
        if let Some(c) = cfg {
            config::save_paired(&creds, &c).unwrap();
        }
        HubView {
            creds,
            line_state: Box::new(move || state),
            memory_server: Box::new(move || memory),
        }
    }

    fn read(v: &HubView) -> Value {
        let mut db = rusqlite::Connection::open_in_memory().unwrap();
        let tx = db.transaction().unwrap();
        v.read(&tx, KEY)
    }

    #[test]
    fn a_chat_sees_the_connection_but_never_the_address_the_token_or_an_id() {
        let paired = RelayConfig {
            lan_url: LAN.into(),
            fallback_url: FALLBACK.into(),
            token: TOKEN.into(),
            person_id: "7731".into(),
            person_name: "Pat".into(),
            machine_id: "9412".into(),
            machine_name: "Laptop".into(),
        };
        let typed = RelayConfig {
            person_id: String::new(),
            person_name: String::new(),
            machine_id: String::new(),
            machine_name: String::new(),
            fallback_url: String::new(),
            ..paired.clone()
        };
        let no_token = RelayConfig {
            token: String::new(),
            ..paired.clone()
        };
        // (stored connection, line state, memory server present, the value a chat reads)
        let rows = [
            (
                None,
                "offline",
                false,
                json!({"paired": false, "state": "off", "personName": "", "machineName": "",
                       "hasFallback": false, "memoryServer": false}),
            ),
            (
                Some(paired),
                "online",
                true,
                json!({"paired": true, "state": "online", "personName": "Pat",
                       "machineName": "Laptop", "hasFallback": true, "memoryServer": true}),
            ),
            (
                Some(typed),
                "connecting",
                false,
                json!({"paired": true, "state": "connecting", "personName": "",
                       "machineName": "", "hasFallback": false, "memoryServer": false}),
            ),
            (
                Some(no_token),
                "offline",
                false,
                json!({"paired": false, "state": "off", "personName": "Pat",
                       "machineName": "Laptop", "hasFallback": true, "memoryServer": false}),
            ),
        ];
        for (i, (cfg, state, memory, want)) in rows.into_iter().enumerate() {
            let dir = tempfile::tempdir().unwrap();
            let v = view(dir.path(), cfg, state, memory);
            let got = read(&v);
            assert_eq!(got, want, "row {i}");
            let text = got.to_string();
            for secret in [TOKEN, "192.168", "100.64", "7731", "9412", "8000"] {
                assert!(!text.contains(secret), "row {i} leaks {secret}: {text}");
            }
            let d = v.describe(KEY).unwrap().to_string();
            assert!(d.contains("read") && !d.contains(TOKEN), "{d}");
        }
    }

    #[test]
    fn it_is_read_only_and_names_the_way_to_pair() {
        let dir = tempfile::tempdir().unwrap();
        let v = view(dir.path(), None, "off", false);
        let mut db = rusqlite::Connection::open_in_memory().unwrap();
        let tx = db.transaction().unwrap();
        for e in [
            v.apply(&tx, KEY, &json!({"paired": true})).unwrap_err(),
            v.apply(&tx, KEY, &Value::Null).unwrap_err(),
            v.restore(&tx, KEY, &json!({})).unwrap_err(),
        ] {
            assert!(
                e.contains("app_stage_settings") && e.contains("hubPairing"),
                "{e}"
            );
        }
        assert_eq!(v.keys(), vec![KEY]);
        assert!(v.describe("other").is_none());
        // Names from the hub stay on one short line.
        let long = RelayConfig {
            lan_url: LAN.into(),
            token: TOKEN.into(),
            person_name: format!("Pat\n{}", "x".repeat(200)),
            ..Default::default()
        };
        let v = view(dir.path(), Some(long), "online", false);
        let name = read(&v)["personName"].as_str().unwrap().to_owned();
        assert!(
            name.chars().count() == MAX_NAME && !name.contains('\n'),
            "{name}"
        );
    }
}
