//! Server presets. A preset is a starting point for the Settings form:
//! a name, the transport, the auth shape and the guidance the agent gets. None carries an
//! address or a token: the repo is public, and the user enters both in Settings → MCP.
//!
//! Gmail is not a preset: it arrives natively through the claude.ai connectors.

use serde::Serialize;
use ts_rs::TS;

#[derive(Clone, Debug, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct McpPreset {
    /// Becomes the server's id and its `preset` field.
    pub id: String,
    pub name: String,
    pub description: String,
    /// Always `http` for the built-in presets.
    pub transport: crate::types::TransportKind,
    /// A hint for the address field, never a real address.
    pub url_placeholder: String,
    /// The header that carries the token (`Authorization`), with its prefix.
    pub token_header: String,
    pub token_prefix: String,
    /// The server is usually plain http on the private network, so the form shows the
    /// "Allow unencrypted on my network" box (still unticked).
    pub suggest_private_http: bool,
    /// Test connection passes when the server lists all of these, whatever the total.
    pub expect_tools: Vec<String>,
    /// Given to the agent with this server's discovered tools.
    pub guidance: String,
}

fn preset(
    id: &str,
    name: &str,
    description: &str,
    placeholder: &str,
    private: bool,
    expect: &[&str],
    guidance: &str,
) -> McpPreset {
    McpPreset {
        id: id.into(),
        name: name.into(),
        description: description.into(),
        transport: crate::types::TransportKind::Http,
        url_placeholder: placeholder.into(),
        token_header: "Authorization".into(),
        token_prefix: "Bearer ".into(),
        suggest_private_http: private,
        expect_tools: expect.iter().map(|s| s.to_string()).collect(),
        guidance: guidance.into(),
    }
}

pub fn presets() -> Vec<McpPreset> {
    vec![
        preset(
            "agent_standup",
            "Agent Standup",
            "The task board: read items, record notes and checkpoints.",
            "https://your-board.example/mcp",
            false,
            &["get_item", "record", "ownership"],
            "Agent Standup is a live service. Read freely; write (record, transition, claim) only when the user asked for it.",
        ),
        preset(
            "home_assistant",
            "Home Assistant",
            "Home Assistant's MCP server: entities, automations and chores.",
            "http://homeassistant.local:8123/mcp",
            true,
            &[],
            "Home Assistant controls a real home. Its write tools are dry runs unless you pass act=true; pass it only on the user's explicit request, after showing the dry run.",
        ),
        preset(
            "helm_hub",
            "Helm hub memory",
            "Shared memory on the Helm hub: keyword search over saved memories.",
            "http://your-hub.example/mcp",
            true,
            &["memory_search"],
            "The hub's memory search is keyword-only (no vectors in v1): search with the distinctive words a memory would contain.",
        ),
    ]
}

/// The guidance for a server created from `preset`, if any.
pub fn guidance_for(preset: Option<&str>) -> Option<String> {
    let id = preset?;
    presets()
        .into_iter()
        .find(|p| p.id == id)
        .map(|p| p.guidance)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn presets_carry_no_address_or_token_and_no_gmail() {
        let all = presets();
        assert_eq!(
            all.iter().map(|p| p.id.as_str()).collect::<Vec<_>>(),
            ["agent_standup", "home_assistant", "helm_hub"]
        );
        for p in &all {
            assert!(crate::validate::validate_id(&p.id).is_ok());
            let text = serde_json::to_string(p).unwrap();
            assert!(!text.to_lowercase().contains("gmail"));
            // Placeholders use reserved example names, never a real host or IP.
            assert!(
                p.url_placeholder.contains("example")
                    || p.url_placeholder.contains("your-")
                    || p.url_placeholder.contains(".local"),
                "{}",
                p.url_placeholder
            );
            let host = url::Url::parse(&p.url_placeholder).unwrap();
            assert!(
                host.host_str()
                    .unwrap()
                    .parse::<std::net::IpAddr>()
                    .is_err(),
                "no IP address in a preset"
            );
        }
        assert_eq!(all[0].expect_tools, ["get_item", "record", "ownership"]);
        assert!(guidance_for(Some("home_assistant"))
            .unwrap()
            .contains("act=true"));
        assert!(guidance_for(Some("nope")).is_none());
        assert!(guidance_for(None).is_none());
    }
}
