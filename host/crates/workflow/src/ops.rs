//! The workflow `api` ops.
//!
//! | op | class | args to result |
//! |---|---|---|
//! | `workflow_settings_get` | blocking | `{}` to [`WorkflowSettings`] |
//! | `workflow_settings_save` | maintenance | `{settings}` to [`WorkflowSettings`]; emits `workflow_settings {}` |
//! | `workflow_chat_mode_get` | blocking | [`ChatModeArgs`] to [`ChatModeView`] |
//! | `workflow_chat_mode_set` | maintenance | [`ChatModeSetArgs`] to [`ChatModeSetResult`]; emits `workflow_chat_mode {chatId}` |
//! | `workflow_run_get` | blocking | [`ChatModeArgs`] to the chat's current task in the `app_workflow status` shape ([`crate::actions::chat_view`]); `not_found` for no such chat |
//! | `workflow_change_action` | maintenance | `{runId, repo, branch, action}` with action merge, retry, drop or decline (the user's, main window only) to `{state}` |
//! | `workflow_change_ready` | blocking | `{runId, repo, branch}`: Ready for review on a handed-off change (the user's, main window only) to `{state}`. It asks GitHub, so it holds no global lock ([`crate::land::ready`]) |
//! | `workflow_task_action` | maintenance | `{runId, action, planId?, stage?, n?}` with action signOff, skip, allowMore, park, reopen, visual, notVisual or end (the user's, main window only); see [`crate::user`] |
//!
//! The user's ops are never host tools, so no agent reaches them; agents take the consented
//! subset through `app_workflow` with the user's words or an Allow grant.
//!
//! Every op, tool and land pass that writes a task, change or event row emits
//! `workflow_run {chatId}` for each chat it wrote ([`crate::store::emit_touched`]).

use crate::types::*;
use crate::{mode, settings};
use helm_core::db::Database;
use helm_ipc::{ApiError, AppEvent, OpClass, OpCtx, OpRegistry};
use rusqlite::{OptionalExtension, Transaction};
use serde::Deserialize;
use serde_json::{json, Value};

pub const TOGGLE_WHILE_RUNNING: &str =
    "A Claude Code chat is running a turn; turn Orchestrator on or off once it has finished.";

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SaveArgs {
    settings: Value,
}

fn parse<T: for<'de> Deserialize<'de>>(args: Value) -> Result<T, ApiError> {
    serde_json::from_value(args).map_err(|e| ApiError::bad_args(e.to_string()))
}

fn sql(e: rusqlite::Error) -> ApiError {
    ApiError::internal(e.to_string())
}

fn to_value<T: serde::Serialize>(v: &T) -> Result<Value, ApiError> {
    serde_json::to_value(v).map_err(|e| ApiError::internal(e.to_string()))
}

pub fn register_ops(reg: &mut OpRegistry) {
    reg.blocking("workflow_settings_get", OpClass::Blocking, settings_get)
        .blocking(
            "workflow_settings_save",
            OpClass::Maintenance,
            settings_save,
        )
        .blocking("workflow_chat_mode_get", OpClass::Blocking, chat_mode_get)
        .blocking(
            "workflow_chat_mode_set",
            OpClass::Maintenance,
            chat_mode_set,
        )
        .blocking("workflow_run_get", OpClass::Blocking, run_get)
        .blocking(
            "workflow_change_action",
            OpClass::Maintenance,
            change_action,
        )
        .blocking("workflow_change_ready", OpClass::Blocking, change_ready)
        .blocking("workflow_task_action", OpClass::Maintenance, task_action);
}

/// A Claude Code chat whose turn is starting, running or stopping.
pub fn any_claude_turn(tx: &Transaction<'_>) -> rusqlite::Result<bool> {
    Ok(tx
        .query_row(
            "SELECT 1 FROM chats WHERE harness = 'claude'
               AND status IN ('starting', 'running', 'stopping') LIMIT 1",
            [],
            |_| Ok(()),
        )
        .optional()?
        .is_some())
}

fn settings_get(ctx: &OpCtx, _args: Value) -> Result<Value, ApiError> {
    let db = ctx.services.need::<Database>()?;
    to_value(&db.snapshot(settings::load).map_err(sql)?)
}

fn settings_save(ctx: &OpCtx, args: Value) -> Result<Value, ApiError> {
    let a: SaveArgs = parse(args)?;
    let next = settings::validate(a.settings).map_err(ApiError::bad_args)?;
    let db = ctx.services.need::<Database>()?;
    let saved = db
        .write(|tx| {
            let prev = settings::load(tx)?;
            if prev.enabled != next.enabled && any_claude_turn(tx)? {
                return Ok(Err(ApiError::conflict(TOGGLE_WHILE_RUNNING)));
            }
            settings::store(tx, &next, helm_core::now_ms())?;
            Ok::<_, rusqlite::Error>(Ok(next))
        })
        .map_err(sql)??;
    ctx.events.emit(AppEvent::new("workflow_settings"));
    to_value(&saved)
}

fn chat_mode_get(ctx: &OpCtx, args: Value) -> Result<Value, ApiError> {
    let a: ChatModeArgs = parse(args)?;
    let db = ctx.services.need::<Database>()?;
    let v = db
        .snapshot(|tx| {
            let s = settings::load(tx)?;
            mode::view(tx, &a.chat_id, &s)
        })
        .map_err(sql)?
        .ok_or_else(|| ApiError::new("not_found", "No such chat."))?;
    to_value(&v)
}

fn chat_mode_set(ctx: &OpCtx, args: Value) -> Result<Value, ApiError> {
    let a: ChatModeSetArgs = parse(args)?;
    let db = ctx.services.need::<Database>()?;
    let now = helm_core::now_ms();
    let out = db.write(|tx| {
        let s = settings::load(tx)?;
        let changed = match mode::user_set(tx, &a.chat_id, a.mode, &s, now)? {
            Ok(c) => c,
            Err(e) => return Ok(Err(ApiError::conflict(e))),
        };
        if changed.as_ref().is_some_and(|c| c.to != ChatMode::On) {
            crate::engine::park_for_off(tx, &a.chat_id, now)?;
        }
        let view = mode::view(tx, &a.chat_id, &s)?.expect("the chat exists");
        Ok::<_, rusqlite::Error>(Ok((view, changed, s)))
    });
    // Turning a chat off parks its task: that is a run write.
    crate::store::emit_touched(&*ctx.events);
    let out = out.map_err(sql)??;
    let (view, changed, s) = out;
    if let Some(c) = &changed {
        if let Some(rt) = ctx.services.get::<helm_chats::ChatRuntime>() {
            let d = crate::notes::plan(&a.chat_id, c, view.running, false, &s, now);
            crate::notes::deliver(&rt, &a.chat_id, d);
        }
        ctx.events
            .emit(AppEvent::new("workflow_chat_mode").with("chatId", json!(a.chat_id)));
    }
    to_value(&ChatModeSetResult {
        view,
        row: changed.map(|c| c.row),
    })
}

/// The chat's current task for the workflow panel. Readable whatever the chat's Orchestrator state,
/// so a task parked by turning the chat off still shows (and can be reopened or ended).
fn run_get(ctx: &OpCtx, args: Value) -> Result<Value, ApiError> {
    let a: ChatModeArgs = parse(args)?;
    let db = ctx.services.need::<Database>()?;
    db.snapshot(|tx| {
        if mode::facts(tx, &a.chat_id)?.is_none() {
            return Ok(None);
        }
        let s = settings::load(tx)?;
        crate::actions::chat_view(tx, &s, &a.chat_id).map(Some)
    })
    .map_err(sql)?
    .ok_or_else(|| ApiError::new("not_found", "No such chat."))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ChangeAction {
    run_id: String,
    repo: String,
    branch: String,
    action: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ReadyArgs {
    run_id: String,
    repo: String,
    branch: String,
}

/// The user's Ready for review on a handed-off change. It is not a maintenance op: it asks GitHub,
/// which can take a minute, and the global maintenance lock must not be held meanwhile. The state
/// is read first, GitHub is asked, and the result is applied in a write that re-reads the state.
fn change_ready(ctx: &OpCtx, args: Value) -> Result<Value, ApiError> {
    change_ready_with(ctx, args, &crate::gh::GhCli)
}

fn change_ready_with(ctx: &OpCtx, args: Value, gh: &dyn crate::gh::Gh) -> Result<Value, ApiError> {
    let a: ReadyArgs = parse(args)?;
    let db = ctx.services.need::<Database>()?;
    let out = crate::land::ready(&db, gh, &a.run_id, &a.repo, &a.branch, helm_core::now_ms());
    // A failed check still writes its row, so emit before looking at the outcome.
    crate::store::emit_touched(&*ctx.events);
    let state = out.map_err(sql)?.map_err(ApiError::conflict)?;
    Ok(json!({ "state": state }))
}

/// The user's Merge, Retry, Drop change or Decline on a task row. The land step does the rest.
fn change_action(ctx: &OpCtx, args: Value) -> Result<Value, ApiError> {
    let a: ChangeAction = parse(args)?;
    let db = ctx.services.need::<Database>()?;
    let now = helm_core::now_ms();
    if a.action == "ready" {
        return Err(ApiError::bad_args(
            "Ready for review has its own operation, workflow_change_ready.",
        ));
    }
    let out = db.write(|tx| {
        let s = settings::load(tx)?;
        if !s.enabled && matches!(a.action.as_str(), "merge" | "retry") {
            return Ok(Err(ApiError::conflict(
                "Orchestrator is off in Settings, so Helm lands nothing.",
            )));
        }
        // While the chat is off, its changes already handed off keep landing (Merge, Decline, a
        // merge Retry), but Drop change and a draft Retry wait for it to be on.
        if let Some(run) = crate::store::run(tx, &a.run_id)? {
            let draft_retry = a.action == "retry"
                && crate::store::change(tx, &a.run_id, &a.repo, &a.branch)?
                    .is_some_and(|c| c.failed_mode.as_deref() == Some("draft"));
            if (a.action == "drop" || draft_retry) && !mode::in_effect(tx, &run.chat_id, &s)? {
                return Ok(Err(ApiError::conflict(crate::user::CHAT_OFF)));
            }
        }
        let r = crate::land::user_action(tx, &a.run_id, &a.repo, &a.branch, &a.action, now)?;
        Ok::<_, rusqlite::Error>(r.map_err(ApiError::conflict))
    });
    // A refused move still writes its conflict row, so emit before looking at the outcome.
    crate::store::emit_touched(&*ctx.events);
    let state = out.map_err(sql)??;
    crate::land::kick();
    Ok(json!({ "state": state }))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct TaskAction {
    run_id: String,
    action: String,
    #[serde(default)]
    plan_id: Option<String>,
    #[serde(default)]
    stage: Option<String>,
    #[serde(default)]
    n: Option<i64>,
}

/// The user's Sign off plan and task menu actions ([`crate::user`]).
fn task_action(ctx: &OpCtx, args: Value) -> Result<Value, ApiError> {
    let a: TaskAction = parse(args)?;
    let db = ctx.services.need::<Database>()?;
    let kinds = ctx
        .services
        .get::<helm_consent::AllowKinds>()
        .unwrap_or_else(|| {
            let mut k = helm_consent::AllowKinds::new();
            crate::kinds::register_allow_kinds(&mut k);
            std::sync::Arc::new(k)
        });
    let fields = json!({"action": a.action, "planId": a.plan_id, "stage": a.stage, "n": a.n});
    let fields = match fields {
        Value::Object(m) => Value::Object(m.into_iter().filter(|(_, v)| !v.is_null()).collect()),
        v => v,
    };
    let out = db
        .write(|tx| crate::user::task_action(tx, &kinds, &a.run_id, &fields, helm_core::now_ms()));
    crate::store::emit_touched(&*ctx.events);
    let r = out.map_err(sql)?.map_err(ApiError::conflict)?;
    crate::land::kick();
    Ok(r)
}

#[cfg(test)]
mod tests {
    use super::*;
    use helm_ipc::{OpCtx, RecordingSink, Services};
    use std::sync::Arc;

    fn ctx(db: Database, sink: Arc<RecordingSink>) -> OpCtx {
        let mut s = Services::new();
        s.insert(Arc::new(db));
        OpCtx {
            services: Arc::new(s),
            events: sink,
        }
    }

    #[test]
    fn settings_save_validates_and_refuses_a_toggle_mid_turn() {
        let (_d, db) = crate::mode::tests::db(&[("a", "claude")]);
        let sink = Arc::new(RecordingSink::default());
        let c = ctx(db, sink.clone());
        let e = settings_save(&c, json!({"settings": {"bogus": true}})).unwrap_err();
        assert!(e.message.contains("bogus"), "{}", e.message);
        let got =
            settings_save(&c, json!({"settings": {"enabled": true, "moveFast": true}})).unwrap();
        assert_eq!(got["enabled"], true);
        assert_eq!(got["moveFast"], true);
        assert_eq!(settings_get(&c, json!({})).unwrap()["moveFast"], true);
        assert_eq!(sink.kinds(), ["workflow_settings"]);
        let db = c.services.need::<Database>().unwrap();
        db.write::<_, rusqlite::Error>(|tx| {
            tx.execute("UPDATE chats SET status = 'running'", [])?;
            Ok(())
        })
        .unwrap();
        // A gate-only change is fine mid-turn; the master switch is not.
        assert!(settings_save(&c, json!({"settings": {"enabled": true}})).is_ok());
        let e = settings_save(&c, json!({"settings": {"enabled": false}})).unwrap_err();
        assert_eq!(e.message, TOGGLE_WHILE_RUNNING);
        assert_eq!(settings_get(&c, json!({})).unwrap()["enabled"], true);
    }

    fn start_task(c: &OpCtx, chat: &str, title: &str) {
        let db = c.services.need::<Database>().unwrap();
        let mut k = helm_consent::AllowKinds::new();
        crate::kinds::register_allow_kinds(&mut k);
        crate::wftools::workflow_call(
            &db,
            &k,
            chat,
            false,
            &json!({"action": "start", "title": title}),
            5,
        )
        .unwrap();
    }

    fn run_events(sink: &RecordingSink) -> Vec<String> {
        sink.events
            .lock()
            .unwrap()
            .iter()
            .filter(|e| e.kind == crate::store::RUN_EVENT)
            .map(|e| {
                serde_json::to_value(e).unwrap()["chatId"]
                    .as_str()
                    .unwrap()
                    .to_owned()
            })
            .collect()
    }

    #[test]
    fn run_get_reads_the_current_task_whatever_the_chat_state() {
        let (_d, db) = crate::mode::tests::db(&[("a", "claude"), ("b", "claude")]);
        let sink = Arc::new(RecordingSink::default());
        let c = ctx(db, sink.clone());
        let none = run_get(&c, json!({"chatId": "a"})).unwrap();
        assert_eq!(
            none,
            json!({"task": null, "plan": null, "changes": [], "otherTasks": []})
        );
        assert_eq!(
            run_get(&c, json!({"chatId": "zz"})).unwrap_err().code,
            "not_found"
        );
        assert_eq!(
            run_get(&c, json!({"chatId": "a", "x": 1}))
                .unwrap_err()
                .code,
            "bad_args"
        );
        settings_save(
            &c,
            json!({"settings": {"enabled": true, "newChats": "always"}}),
        )
        .unwrap();
        start_task(&c, "a", "Add login");
        let v = run_get(&c, json!({"chatId": "a"})).unwrap();
        assert_eq!(v["task"]["title"], "Add login");
        assert_eq!(v["task"]["status"], "active");
        assert!(v["plan"]["missing"].as_str().unwrap().contains("plan"));
        assert_eq!(
            run_get(&c, json!({"chatId": "b"})).unwrap()["task"],
            Value::Null,
            "another chat's task is not this chat's"
        );
        // Turning the chat off parks the task; the panel still reads it, and the park is a run
        // write, so the UI hears about it.
        sink.events.lock().unwrap().clear();
        chat_mode_set(&c, json!({"chatId": "a", "mode": "off"})).unwrap();
        assert_eq!(run_events(&sink), ["a"]);
        let v = run_get(&c, json!({"chatId": "a"})).unwrap();
        assert_eq!(v["task"]["status"], "parked");
        assert_eq!(v["otherTasks"], json!([]));
    }

    #[test]
    fn a_change_action_announces_its_chat_and_a_refusal_announces_nothing() {
        let (_d, db) = crate::mode::tests::db(&[("a", "claude")]);
        let sink = Arc::new(RecordingSink::default());
        let c = ctx(db, sink.clone());
        settings_save(
            &c,
            json!({"settings": {"enabled": true, "newChats": "always"}}),
        )
        .unwrap();
        start_task(&c, "a", "T");
        let db = c.services.need::<Database>().unwrap();
        let run_id: String = db
            .snapshot(|tx| tx.query_row("SELECT id FROM workflow_runs", [], |r| r.get(0)))
            .unwrap();
        db.write::<_, rusqlite::Error>(|tx| {
            let ch = crate::store::Change {
                run_id: run_id.clone(),
                repo: "github.com/o/r".into(),
                branch: "b".into(),
                state: "open".into(),
                ..Default::default()
            };
            crate::store::upsert_change(tx, &ch, 6)
        })
        .unwrap();
        let _ = crate::store::take_touched();
        let act = |id: &str, action: &str| {
            change_action(
                &c,
                json!({"runId": id, "repo": "github.com/o/r", "branch": "b", "action": action}),
            )
        };
        assert!(act("nope", "drop")
            .unwrap_err()
            .message
            .contains("No such task"));
        assert_eq!(act(&run_id, "retry").unwrap_err().code, "conflict");
        assert!(run_events(&sink).is_empty(), "a refusal wrote nothing");
        assert_eq!(act(&run_id, "drop").unwrap(), json!({"state": "dropped"}));
        assert_eq!(run_events(&sink), ["a"]);
    }

    #[test]
    fn the_current_task_is_the_active_one_else_the_one_waiting_on_the_user() {
        let (_d, db) = crate::mode::tests::db(&[("a", "claude")]);
        let s = WorkflowSettings::default();
        let set = |id: &str, status: &str, at: i64| {
            db.write::<_, rusqlite::Error>(|tx| {
                crate::store::insert_run(tx, id, "a", id, None, at)?;
                let mut r = crate::store::run(tx, id)?.unwrap();
                r.status = status.into();
                crate::store::save_run(tx, &r, None, at)
            })
            .unwrap()
        };
        set("handed", "handed_off", 1);
        db.write::<_, rusqlite::Error>(|tx| {
            let c = crate::store::Change {
                run_id: "handed".into(),
                repo: "github.com/o/r".into(),
                branch: "b".into(),
                state: "awaiting_merge".into(),
                ..Default::default()
            };
            crate::store::upsert_change(tx, &c, 1)
        })
        .unwrap();
        set("parked", "parked", 2);
        let view = |db: &Database| {
            db.snapshot(|tx| crate::actions::chat_view(tx, &s, "a"))
                .unwrap()
        };
        let v = view(&db);
        assert_eq!(
            v["task"]["runId"], "handed",
            "the task waiting on a Merge, not the newer parked one"
        );
        assert_eq!(v["changes"][0]["state"], "awaiting_merge");
        assert_eq!(v["otherTasks"][0]["runId"], "parked");
        set("active", "active", 3);
        let v = view(&db);
        assert_eq!(v["task"]["runId"], "active");
        assert_eq!(v["otherTasks"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn ready_for_review_is_not_a_maintenance_op() {
        let mut reg = OpRegistry::new();
        register_ops(&mut reg);
        assert_eq!(
            reg.class_of("workflow_change_ready"),
            Some(OpClass::Blocking)
        );
        assert_eq!(
            reg.class_of("workflow_change_action"),
            Some(OpClass::Maintenance)
        );
        let (_d, db) = crate::mode::tests::db(&[("a", "claude")]);
        let c = ctx(db, Arc::new(RecordingSink::default()));
        let e = change_action(
            &c,
            json!({"runId": "r", "repo": "github.com/o/r", "branch": "b", "action": "ready"}),
        )
        .unwrap_err();
        assert_eq!(e.code, "bad_args");
        assert!(e.message.contains("workflow_change_ready"), "{}", e.message);
    }

    #[test]
    fn other_tasks_carry_the_changes_that_wait_on_the_user() {
        let (_d, db) = crate::mode::tests::db(&[("a", "claude")]);
        let s = WorkflowSettings::default();
        let put = |id: &str, status: &str, at: i64, change: Option<&str>| {
            db.write::<_, rusqlite::Error>(|tx| {
                crate::store::insert_run(tx, id, "a", id, None, at)?;
                let mut r = crate::store::run(tx, id)?.unwrap();
                r.status = status.into();
                crate::store::save_run(tx, &r, None, at)?;
                if let Some(state) = change {
                    let c = crate::store::Change {
                        run_id: id.into(),
                        repo: "github.com/o/r".into(),
                        branch: format!("{id}-b"),
                        state: state.into(),
                        ..Default::default()
                    };
                    crate::store::upsert_change(tx, &c, 1)?;
                }
                Ok(())
            })
            .unwrap();
        };
        // Nine other tasks: the one waiting on a Merge is the oldest, so a cut by age would lose it.
        put("waits", "handed_off", 2, Some("awaiting_merge"));
        for i in 0..9 {
            put(&format!("parked{i}"), "parked", 10 + i, Some("open"));
        }
        put("active", "active", 1, None);
        let v = db
            .snapshot(|tx| crate::actions::chat_view(tx, &s, "a"))
            .unwrap();
        assert_eq!(v["task"]["runId"], "active");
        let others = v["otherTasks"].as_array().unwrap();
        assert_eq!(others.len(), 8);
        assert_eq!(others[0]["runId"], "waits", "waiting tasks come first");
        assert_eq!(others[0]["waiting"][0]["state"], "awaiting_merge");
        assert_eq!(others[0]["waiting"][0]["branch"], "waits-b");
        assert_eq!(others[1]["waiting"], json!([]));
    }

    #[test]
    fn the_user_ops_act_announce_and_refuse_unknown_fields() {
        let (_d, db) = crate::mode::tests::db(&[("a", "claude")]);
        let sink = Arc::new(RecordingSink::default());
        let c = ctx(db, sink.clone());
        settings_save(
            &c,
            json!({"settings": {"enabled": true, "newChats": "always"}}),
        )
        .unwrap();
        start_task(&c, "a", "T");
        let db = c.services.need::<Database>().unwrap();
        let run_id: String = db
            .snapshot(|tx| tx.query_row("SELECT id FROM workflow_runs", [], |r| r.get(0)))
            .unwrap();
        sink.events.lock().unwrap().clear();
        let r = task_action(&c, json!({"runId": run_id, "action": "visual"})).unwrap();
        assert_eq!(r, json!({"visual": true, "changed": true}));
        assert_eq!(run_events(&sink), ["a"]);
        assert_eq!(
            task_action(&c, json!({"runId": run_id, "action": "visual", "bogus": 1}))
                .unwrap_err()
                .code,
            "bad_args"
        );
        let e = task_action(&c, json!({"runId": run_id, "action": "reopen"})).unwrap_err();
        assert_eq!(e.code, "conflict");
        // Ready goes through GitHub first; with no PR recorded it is refused and says why.
        db.write::<_, rusqlite::Error>(|tx| {
            let ch = crate::store::Change {
                run_id: run_id.clone(),
                repo: "github.com/o/r".into(),
                branch: "b".into(),
                state: "handed_off".into(),
                ..Default::default()
            };
            crate::store::upsert_change(tx, &ch, 6)
        })
        .unwrap();
        struct NoGh;
        impl crate::gh::Gh for NoGh {
            fn run(&self, _: &[String]) -> Result<crate::gh::Out, crate::gh::GhError> {
                panic!("no PR is recorded, so GitHub is never asked")
            }
        }
        sink.events.lock().unwrap().clear();
        let e = change_ready_with(
            &c,
            json!({"runId": run_id, "repo": "github.com/o/r", "branch": "b"}),
            &NoGh,
        )
        .unwrap_err();
        assert!(e.message.contains("no PR"), "{}", e.message);
        assert_eq!(
            run_events(&sink),
            ["a"],
            "the failed check is recorded on the change"
        );
        // With the chat off, Drop change waits for it to be on.
        chat_mode_set(&c, json!({"chatId": "a", "mode": "off"})).unwrap();
        let e = change_action(
            &c,
            json!({"runId": run_id, "repo": "github.com/o/r", "branch": "b", "action": "drop"}),
        )
        .unwrap_err();
        assert_eq!(e.message, crate::user::CHAT_OFF);
    }

    #[test]
    fn the_chat_mode_ops_read_and_flip_one_chat() {
        let (_d, db) = crate::mode::tests::db(&[("a", "claude")]);
        let sink = Arc::new(RecordingSink::default());
        let c = ctx(db, sink.clone());
        let v = chat_mode_get(&c, json!({"chatId": "a"})).unwrap();
        assert_eq!(v["mode"], "auto", "new chats start in Auto by default");
        assert_eq!(v["inEffect"], false, "the master switch is off by default");
        assert!(v["unavailable"].as_str().unwrap().contains("Settings"));
        assert!(chat_mode_set(&c, json!({"chatId": "a", "mode": "off"})).is_err());
        settings_save(&c, json!({"settings": {"enabled": true}})).unwrap();
        let r = chat_mode_set(&c, json!({"chatId": "a", "mode": "off"})).unwrap();
        assert_eq!(r["view"]["mode"], "off");
        assert_eq!(r["view"]["setBy"], "user");
        assert_eq!(r["row"], "You turned Orchestrator off for this chat.");
        let again = chat_mode_set(&c, json!({"chatId": "a", "mode": "off"})).unwrap();
        assert!(again.get("row").is_none());
        assert_eq!(sink.kinds(), ["workflow_settings", "workflow_chat_mode"]);
        assert_eq!(
            chat_mode_get(&c, json!({"chatId": "zz"})).unwrap_err().code,
            "not_found"
        );
        assert!(chat_mode_set(&c, json!({"chatId": "a", "mode": "sideways"})).is_err());
    }
}
