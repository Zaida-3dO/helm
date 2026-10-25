//! The schedule tools: list, create, update and cancel, as thin wrappers over the schedules
//! crate's own ops, so an agent's schedule is checked by exactly the rules the Settings editor
//! uses (name and message length, the timing limits, an existing unarchived chat, at most 256).
//!
//! - Writes run only when the user explicitly asked, like the other mutating tools.
//! - The agent never quotes a revision: the tool reads the current one and the op still refuses a
//!   change that raced with another.
//! - A create carries a request id taken from the tool call, so a retried call returns the saved
//!   schedule instead of making a second one.
//! - The prompt a schedule sends later is framed as not typed by the user, and goes in as a
//!   schedule-origin, gated send (see the schedules crate); it cannot spend the user's consent.

use crate::registry::{HostToolRegistry, ToolCtx, ToolError, ToolResult};
use crate::spec::ToolSpec;
use serde_json::{json, Map, Value};

const EXPLICIT: &str = " Act only when the user explicitly asked; report what changed.";

fn timing_schema() -> Value {
    json!({
        "type": "object",
        "description": "When it fires. once: {kind:'once', at} where at is an ISO 8601 time with an offset (2026-11-02T09:00:00+00:00) or Unix milliseconds, or {kind:'once', inSeconds} for a delay from now. interval: {kind:'interval', seconds (60 to a year), startAt?}. weekly: {kind:'weekly', hour, minute, days} in the user's local time, days 0 = Monday to 6 = Sunday.",
        "properties": {
            "kind": {"type": "string", "enum": ["once", "interval", "weekly"]},
            "at": {"type": ["string", "integer"]},
            "inSeconds": {"type": "integer", "minimum": 1},
            "seconds": {"type": "integer", "minimum": 60},
            "startAt": {"type": ["string", "integer"]},
            "hour": {"type": "integer", "minimum": 0, "maximum": 23},
            "minute": {"type": "integer", "minimum": 0, "maximum": 59},
            "days": {"type": "array", "items": {"type": "integer", "minimum": 0, "maximum": 6}}
        },
        "required": ["kind"],
        "additionalProperties": false
    })
}

fn refuse(m: impl Into<String>) -> ToolError {
    ToolError::refused(m)
}

/// An instant given as Unix milliseconds or an ISO 8601 time with an offset.
fn instant(v: &Value, what: &str) -> Result<i64, ToolError> {
    match v {
        Value::Number(n) => n
            .as_i64()
            .ok_or_else(|| refuse(format!("{what} must be a whole number of milliseconds."))),
        Value::String(s) => chrono::DateTime::parse_from_rfc3339(s.trim())
            .map(|d| d.timestamp_millis())
            .map_err(|_| {
                refuse(format!(
                    "{what} must be an ISO 8601 time with an offset, such as 2026-11-02T09:00:00+00:00."
                ))
            }),
        _ => Err(refuse(format!("{what} must be a time."))),
    }
}

/// The agent's timing as the schedules op takes it.
fn timing(v: &Value) -> Result<Value, ToolError> {
    let o = v
        .as_object()
        .ok_or_else(|| refuse("timing must be an object."))?;
    let mut out = Map::new();
    let kind = o.get("kind").and_then(Value::as_str).unwrap_or_default();
    out.insert("kind".into(), json!(kind));
    match kind {
        "once" => {
            let at = match (o.get("at"), o.get("inSeconds").and_then(Value::as_i64)) {
                (Some(at), None) => instant(at, "at")?,
                (None, Some(s)) if s > 0 => helm_core::now_ms() + s * 1000,
                _ => return Err(refuse("A one-time timing needs either at or inSeconds.")),
            };
            out.insert("at".into(), json!(at));
        }
        "interval" => {
            let s = o
                .get("seconds")
                .and_then(Value::as_u64)
                .ok_or_else(|| refuse("An interval needs seconds."))?;
            out.insert("seconds".into(), json!(s));
            if let Some(start) = o.get("startAt").filter(|v| !v.is_null()) {
                out.insert("startAt".into(), json!(instant(start, "startAt")?));
            }
        }
        "weekly" => {
            for k in ["hour", "minute", "days"] {
                out.insert(
                    k.into(),
                    o.get(k)
                        .cloned()
                        .ok_or_else(|| refuse(format!("A weekly timing needs {k}.")))?,
                );
            }
        }
        _ => return Err(refuse("timing.kind must be once, interval or weekly.")),
    }
    Ok(Value::Object(out))
}

fn iso(ms: i64) -> Value {
    chrono::DateTime::from_timestamp_millis(ms)
        .map(|d| json!(d.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)))
        .unwrap_or(Value::Null)
}

/// One schedule as the agent reads it.
fn summary(s: &Value) -> Value {
    let d = &s["definition"];
    let mut out = json!({
        "id": s["id"],
        "title": d["title"],
        "message": d["message"],
        "timing": d["timing"],
        "chatId": d["chatId"],
        "chatTitle": s["chatTitle"],
        "state": s["state"],
        "revision": s["revision"],
    });
    if let Some(ms) = s["nextRunAt"].as_i64() {
        out["nextRunAt"] = json!(ms);
        out["nextRunAtUtc"] = iso(ms);
    }
    out
}

async fn find(ctx: &ToolCtx, id: &str) -> Result<Value, ToolError> {
    let all = ctx
        .op("schedule_list", json!({"includeInactive": true}))
        .await?;
    all["schedules"]
        .as_array()
        .and_then(|l| l.iter().find(|s| s["id"] == id))
        .cloned()
        .ok_or_else(|| refuse("There is no schedule with that id. List schedules first."))
}

fn id_arg(a: &Value) -> Result<String, ToolError> {
    a.get("scheduleId")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| refuse("scheduleId is required."))
}

async fn list(ctx: ToolCtx, a: Value) -> ToolResult {
    let all = a
        .get("includeInactive")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let out = ctx
        .op("schedule_list", json!({"includeInactive": all}))
        .await?;
    let schedules: Vec<Value> = out["schedules"]
        .as_array()
        .map(|l| l.iter().map(summary).collect())
        .unwrap_or_default();
    Ok(json!({"schedules": schedules, "nowUtc": iso(out["now"].as_i64().unwrap_or_default())}))
}

async fn create(ctx: ToolCtx, a: Value) -> ToolResult {
    let str_of = |k: &str| a.get(k).and_then(Value::as_str);
    let title = str_of("title").ok_or_else(|| refuse("title is required."))?;
    let message = str_of("message").ok_or_else(|| refuse("message is required."))?;
    let t = timing(
        a.get("timing")
            .ok_or_else(|| refuse("timing is required."))?,
    )?;
    let chat = str_of("chatId").unwrap_or(&ctx.caller_chat_id);
    let request_id = match &ctx.tool_call_id {
        Some(id) => format!("tool:{}:{id}", ctx.caller_chat_id),
        None => format!("tool:{}", uuid::Uuid::new_v4()),
    };
    let s = ctx
        .op(
            "schedule_create",
            json!({"requestId": request_id, "title": title, "message": message, "timing": t, "chatId": chat}),
        )
        .await?;
    Ok(json!({"created": summary(&s)}))
}

async fn update(ctx: ToolCtx, a: Value) -> ToolResult {
    let id = id_arg(&a)?;
    let current = find(&ctx, &id).await?;
    let mut args = Map::new();
    args.insert("scheduleId".into(), json!(id));
    args.insert("revision".into(), current["revision"].clone());
    for k in ["title", "message", "chatId", "paused"] {
        if let Some(v) = a.get(k).filter(|v| !v.is_null()) {
            args.insert(k.into(), v.clone());
        }
    }
    if let Some(t) = a.get("timing").filter(|v| !v.is_null()) {
        args.insert("timing".into(), timing(t)?);
    }
    if args.len() == 2 {
        return Err(refuse(
            "Nothing to change: pass title, message, timing, chatId or paused.",
        ));
    }
    let s = ctx.op("schedule_update", Value::Object(args)).await?;
    Ok(json!({"updated": summary(&s)}))
}

async fn cancel(ctx: ToolCtx, a: Value) -> ToolResult {
    let id = id_arg(&a)?;
    let current = find(&ctx, &id).await?;
    let s = ctx
        .op(
            "schedule_cancel",
            json!({"scheduleId": id, "revision": current["revision"]}),
        )
        .await?;
    Ok(json!({"cancelled": summary(&s)}))
}

pub fn register_host_tools(r: &mut HostToolRegistry) {
    r.async_tool(
        ToolSpec::read(
            "app_list_schedules",
            "List the user's schedules: active and paused ones, or every one with includeInactive. Each has its id, name, message, timing, destination chat and next run.",
            json!({"type": "object", "properties": {"includeInactive": {"type": "boolean"}}, "additionalProperties": false}),
        )
        .self_locking(),
        list,
    )
    .async_tool(
        ToolSpec::write(
            "app_create_schedule",
            format!(
                "Create a schedule that sends a saved message into a chat later, once, every so often or on chosen weekdays. It runs only while Helm is open and awake. The chat defaults to this one.{EXPLICIT}"
            ),
            json!({
                "type": "object",
                "properties": {
                    "title": {"type": "string", "description": "A short name, at most 160 characters."},
                    "message": {"type": "string", "description": "What to send, at most 8000 bytes."},
                    "timing": timing_schema(),
                    "chatId": {"type": "string", "description": "The chat to send into; defaults to this chat."}
                },
                "required": ["title", "message", "timing"],
                "additionalProperties": false
            }),
        )
        .large()
        .self_locking(),
        create,
    )
    .async_tool(
        ToolSpec::write(
            "app_update_schedule",
            format!(
                "Change a schedule's name, message, timing or chat, or pause (paused: true) or resume (paused: false) it. A new time on a finished schedule leaves it paused until resumed.{EXPLICIT}"
            ),
            json!({
                "type": "object",
                "properties": {
                    "scheduleId": {"type": "string"},
                    "title": {"type": "string"},
                    "message": {"type": "string"},
                    "timing": timing_schema(),
                    "chatId": {"type": "string"},
                    "paused": {"type": "boolean"}
                },
                "required": ["scheduleId"],
                "additionalProperties": false
            }),
        )
        .large()
        .self_locking(),
        update,
    )
    .async_tool(
        ToolSpec::write(
            "app_cancel_schedule",
            format!("Cancel a schedule for good; it cannot be resumed.{EXPLICIT}"),
            json!({
                "type": "object",
                "properties": {"scheduleId": {"type": "string"}},
                "required": ["scheduleId"],
                "additionalProperties": false
            }),
        )
        .self_locking(),
        cancel,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::{CallOrigin, OpInvoker, ToolHandler};
    use std::sync::{Arc, Mutex};

    /// Answers the schedule ops from one stored schedule and records every call.
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
            let sched = json!({
                "id": "s1", "revision": 3, "state": "active", "chatTitle": "Work", "nextRunAt": 1_800_000_000_000i64,
                "definition": {"title": "T", "message": "M", "chatId": "c", "timing": {"kind": "once", "at": 1}}
            });
            let out = match op {
                "schedule_list" => {
                    Ok(json!({"schedules": [sched], "templates": [], "now": 1_800_000_000_000i64}))
                }
                "schedule_create" if args["title"] == "bad" => Err(helm_ipc::ApiError::bad_args(
                    "The name must be 1 to 160 characters.",
                )),
                _ => Ok(sched),
            };
            Box::pin(async move { out })
        }
    }

    async fn call(fake: &Arc<Fake>, name: &str, args: Value) -> ToolResult {
        let mut r = HostToolRegistry::new();
        register_host_tools(&mut r);
        let ToolHandler::Async(f) = r.get(name).unwrap().handler.clone() else {
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
    async fn the_tools_go_through_the_schedule_ops_with_their_rules() {
        let fake = Arc::new(Fake(Mutex::new(vec![])));
        let last = |f: &Arc<Fake>| f.0.lock().unwrap().last().cloned().unwrap();

        // List: the agent sees ids, timing and a readable next run.
        let l = call(&fake, "app_list_schedules", json!({})).await.unwrap();
        assert_eq!(l["schedules"][0]["id"], "s1");
        assert_eq!(l["schedules"][0]["nextRunAtUtc"], "2027-01-15T08:00:00Z");

        // Create: the chat defaults to the caller, the delay becomes an instant, the request id
        // comes from the tool call so a retry is the same schedule.
        let c = call(
            &fake,
            "app_create_schedule",
            json!({"title": "T", "message": "M", "timing": {"kind": "once", "inSeconds": 60}}),
        )
        .await
        .unwrap();
        assert_eq!(c["created"]["id"], "s1");
        let (op, args) = last(&fake);
        assert_eq!(op, "schedule_create");
        assert_eq!(args["chatId"], "me");
        assert_eq!(args["requestId"], "tool:me:tc1");
        assert!(args["timing"]["at"].as_i64().unwrap() > helm_core::now_ms());

        // ISO times, weekly and interval timings pass through in the op's shape.
        assert_eq!(
            timing(&json!({"kind": "once", "at": "2026-11-02T09:00:00+01:00"})).unwrap(),
            json!({"kind": "once", "at": 1_793_606_400_000i64})
        );
        assert_eq!(
            timing(&json!({"kind": "weekly", "hour": 9, "minute": 0, "days": [0, 4]})).unwrap(),
            json!({"kind": "weekly", "hour": 9, "minute": 0, "days": [0, 4]})
        );
        assert_eq!(
            timing(&json!({"kind": "interval", "seconds": 600, "startAt": 5})).unwrap(),
            json!({"kind": "interval", "seconds": 600, "startAt": 5})
        );
        for bad in [
            json!({"kind": "once"}),
            json!({"kind": "once", "at": "tomorrow"}),
            json!({"kind": "weekly", "hour": 9}),
            json!({"kind": "yearly"}),
        ] {
            let e = timing(&bad).unwrap_err();
            assert_eq!(e.changed, crate::Changed::No, "{bad}");
        }

        // The op's own refusal reaches the agent, and nothing changed.
        let e = call(
            &fake,
            "app_create_schedule",
            json!({"title": "bad", "message": "M", "timing": {"kind": "interval", "seconds": 60}}),
        )
        .await
        .unwrap_err();
        assert!(e.public.contains("160 characters") && e.changed == crate::Changed::No);

        // Update and cancel quote the revision they just read; an empty update is refused.
        call(
            &fake,
            "app_update_schedule",
            json!({"scheduleId": "s1", "paused": true}),
        )
        .await
        .unwrap();
        let (op, args) = last(&fake);
        assert_eq!(
            (op.as_str(), args["revision"].as_i64()),
            ("schedule_update", Some(3))
        );
        assert!(
            call(&fake, "app_update_schedule", json!({"scheduleId": "s1"}))
                .await
                .is_err()
        );
        call(&fake, "app_cancel_schedule", json!({"scheduleId": "s1"}))
            .await
            .unwrap();
        let (op, args) = last(&fake);
        assert_eq!(
            (op.as_str(), args["revision"].as_i64()),
            ("schedule_cancel", Some(3))
        );
        let e = call(&fake, "app_cancel_schedule", json!({"scheduleId": "nope"}))
            .await
            .unwrap_err();
        assert!(e.public.contains("no schedule"));
    }
}
