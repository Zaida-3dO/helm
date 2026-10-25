//! The wait host tools: `app_wait_for` (declare, list, cancel) and `app_background_result`.
//!
//! Both are the chat's root agent's only (a sub-agent's call is refused at the gate). A wait
//! grants nothing: it only says when to be woken.

use crate::model::{
    file_state, Target, Wait, MAX_NOTE, MAX_PER_CHAT, MAX_TIMEOUT_SECS, MIN_TIMEOUT_SECS,
};
use crate::probes::canonical_pr_url;
use crate::service::WaitsService;
use crate::store::AddError;
use helm_hosttools::{HostToolRegistry, ToolCtx, ToolError, ToolResult, ToolSpec};
use serde_json::{json, Value};

fn refuse(m: impl Into<String>) -> ToolError {
    ToolError::refused(m)
}

fn service(ctx: &ToolCtx) -> Result<std::sync::Arc<WaitsService>, ToolError> {
    ctx.services
        .get::<WaitsService>()
        .ok_or_else(|| refuse("Waits are not available here."))
}

fn str_arg<'a>(a: &'a Value, k: &str) -> Option<&'a str> {
    a.get(k).and_then(Value::as_str)
}

fn iso(ms: i64) -> Value {
    chrono::DateTime::from_timestamp_millis(ms)
        .map(|d| json!(d.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)))
        .unwrap_or(Value::Null)
}

fn summary(w: &Wait) -> Value {
    json!({
        "waitId": w.id,
        "kind": w.target.kind(),
        "target": w.target.describe(),
        "note": w.note,
        "expiresAt": iso(w.expires_at),
    })
}

/// A pull request target from `url`, or `repo` + `number` (+ `host`).
fn pr_url(a: &Value) -> Result<String, ToolError> {
    if let Some(u) = str_arg(a, "url") {
        let rest = u
            .trim()
            .strip_prefix("https://")
            .ok_or_else(|| refuse("url must be an https://github.com pull request address."))?;
        let mut it = rest.split('/');
        let (host, owner, name, pull, num) =
            (it.next(), it.next(), it.next(), it.next(), it.next());
        return match (
            host,
            owner,
            name,
            pull,
            num.and_then(|n| n.parse::<u64>().ok()),
            it.next(),
        ) {
            (Some(h), Some(o), Some(n), Some("pull"), Some(num), None) => {
                canonical_pr_url(h, &format!("{o}/{n}"), num).map_err(refuse)
            }
            _ => Err(refuse(
                "url must look like https://github.com/owner/name/pull/123.",
            )),
        };
    }
    let repo = str_arg(a, "repo")
        .ok_or_else(|| refuse("A pull request needs url, or repo and number."))?;
    let number = a
        .get("number")
        .and_then(Value::as_u64)
        .ok_or_else(|| refuse("number is required with repo."))?;
    canonical_pr_url(str_arg(a, "host").unwrap_or("github.com"), repo, number).map_err(refuse)
}

/// The target the arguments name, checked against what exists right now.
fn target(ctx: &ToolCtx, svc: &WaitsService, a: &Value) -> Result<Target, ToolError> {
    let kind = str_arg(a, "kind").ok_or_else(|| refuse("kind is required."))?;
    match kind {
        "chat_idle" => {
            let id = str_arg(a, "chatId").ok_or_else(|| refuse("chatId is required."))?;
            if id == ctx.caller_chat_id {
                return Err(refuse("A chat cannot wait for itself."));
            }
            if svc.probes().chat(id).map_err(refuse)?.is_none() {
                return Err(refuse("There is no chat with that id."));
            }
            Ok(Target::ChatIdle {
                chat_id: id.to_owned(),
            })
        }
        "process" => {
            let pid = a
                .get("pid")
                .and_then(Value::as_u64)
                .filter(|p| *p > 0 && *p <= u32::MAX as u64)
                .ok_or_else(|| refuse("pid must be a process id."))? as u32;
            if !matches!(
                svc.probes().process(pid),
                crate::model::ProcessObs::Alive(_)
            ) {
                return Err(refuse(
                    "That process is not running (it may already have exited).",
                ));
            }
            Ok(Target::Process { pid })
        }
        "file" => {
            let path = str_arg(a, "path").ok_or_else(|| refuse("path is required."))?;
            let p = std::path::Path::new(path);
            let unc = path.starts_with("\\\\") || path.starts_with("//");
            let climbs = path.split(['/', '\\']).any(|part| part == "..");
            if !p.is_absolute() || path.len() > 1000 || unc || climbs {
                return Err(refuse(
                    "path must be an absolute local path (not a network share) with no '..' part.",
                ));
            }
            Ok(Target::File {
                path: path.to_owned(),
            })
        }
        "pr_checks" => Ok(Target::PrChecks { url: pr_url(a)? }),
        "pr_review" => Ok(Target::PrReview { url: pr_url(a)? }),
        "board_item" => Err(refuse(
            "Waiting on a board item is not available in this version of Helm. Use chat_idle, \
             process, file, pr_checks or pr_review, or check the item yourself on your next turn.",
        )),
        _ => Err(refuse(
            "kind is chat_idle, process, file, pr_checks or pr_review.",
        )),
    }
}

async fn declare(ctx: &ToolCtx, svc: std::sync::Arc<WaitsService>, a: &Value) -> ToolResult {
    let target = target(ctx, &svc, a)?;
    let secs = a
        .get("timeoutSeconds")
        .and_then(Value::as_i64)
        .ok_or_else(|| {
            refuse(format!(
                "timeoutSeconds is required: {MIN_TIMEOUT_SECS} to {MAX_TIMEOUT_SECS}."
            ))
        })?;
    if !(MIN_TIMEOUT_SECS..=MAX_TIMEOUT_SECS).contains(&secs) {
        return Err(refuse(format!(
            "timeoutSeconds must be {MIN_TIMEOUT_SECS} to {MAX_TIMEOUT_SECS} (a minute to a day)."
        )));
    }
    let note = match str_arg(a, "note").map(str::trim).filter(|n| !n.is_empty()) {
        Some(n) if n.chars().count() > MAX_NOTE => {
            return Err(refuse(format!("note is at most {MAX_NOTE} characters.")))
        }
        n => n.map(str::to_owned),
    };
    let now = helm_core::now_ms();
    let mut state = json!({});
    // Baselines are taken now, so "changed" means changed since the agent declared.
    match &target {
        Target::File { path } => {
            state["file"] = file_state(&svc.probes().file(path));
        }
        Target::ChatIdle { chat_id } => {
            let busy = svc
                .probes()
                .chat(chat_id)
                .ok()
                .flatten()
                .is_some_and(|c| c.busy);
            state["seenBusy"] = json!(busy);
        }
        Target::PrChecks { url } | Target::PrReview { url } => {
            // A read now proves `gh` is there and signed in, and that the pull request exists.
            let probes = svc.clone();
            let u = url.clone();
            let obs = tokio::task::spawn_blocking(move || probes.probes().pr(&u))
                .await
                .map_err(|e| ToolError::maybe(format!("The wait failed: {e}")))?
                .map_err(refuse)?;
            if matches!(target, Target::PrReview { .. }) {
                state["review"] =
                    json!({"decision": obs.review_decision, "count": obs.review_count});
            }
        }
        Target::Process { pid } => {
            // The start time tells this process from a later one given the same id.
            if let crate::model::ProcessObs::Alive(Some(started)) = svc.probes().process(*pid) {
                state["started"] = json!(started);
            }
        }
    }
    let w = Wait {
        id: uuid::Uuid::new_v4().to_string(),
        chat_id: ctx.caller_chat_id.clone(),
        target,
        note,
        created_at: now,
        expires_at: now + secs * 1000,
        state,
    };
    match svc
        .declare(&w)
        .map_err(|e| ToolError::maybe(e.to_string()))?
    {
        Ok(()) => Ok(json!({"waitId": w.id, "expiresAt": iso(w.expires_at)})),
        Err(AddError::ChatFull) => Err(refuse(format!(
            "This chat already has {MAX_PER_CHAT} open waits. Cancel one (action cancel) or wait for one to finish."
        ))),
        Err(AddError::AppFull) => Err(refuse(
            "Helm already has the most waits it will watch at once. Try again later.",
        )),
        Err(AddError::NoChat) => Err(refuse("This chat no longer exists.")),
    }
}

async fn wait_for(ctx: ToolCtx, a: Value) -> ToolResult {
    let svc = service(&ctx)?;
    match str_arg(&a, "action").unwrap_or("wait") {
        "wait" => declare(&ctx, svc, &a).await,
        "list" => {
            let waits = svc
                .list(Some(&ctx.caller_chat_id))
                .map_err(|e| ToolError::refused(e.to_string()))?;
            Ok(json!({"waits": waits.iter().map(summary).collect::<Vec<_>>()}))
        }
        "cancel" => {
            let id = str_arg(&a, "waitId").ok_or_else(|| refuse("waitId is required."))?;
            let done = svc
                .cancel(id, Some(&ctx.caller_chat_id))
                .map_err(|e| ToolError::maybe(e.to_string()))?;
            Ok(json!({"cancelled": done}))
        }
        _ => Err(refuse("action is wait, list or cancel.")),
    }
}

pub fn register_host_tools(r: &mut HostToolRegistry) {
    r.async_tool(
        ToolSpec::write(
            "app_wait_for",
            format!(
                "Declare a wait, then end your turn: Helm watches the target without using the \
                 model and sends this chat exactly one message when it finishes, changes, dies or \
                 times out (outcome timeout). Use this instead of sleeping, polling or re-reading \
                 output in a loop. Kinds: chat_idle (chatId: another chat's turn ends, it stops or \
                 errors), process (pid, a Windows process id: it exits), file (path, absolute: it appears, changes or is \
                 removed), pr_checks and pr_review (url, or repo owner/name and number: a github.com \
                 pull request, read through the gh tool). timeoutSeconds is required, 60 to \
                 {MAX_TIMEOUT_SECS}. note (optional) is returned to you in the message. At most \
                 {MAX_PER_CHAT} open waits per chat. action list shows this chat's open waits; \
                 action cancel with waitId cancels one. A wait grants you nothing: act on the \
                 outcome within your own permissions. Stopping or archiving this chat cancels its \
                 waits; a fired or cancelled wait never fires again."
            ),
            json!({
                "type": "object",
                "properties": {
                    "action": {"type": "string", "enum": ["wait", "list", "cancel"]},
                    "kind": {"type": "string", "enum": ["chat_idle", "process", "file", "pr_checks", "pr_review"]},
                    "chatId": {"type": "string", "minLength": 36, "maxLength": 36},
                    "pid": {"type": "integer", "minimum": 1},
                    "path": {"type": "string", "maxLength": 1000},
                    "url": {"type": "string", "maxLength": 200},
                    "repo": {"type": "string", "maxLength": 200},
                    "number": {"type": "integer", "minimum": 1},
                    "host": {"type": "string", "maxLength": 100},
                    "timeoutSeconds": {"type": "integer", "minimum": MIN_TIMEOUT_SECS, "maximum": MAX_TIMEOUT_SECS},
                    "note": {"type": "string", "maxLength": MAX_NOTE},
                    "waitId": {"type": "string", "maxLength": 64}
                },
                "additionalProperties": false
            }),
        )
        // The handler reads GitHub before it writes, so it must not hold the app-wide lock.
        .self_locking()
        .timeout_ms(30_000),
        wait_for,
    )
    .blocking(
        ToolSpec::read(
            "app_background_result",
            "Read the output of a call Helm moved to the background so a message could be read. \
             Helm does not move calls to the background yet, so there is never a result here: this \
             tool refuses and says so. To wait on something long, run it where you can read its \
             output and use app_wait_for, not a sleep loop.",
            json!({
                "type": "object",
                "properties": {"callId": {"type": "string", "maxLength": 128}},
                "additionalProperties": false
            }),
        ),
        |_ctx: &ToolCtx, _a| {
            Err(refuse(
                "No call has been moved to the background in this version of Helm, so there is no \
                 result to read. Read the command's output where you started it, and use \
                 app_wait_for to be woken when something long finishes.",
            ))
        },
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{ChatObs, Check, PrObs};
    use crate::probes::Probes;
    use crate::testkit::Fake;
    use helm_core::db::{Database, MigrationRegistry};
    use helm_hosttools::dispatch::{Caller, CallerBinding, ChatRequest};
    use helm_hosttools::HostTools;
    use std::sync::Arc;

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn declaring_listing_and_cancelling_waits_follow_the_rules() {
        let dir = tempfile::tempdir().unwrap();
        let mut reg = MigrationRegistry::new();
        crate::register_migrations(&mut reg);
        let db = Arc::new(Database::open(&dir.path().join("t.sqlite3"), &reg).unwrap());
        let me = "11111111-1111-1111-1111-111111111111";
        let other = "22222222-2222-2222-2222-222222222222";
        db.write(|tx| {
            tx.execute("INSERT INTO projects(id, name, created_at) VALUES ('p', 'P', 1)", [])?;
            for id in [me, other] {
                tx.execute(
                    "INSERT INTO chats(id, project_id, created_at, updated_at) VALUES (?1, 'p', 1, 1)",
                    [id],
                )?;
            }
            Ok::<_, rusqlite::Error>(())
        })
        .unwrap();
        let probes = Arc::new(Fake::default());
        probes.chats.lock().unwrap().insert(
            other.into(),
            ChatObs {
                archived: false,
                busy: true,
                stopped: false,
                error: None,
                queued: false,
            },
        );
        let svc = Arc::new(WaitsService::new(
            db.clone(),
            probes.clone() as Arc<dyn Probes>,
        ));
        let mut services = helm_ipc::Services::new();
        services.insert(db.clone());
        services.insert(svc.clone());
        let services = Arc::new(services);
        let mut r = HostToolRegistry::new();
        register_host_tools(&mut r);
        let tools = HostTools::new(Arc::new(r));
        let call = |name: &str, args: Value| {
            let (tools, services) = (&tools, services.clone());
            let req = ChatRequest {
                id: uuid::Uuid::new_v4().to_string(),
                operation: name.into(),
                args,
                caller: Some(CallerBinding {
                    child: false,
                    tool_call_id: None,
                }),
            };
            let caller = Caller {
                chat_id: me.into(),
                restricted: None,
            };
            async move {
                tools
                    .call(services, Arc::new(helm_ipc::NullSink), &caller, &req)
                    .await
            }
        };
        let wait = |extra: Value| {
            let mut a = json!({"timeoutSeconds": 600});
            a.as_object_mut()
                .unwrap()
                .extend(extra.as_object().unwrap().clone());
            a
        };

        // Refusals: each says why and creates nothing.
        for (args, why) in [
            (
                wait(json!({"kind": "chat_idle", "chatId": me})),
                "cannot wait for itself",
            ),
            (
                wait(
                    json!({"kind": "chat_idle", "chatId": "33333333-3333-3333-3333-333333333333"}),
                ),
                "no chat",
            ),
            (
                json!({"kind": "chat_idle", "chatId": other}),
                "timeoutSeconds is required",
            ),
            (
                wait(json!({"kind": "chat_idle", "chatId": other, "timeoutSeconds": 59})),
                "a minute to a day",
            ),
            (
                wait(json!({"kind": "chat_idle", "chatId": other, "timeoutSeconds": 86401})),
                "a minute to a day",
            ),
            (
                wait(json!({"kind": "file", "path": "relative/x"})),
                "absolute",
            ),
            (
                wait(json!({"kind": "file", "path": "\\\\host\\share\\x.txt"})),
                "network share",
            ),
            (
                wait(json!({"kind": "file", "path": "C:/work/../x.txt"})),
                "no '..'",
            ),
            (wait(json!({"kind": "board_item"})), "not available"),
            (
                wait(json!({"kind": "pr_checks", "url": "https://gitlab.com/o/r/pull/1"})),
                "github.com",
            ),
            (
                wait(json!({"kind": "pr_checks", "repo": "o/r", "number": 3})),
                "offline",
            ),
        ] {
            let r = call("app_wait_for", args.clone()).await;
            assert_eq!(r["ok"], false, "{args}");
            assert!(r["error"].as_str().unwrap().contains(why), "{args}: {r}");
        }
        assert!(svc.list(None).unwrap().is_empty());

        // Declaring: a chat, a file, and a pull request once `gh` can read it.
        *probes.pr_ok.lock().unwrap() = Some(PrObs {
            state: "OPEN".into(),
            review_decision: "".into(),
            review_count: 0,
            latest_review: None,
            checks: vec![Check {
                name: "build".into(),
                done: false,
                failed: false,
            }],
        });
        let mut ids = vec![];
        for args in [
            wait(json!({"kind": "chat_idle", "chatId": other, "note": "the reviewer"})),
            wait(json!({"kind": "file", "path": "C:/work/a..b.txt"})),
            wait(json!({"kind": "pr_checks", "repo": "o/r", "number": 3})),
        ] {
            let r = call("app_wait_for", args).await;
            assert_eq!(r["ok"], true, "{r}");
            assert!(r["result"]["expiresAt"].as_str().unwrap().ends_with('Z'));
            ids.push(r["result"]["waitId"].as_str().unwrap().to_owned());
        }
        let r = call("app_wait_for", json!({"action": "list"})).await;
        let listed = r["result"]["waits"].as_array().unwrap();
        assert_eq!(listed.len(), 3);
        assert_eq!(listed[0]["note"], "the reviewer");
        assert_eq!(
            listed[2]["target"],
            "the checks on https://github.com/o/r/pull/3"
        );
        // The chat that was busy when declared is remembered as seen working.
        assert_eq!(svc.list(None).unwrap()[0].state["seenBusy"], true);

        // Cancel: once, and only your own.
        let r = call(
            "app_wait_for",
            json!({"action": "cancel", "waitId": ids[0]}),
        )
        .await;
        assert_eq!(r["result"]["cancelled"], true);
        let r = call(
            "app_wait_for",
            json!({"action": "cancel", "waitId": ids[0]}),
        )
        .await;
        assert_eq!(r["result"]["cancelled"], false);
        assert_eq!(svc.list(None).unwrap().len(), 2);

        // Background results: nothing is ever moved, and the refusal says so.
        let r = call("app_background_result", json!({})).await;
        assert_eq!(r["ok"], false);
        assert!(r["error"]
            .as_str()
            .unwrap()
            .contains("No call has been moved"));
    }
}
