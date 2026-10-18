//! The `app_orchestrator_on` host tool and the `workflow` prompt section.
//!
//! **Note:** the host-tool catalogue has no per-chat launch condition, so the tool is
//! listed in every root chat (one append-only line in the tool-surface baseline) and its handler
//! refuses unless this chat is on Auto under an enabled master switch. Sub-agent calls are refused
//! at the handler, which is where the worker-side attribution check lands.

use crate::mode::{self, Changed};
use crate::types::*;
use crate::{notes, rules, settings};
use helm_core::db::Database;
use helm_hosttools::{AddendumSection, CallOrigin, HostToolRegistry, ToolCtx, ToolError, ToolSpec};
use helm_ipc::AppEvent;
use helm_workers::contrib::LaunchCtx;
use serde_json::{json, Value};
use std::sync::Arc;

pub const DESCRIPTION: &str = "Turn Orchestrator on for THIS chat, when it is on Auto and the user's request should run under the orchestrator workflow (a planned, reviewed, multi-step code change). reason: one plain sentence (1-500 characters) saying why. It takes effect from your next turn, and the chat shows your reason. Only the chat's own agent may call it, only while the chat is on Auto; it can never turn Orchestrator off.";

pub fn schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "reason": {"type": "string", "description": "One plain sentence, 1-500 characters."}
        },
        "required": ["reason"],
        "additionalProperties": false
    })
}

/// The tool's checks and write, without delivery: `(change, chat busy, settings)`.
pub fn turn_on(
    db: &Database,
    chat_id: &str,
    is_child: bool,
    origin: CallOrigin,
    args: &Value,
    now: i64,
) -> Result<(Changed, bool, WorkflowSettings), ToolError> {
    if is_child || origin == CallOrigin::Subagent {
        return Err(ToolError::refused(
            "Only the chat's own agent can turn Orchestrator on, not a sub-agent.",
        ));
    }
    let reason = args.get("reason").and_then(Value::as_str).ok_or_else(|| {
        ToolError::refused("reason must be one plain sentence of 1-500 characters")
    })?;
    db.write(|tx| {
        let s = settings::load(tx)?;
        let busy = mode::facts(tx, chat_id)?.is_some_and(|f| f.busy);
        Ok::<_, rusqlite::Error>(
            mode::agent_on(tx, chat_id, reason, &s, now)?.map(|c| (c, busy, s)),
        )
    })
    .map_err(|e| ToolError::maybe(e.to_string()))?
    .map_err(ToolError::refused)
}

pub fn register_host_tools(r: &mut HostToolRegistry) {
    r.blocking(
        ToolSpec::write("app_orchestrator_on", DESCRIPTION, schema()),
        |ctx: &ToolCtx, args| {
            let db = ctx
                .services
                .get::<Database>()
                .ok_or_else(|| ToolError::refused("Orchestrator is not available here."))?;
            let now = helm_core::now_ms();
            let chat = ctx.caller_chat_id.clone();
            let (c, busy, s) = turn_on(&db, &chat, ctx.is_child, ctx.origin, &args, now)?;
            if let Some(rt) = ctx.services.get::<helm_chats::ChatRuntime>() {
                notes::deliver(&rt, &chat, notes::plan(&chat, &c, busy, true, &s, now));
            }
            ctx.events
                .emit(AppEvent::new("workflow_chat_mode").with("chatId", json!(chat)));
            Ok(json!({"on": true, "appliesFrom": "next turn", "row": c.row}))
        },
    );
    crate::wftools::register_host_tools(r);
}

/// The `workflow` prompt section (order 90, after every other section). Pins the chat's state at
/// its first launch while the master switch is on. Adds the Workflow rules when the workflow is in
/// effect, the Auto sentence on an Auto chat, and nothing otherwise, so a launch with the workflow
/// off is byte-identical to one without this section.
pub struct WorkflowSection {
    pub db: Arc<Database>,
}

pub const SECTION_ORDER: i32 = 90;

pub fn section_text(db: &Database, chat_id: &str, now: i64) -> Option<String> {
    db.write(|tx| {
        let s = settings::load(tx)?;
        let Some(f) = mode::facts(tx, chat_id)? else {
            return Ok(None);
        };
        if mode::unavailable(&s, &f).is_some() {
            return Ok(None);
        }
        Ok::<_, rusqlite::Error>(match mode::pin(tx, chat_id, &s, now)? {
            ChatMode::On => Some(rules::workflow_rules(&s)),
            ChatMode::Auto => Some(rules::AUTO.to_owned()),
            ChatMode::Off => None,
        })
    })
    .map_err(|e| log::warn!("workflow section for {chat_id}: {e}"))
    .ok()
    .flatten()
}

impl AddendumSection for WorkflowSection {
    fn id(&self) -> &'static str {
        "workflow"
    }
    fn order(&self) -> i32 {
        SECTION_ORDER
    }
    fn text(&self, ctx: &LaunchCtx<'_>) -> Option<String> {
        section_text(&self.db, ctx.chat_id, helm_core::now_ms())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mode::tests::db;

    fn enable(db: &Database, f: impl FnOnce(&mut WorkflowSettings)) {
        db.write::<_, rusqlite::Error>(|tx| {
            let mut s = WorkflowSettings {
                enabled: true,
                ..Default::default()
            };
            f(&mut s);
            settings::store(tx, &s, 1)
        })
        .unwrap();
    }

    #[test]
    fn the_section_is_absent_while_the_workflow_is_off() {
        let (_d, db) = db(&[("a", "claude"), ("x", "codex")]);
        assert_eq!(section_text(&db, "a", 1), None, "master switch off");
        enable(&db, |_| {});
        assert!(section_text(&db, "a", 2)
            .unwrap()
            .starts_with(rules::HEADING));
        assert_eq!(section_text(&db, "x", 2), None, "not a Claude Code chat");
        db.write::<_, rusqlite::Error>(|tx| {
            mode::user_set(tx, "a", ChatMode::Off, &settings::load(tx)?, 3)?.unwrap();
            Ok(())
        })
        .unwrap();
        assert_eq!(section_text(&db, "a", 4), None, "chat off");
    }

    #[test]
    fn an_auto_chat_gets_the_auto_sentence_and_its_first_launch_pins_it() {
        let (_d, db) = db(&[("a", "claude")]);
        enable(&db, |s| s.new_chats = NewChats::Auto);
        assert_eq!(section_text(&db, "a", 1).as_deref(), Some(rules::AUTO));
        enable(&db, |s| s.new_chats = NewChats::Always);
        assert_eq!(
            section_text(&db, "a", 2).as_deref(),
            Some(rules::AUTO),
            "a used chat keeps its pinned state"
        );
    }

    #[test]
    fn the_tool_refuses_sub_agents_and_non_auto_chats_and_writes_on_for_auto() {
        let (_d, db) = db(&[("a", "claude")]);
        let args = json!({"reason": "This needs three PRs."});
        enable(&db, |s| s.new_chats = NewChats::Auto);
        let e = turn_on(&db, "a", true, CallOrigin::Subagent, &args, 1).unwrap_err();
        assert!(e.text().contains("sub-agent"));
        let e = turn_on(&db, "a", false, CallOrigin::Agent, &json!({}), 1).unwrap_err();
        assert!(e.text().contains("reason"));
        let (c, busy, _) = turn_on(&db, "a", false, CallOrigin::Agent, &args, 2).unwrap();
        assert_eq!(c.to, ChatMode::On);
        assert!(!busy);
        let e = turn_on(&db, "a", false, CallOrigin::Agent, &args, 3).unwrap_err();
        assert!(e.text().contains("already on"));
        assert!(section_text(&db, "a", 4)
            .unwrap()
            .starts_with(rules::HEADING));
    }

    #[test]
    fn the_tool_registers_under_its_reserved_name() {
        let mut r = HostToolRegistry::new();
        register_host_tools(&mut r);
        assert!(r.contains("app_orchestrator_on"));
        assert!(
            !r.get("app_orchestrator_on")
                .unwrap()
                .spec
                .availability
                .children
        );
    }
}
