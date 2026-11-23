//! `app_update`: keep Helm itself up to date — check the update folder, or restart into the
//! newer setup file the way the "Restart to update" pill does.
//!
//! The tool never runs a setup file itself: it goes through the update ops, which re-check the
//! folder the user chose, drain the working chats with drafts saved, leave the pending-update
//! note (so the always-on launcher starts the installed Helm again once the setup ends, even if
//! the setup fails) and exit. An agent running the setup by hand skips all of that, so the
//! guidelines say plainly that it must not.

use crate::registry::{HostToolRegistry, ToolCtx, ToolError, ToolResult};
use crate::spec::ToolSpec;
use serde_json::{json, Value};

async fn update(ctx: ToolCtx, a: Value) -> ToolResult {
    let action = a.get("action").and_then(Value::as_str).unwrap_or("");
    match action {
        "check" => {
            let st = ctx.op("update_check", json!({})).await?;
            Ok(json!({
                "current": st["current"],
                "source": st["source"],
                "available": st["available"],
                "checkedAt": st["checkedAt"],
                "error": st["error"],
            }))
        }
        "install" => {
            if a.get("confirm").and_then(Value::as_bool) != Some(true) {
                return Err(ToolError::refused(
                    "install quits Helm (draining the chats that work) and runs the update: \
                     pass confirm true only when the user asked for the update.",
                ));
            }
            // Re-check the folder as late as possible and see what the restart would stop; an
            // absent or unreadable setup refuses here, before anything is drained.
            let prep = ctx.op("update_prepare", json!({})).await?;
            let out = ctx.op("update_install", json!({})).await?;
            Ok(json!({
                "outcome": out["outcome"],
                "version": prep["version"],
                "busy": prep["busy"],
                "busyTotal": prep["busyTotal"],
                "note": "Helm restarts into the update and comes back by itself once the setup \
                         ends (even if it fails). Your own worker is drained, so this reply may \
                         not arrive: do not retry the install, ask app_update check afterwards.",
            }))
        }
        _ => Err(ToolError::refused("action is check or install.")),
    }
}

pub fn register_host_tools(r: &mut HostToolRegistry) {
    r.async_tool(
        ToolSpec::write(
            "app_update",
            "Keep Helm itself up to date: check what the update folder offers against the running version (check), or save drafts, drain the working chats and run the newer setup file, exactly the path the \"Restart to update\" button takes (install).",
            json!({
                "type": "object",
                "properties": {
                    "action": {"type": "string", "enum": ["check", "install"]},
                    "confirm": {
                        "type": "boolean",
                        "description": "With install: true when the user asked for the update."
                    }
                },
                "required": ["action"],
                "additionalProperties": false
            }),
        )
        .self_locking()
        .timeout_ms(60_000)
        .guidelines(
            "check: no arguments; the update folder, the running version, the newer setup found (if any) and any scan error.\n\
             install: Helm quits with drafts saved, the setup file runs, and Helm starts again by itself once that process ends — even if the setup failed. Your own turn ends with the app, so a lost reply is expected: never retry the install, run check afterwards instead.\n\
             Only when the user explicitly asked for the update. Never run a setup file yourself: this tool is the only path that drains the chats and leaves the note that brings Helm back.",
        ),
        update,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::{CallOrigin, OpInvoker, ToolHandler};
    use std::sync::{Arc, Mutex};

    struct Fake(Mutex<Vec<(String, Value)>>);
    impl OpInvoker for Fake {
        fn invoke<'a>(
            &'a self,
            op: &'a str,
            args: Value,
        ) -> std::pin::Pin<
            Box<dyn std::future::Future<Output = Result<Value, helm_ipc::ApiError>> + Send + 'a>,
        > {
            self.0.lock().unwrap().push((op.to_owned(), args.clone()));
            let out = match op {
                "update_check" => Ok(json!({
                    "current": "2.4.4",
                    "source": "C:\\w\\dist",
                    "available": {"version": "2.4.5", "fileName": "Helm_2.4.5_x64-setup.exe"},
                    "checkedAt": 123,
                    "error": null
                })),
                "update_prepare" => {
                    Ok(json!({"version": "2.4.5", "busy": ["chat one (C:/p)"], "busyTotal": 1}))
                }
                "update_install" => Ok(json!({"outcome": "restarting"})),
                _ => Err(helm_ipc::ApiError::conflict(
                    "There is no newer setup file in the folder.",
                )),
            };
            Box::pin(async move { out })
        }
    }

    async fn call(fake: &Arc<Fake>, args: Value) -> ToolResult {
        let mut r = HostToolRegistry::new();
        register_host_tools(&mut r);
        let ToolHandler::Async(f) = r.get("app_update").unwrap().handler.clone() else {
            panic!()
        };
        f(
            ToolCtx {
                caller_chat_id: "me".into(),
                is_child: false,
                tool_call_id: Some("tc1".into()),
                origin: CallOrigin::Agent,
                services: Arc::new(helm_ipc::Services::new()),
                events: Arc::new(helm_ipc::NullSink),
                ops: Some(fake.clone()),
            },
            args,
        )
        .await
    }

    #[tokio::test]
    async fn check_reports_the_folder_and_install_needs_confirm_and_goes_through_the_ops() {
        let fake = Arc::new(Fake(Mutex::new(vec![])));

        // check: the folder's answer reaches the agent whole.
        let r = call(&fake, json!({"action": "check"})).await.unwrap();
        assert_eq!(r["current"], "2.4.4");
        assert_eq!(r["available"]["version"], "2.4.5");
        assert_eq!(fake.0.lock().unwrap().last().unwrap().0, "update_check");

        // install without confirm: refused before any op runs.
        let calls_before = fake.0.lock().unwrap().len();
        for args in [
            json!({"action": "install"}),
            json!({"action": "install", "confirm": false}),
            json!({"action": "restart"}),
            json!({}),
        ] {
            let e = call(&fake, args.clone()).await.unwrap_err();
            assert!(
                e.public.contains("confirm") || e.public.contains("check or install"),
                "{args}: {}",
                e.public
            );
            assert_eq!(e.changed, crate::Changed::No);
        }
        assert_eq!(fake.0.lock().unwrap().len(), calls_before, "nothing ran");

        // install with confirm: the folder is re-checked, then the app quits into the setup.
        let r = call(&fake, json!({"action": "install", "confirm": true}))
            .await
            .unwrap();
        assert_eq!(r["outcome"], "restarting");
        assert_eq!(r["version"], "2.4.5");
        assert_eq!(r["busyTotal"], 1);
        let ops: Vec<String> = {
            let guard = fake.0.lock().unwrap();
            guard
                .iter()
                .skip(calls_before)
                .map(|(op, _)| op.clone())
                .collect()
        };
        assert_eq!(ops, ["update_prepare", "update_install"]);

        // The ops' own refusal (no newer setup) reaches the agent: install never ran, because
        // the prepare refused first.
        let empty = Arc::new(NoUpdate(Mutex::new(vec![])));
        let e = {
            let mut reg = HostToolRegistry::new();
            register_host_tools(&mut reg);
            let ToolHandler::Async(f) = reg.get("app_update").unwrap().handler.clone() else {
                panic!()
            };
            f(
                ToolCtx {
                    caller_chat_id: "me".into(),
                    is_child: false,
                    tool_call_id: None,
                    origin: CallOrigin::Agent,
                    services: Arc::new(helm_ipc::Services::new()),
                    events: Arc::new(helm_ipc::NullSink),
                    ops: Some(empty.clone()),
                },
                json!({"action": "install", "confirm": true}),
            )
            .await
            .unwrap_err()
        };
        assert!(e.public.contains("no newer setup"), "{}", e.public);
        assert_eq!(
            empty
                .0
                .lock()
                .unwrap()
                .iter()
                .map(|(op, _)| op.as_str())
                .collect::<Vec<_>>(),
            ["update_prepare"],
            "the failing prepare stopped there: nothing was drained"
        );
    }

    struct NoUpdate(Mutex<Vec<(String, Value)>>);
    impl OpInvoker for NoUpdate {
        fn invoke<'a>(
            &'a self,
            op: &'a str,
            args: Value,
        ) -> std::pin::Pin<
            Box<dyn std::future::Future<Output = Result<Value, helm_ipc::ApiError>> + Send + 'a>,
        > {
            self.0.lock().unwrap().push((op.to_owned(), args.clone()));
            Box::pin(async move {
                Err(helm_ipc::ApiError::conflict(
                    "There is no newer setup file in the folder.",
                ))
            })
        }
    }
}
