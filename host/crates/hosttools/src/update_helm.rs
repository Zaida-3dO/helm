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
use rusqlite::OptionalExtension;
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
            // `confirm` is only the agent's word that the user asked. The say-so itself must be
            // the owner's own send in this chat: a page, another chat, a schedule or a speaker
            // can talk an agent into passing confirm, and must not restart Helm by proxy.
            owner_say_so(&ctx)?;
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

/// Whether the newest thing put into the calling chat is the owner's own say-so: typed here, typed
/// on another of their PCs, an answer, a send from a notification, their own Telegram or phone
/// voice (so they can update Helm while away from the PC), or another of their chats (an agent
/// chain they started, which could run the setup by hand anyway). Helm's own `app` notes are
/// skipped, so a later system note never hides what last came in. A page, a room speaker anyone
/// can talk to, a schedule, an action, or nothing at all, is no say-so to restart the app.
fn owner_say_so(ctx: &ToolCtx) -> Result<(), ToolError> {
    const REFUSE: &str = "Ask the user to type that they want Helm updated in this chat, \
                          then call install again.";
    let Ok(db) = ctx.services.need::<helm_core::db::Database>() else {
        return Err(ToolError::refused(REFUSE));
    };
    let origin = db
        .snapshot(|tx| {
            tx.query_row(
                "SELECT m.origin FROM submissions s
                   JOIN submission_meta m ON m.submission_id = s.id
                  WHERE s.chat_id = ?1 AND m.origin != 'app'
                  ORDER BY s.created_at DESC, s.id DESC LIMIT 1",
                [&ctx.caller_chat_id],
                |r| r.get::<_, String>(0),
            )
            .optional()
        })
        .ok()
        .flatten();
    match origin.as_deref() {
        Some("typed") | Some("remote") | Some("answer") | Some("notification")
        | Some("telegram") | Some("voice") | Some("other_chat") => Ok(()),
        _ => Err(ToolError::refused(REFUSE)),
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
             Only when the user explicitly asked for the update. The chat's newest non-system message must be your person's own say-so (typed here or on another of their PCs, an answer, a notification, their Telegram or phone voice, or another of their chats; a page, a room speaker or a schedule is refused): if install is refused, ask them to type the request in this chat, then call it again. Never run a setup file yourself: this tool is the only path that drains the chats and leaves the note that brings Helm back.",
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

    async fn call_with(
        fake: &Arc<Fake>,
        args: Value,
        services: Arc<helm_ipc::Services>,
    ) -> ToolResult {
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
                services,
                events: Arc::new(helm_ipc::NullSink),
                ops: Some(fake.clone()),
            },
            args,
        )
        .await
    }

    fn plain() -> Arc<helm_ipc::Services> {
        Arc::new(helm_ipc::Services::new())
    }

    /// A chat "me" whose sends, oldest first, carry these origins; the caller keeps the folder
    /// alive while the database is used. The core migrations carry the chats and submissions
    /// tables; the origin column lives in the chats crate's table, made here in the shape the
    /// gate reads.
    fn owner_db(origins: &[&str]) -> (tempfile::TempDir, Arc<helm_ipc::Services>) {
        let dir = tempfile::tempdir().unwrap();
        let mut reg = helm_core::db::MigrationRegistry::new();
        reg.initializer("test-update-owner", |tx| {
            tx.execute_batch(
                "CREATE TABLE submission_meta (
                     submission_id TEXT PRIMARY KEY REFERENCES submissions(id) ON DELETE CASCADE,
                     origin TEXT NOT NULL);",
            )
        });
        let db = std::sync::Arc::new(
            helm_core::db::Database::open(&dir.path().join("t.db"), &reg).unwrap(),
        );
        for (n, origin) in origins.iter().enumerate() {
            let id = format!("s{n}");
            db.write(|tx| {
                tx.execute(
                    "INSERT OR IGNORE INTO projects (id, name, path, created_at) VALUES ('p', 'p', 'C:/p', 0)",
                    [],
                )?;
                tx.execute(
                    "INSERT OR IGNORE INTO chats (id, project_id, created_at, updated_at) VALUES ('me', 'p', 0, 0)",
                    [],
                )?;
                tx.execute(
                    "INSERT INTO submissions(id, chat_id, text, state, created_at) VALUES (?1, 'me', 'words', 'sent', ?2)",
                    rusqlite::params![id, n as i64],
                )?;
                tx.execute(
                    "INSERT INTO submission_meta(submission_id, origin) VALUES (?1, ?2)",
                    rusqlite::params![id, *origin],
                )
            })
            .unwrap();
        }
        let mut services = helm_ipc::Services::new();
        services.insert(db);
        (dir, Arc::new(services))
    }

    #[tokio::test]
    async fn check_reports_the_folder_and_install_needs_confirm_and_goes_through_the_ops() {
        let fake = Arc::new(Fake(Mutex::new(vec![])));

        // check: the folder's answer reaches the agent whole.
        let r = call_with(&fake, json!({"action": "check"}), plain())
            .await
            .unwrap();
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
            let e = call_with(&fake, args.clone(), plain()).await.unwrap_err();
            assert!(
                e.public.contains("confirm") || e.public.contains("check or install"),
                "{args}: {}",
                e.public
            );
            assert_eq!(e.changed, crate::Changed::No);
        }
        assert_eq!(fake.0.lock().unwrap().len(), calls_before, "nothing ran");

        // confirm is the agent's word; the owner's own send is what the restart needs. Only the
        // last word in the chat counts (Helm's own notes are skipped), and an empty chat with no
        // person's words behind it is refused as firmly as a page's or another bot's.
        // (why, origins oldest..newest, allowed?)
        let say_so: &[(&str, &[&str], bool)] = &[
            ("the person typed the ask", &["typed"], true),
            ("typed on another of their PCs", &["page", "remote"], true),
            ("an Allow-card answer was last", &["typed", "answer"], true),
            ("a quick reply from a toast", &["notification"], true),
            ("a page wrote last", &["typed", "page"], false),
            (
                "another of their chats asked",
                &["typed", "other_chat"],
                true,
            ),
            ("a speaker turn came in last", &["typed", "speaker"], false),
            ("a schedule wrote last", &["schedule"], false),
            (
                "they asked on Telegram, away from the PC",
                &["telegram"],
                true,
            ),
            ("they asked by phone voice", &["voice"], true),
            ("an action button wrote last", &["typed", "action"], false),
            (
                "a Helm note after a page's words does not hide them",
                &["page", "app"],
                false,
            ),
            ("no words in the chat at all", &[], false),
        ];
        for (why, origins, allowed) in say_so {
            let (_dir, services) = owner_db(origins);
            let gate_fake = Arc::new(Fake(Mutex::new(vec![])));
            let out = call_with(
                &gate_fake,
                json!({"action": "install", "confirm": true}),
                services,
            )
            .await;
            if *allowed {
                assert!(out.is_ok(), "{why}: {:?}", out.err().map(|e| e.public));
            } else {
                let e = out.unwrap_err();
                assert!(
                    e.public.contains("Ask the user to type"),
                    "{why}: {}",
                    e.public
                );
                assert_eq!(e.changed, crate::Changed::No);
                assert!(
                    gate_fake.0.lock().unwrap().is_empty(),
                    "{why}: nothing may run without the person's words"
                );
            }
        }

        // install with confirm over the person's own last words: the folder is re-checked, then
        // the app quits into the setup.
        let (_dir, services) = owner_db(&["typed"]);
        let r = call_with(
            &fake,
            json!({"action": "install", "confirm": true}),
            services,
        )
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
        let (_dir, services) = owner_db(&["typed"]);
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
                    services,
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
