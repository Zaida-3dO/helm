//! The `plan_usage` worker line, through the host-tools worker-line registry.
//!
//! A chat's worker turns each `rate_limit_event` from the claude binary into
//! `{type: "plan_usage", source: "rate_limit_event", windows: [{kind, pct, resetsAt}], at}`:
//! `kind` is the binary's window name (`five_hour`, `seven_day`, `seven_day_opus`,
//! `seven_day_sonnet`), `pct` a percent or null, `resetsAt` epoch milliseconds or null. The host
//! merges it (newer wins per window) and signals the UI.

use crate::service::UsageService;
use crate::store::Passive;
use helm_hosttools::lines::WorkerLines;
use serde_json::Value;

/// At most this many windows are read from one line.
const MAX_WINDOWS: usize = 8;

pub fn parse_windows(line: &Value) -> Vec<Passive> {
    line.get("windows")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .take(MAX_WINDOWS)
                .filter_map(|w| {
                    let kind = w.get("kind")?.as_str()?.to_owned();
                    let pct = w
                        .get("pct")
                        .and_then(Value::as_f64)
                        .filter(|p| p.is_finite())
                        .map(|p| p.clamp(0.0, 100.0));
                    let resets_at = w.get("resetsAt").and_then(Value::as_i64);
                    (pct.is_some() || resets_at.is_some()).then_some(Passive {
                        kind,
                        pct,
                        resets_at,
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

pub fn register_worker_lines(l: &mut WorkerLines) {
    l.register_worker_line("plan_usage", |ctx, line| {
        let windows = parse_windows(line);
        if windows.is_empty() {
            return;
        }
        if let Some(svc) = ctx.services.get::<UsageService>() {
            svc.ingest_passive(&windows, helm_core::now_ms());
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::service::{ControlAnswer, PlanReader};
    use crate::store;
    use crate::types::UsageSource;
    use helm_hosttools::lines::WorkerLineCtx;
    use helm_ipc::{EventSink, RecordingSink, Services};
    use serde_json::json;
    use std::sync::Arc;

    struct Never;
    impl PlanReader for Never {
        fn read(&self) -> ControlAnswer {
            unreachable!("a worker line never starts a read")
        }
    }

    #[test]
    fn a_plan_usage_line_reaches_the_store_and_signals_the_ui() {
        let mut reg = helm_core::db::MigrationRegistry::new();
        reg.initializer("plan-usage", store::init);
        let dir = tempfile::tempdir().unwrap();
        let db =
            Arc::new(helm_core::db::Database::open(&dir.path().join("t.sqlite3"), &reg).unwrap());
        let sink = Arc::new(RecordingSink::default());
        let events: Arc<dyn EventSink> = sink.clone();
        let svc = Arc::new(UsageService::new(db, events.clone(), Arc::new(Never)));
        let mut services = Services::new();
        services.insert(svc.clone());
        let services = Arc::new(services);

        let mut lines = WorkerLines::new();
        register_worker_lines(&mut lines);
        let ctx = WorkerLineCtx {
            chat_id: "c1",
            services: &services,
            events: &events,
        };
        let line = json!({"type": "plan_usage", "source": "rate_limit_event", "at": 1,
            "windows": [{"kind": "five_hour", "pct": 82.5, "resetsAt": 1_800_000_000_000i64},
                        {"kind": "overage", "pct": null, "resetsAt": null},
                        {"kind": "seven_day", "pct": 400.0, "resetsAt": null}]});
        assert!(lines.handle(&ctx, &line));
        let r = svc.snapshot(helm_core::now_ms()).reading.unwrap();
        assert_eq!(r.five_hour.as_ref().unwrap().pct, Some(82.5));
        assert_eq!(
            r.five_hour.as_ref().unwrap().source,
            UsageSource::RateLimitEvent
        );
        assert_eq!(
            r.weekly.as_ref().unwrap().pct,
            Some(100.0),
            "a wild number is clamped"
        );
        assert_eq!(sink.kinds(), ["plan_usage"]);
        // A line with nothing usable changes nothing and signals nothing.
        assert!(lines.handle(
            &ctx,
            &json!({"type": "plan_usage", "windows": [{"kind": "five_hour"}, "junk"]})
        ));
        assert_eq!(sink.kinds().len(), 1);
    }
}
