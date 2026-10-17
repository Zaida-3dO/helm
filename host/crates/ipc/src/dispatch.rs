//! Dispatch for the one `api(op, args)` command.

use crate::authority::{is_trusted_origin, Caller, MAIN_WINDOW};
use crate::registry::{ApiError, Handler, OpClass, OpCtx, OpRegistry, OpResult};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use tokio::sync::{Mutex, Semaphore};

/// Permits for expensive reads.
pub const INSPECTION_PERMITS: usize = 2;

pub struct Dispatcher {
    registry: OpRegistry,
    ctx: OpCtx,
    debug: bool,
    maintenance: Arc<Mutex<()>>,
    inspections: Arc<Semaphore>,
    /// Ops each auxiliary window may call. Empty for now.
    window_ops: BTreeMap<String, BTreeSet<String>>,
}

impl Dispatcher {
    /// `debug` admits the dev server origin; pass `cfg!(debug_assertions)`.
    pub fn new(registry: OpRegistry, ctx: OpCtx, debug: bool) -> Self {
        Self {
            registry,
            ctx,
            debug,
            maintenance: Arc::new(Mutex::new(())),
            inspections: Arc::new(Semaphore::new(INSPECTION_PERMITS)),
            window_ops: BTreeMap::new(),
        }
    }

    /// Let an auxiliary window call a fixed list of ops (pets and the like).
    pub fn allow_window_ops(&mut self, label: &str, ops: &[&str]) {
        self.window_ops
            .entry(label.to_owned())
            .or_default()
            .extend(ops.iter().map(|s| s.to_string()));
    }

    /// The global `maintenance` lock, shared with non-op work such as the quit sequence.
    pub fn maintenance(&self) -> Arc<Mutex<()>> {
        self.maintenance.clone()
    }

    pub fn ctx(&self) -> &OpCtx {
        &self.ctx
    }

    fn authorised(&self, caller: &Caller, op: &str) -> bool {
        if caller.window_label == MAIN_WINDOW {
            return is_trusted_origin(&caller.url, self.debug);
        }
        // Auxiliary windows: their own allowlist, and still only from the app's page.
        self.window_ops
            .get(&caller.window_label)
            .is_some_and(|ops| ops.contains(op))
            && is_trusted_origin(&caller.url, self.debug)
    }

    /// Run `op` for `caller`. Unknown ops and unauthorised callers get the same refusal.
    pub async fn call(&self, caller: &Caller, op: &str, args: Value) -> OpResult {
        if !self.authorised(caller, op) {
            log::warn!("api refused for window '{}'", caller.window_label);
            return Err(ApiError::refused());
        }
        let Some(entry) = self.registry.ops.get(op).cloned() else {
            return Err(ApiError::refused());
        };
        let ctx = self.ctx.clone();
        match (entry.class, entry.handler) {
            (_, Handler::Async(f)) => f(ctx, args).await,
            (OpClass::Maintenance, Handler::Blocking(f)) => {
                let guard = self.maintenance.clone().lock_owned().await;
                run_blocking(move || {
                    let _guard = guard;
                    f(&ctx, args)
                })
                .await
            }
            (OpClass::Heavy, Handler::Blocking(f)) => {
                let permit = self
                    .inspections
                    .clone()
                    .acquire_owned()
                    .await
                    .map_err(|_| ApiError::internal("inspections closed"))?;
                run_blocking(move || {
                    let _permit = permit;
                    f(&ctx, args)
                })
                .await
            }
            (_, Handler::Blocking(f)) => run_blocking(move || f(&ctx, args)).await,
        }
    }
}

async fn run_blocking(f: impl FnOnce() -> OpResult + Send + 'static) -> OpResult {
    tokio::task::spawn_blocking(f)
        .await
        .unwrap_or_else(|e| Err(ApiError::internal(format!("op panicked: {e}"))))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::NullSink;
    use crate::services::Services;
    use serde_json::json;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    fn main_caller() -> Caller {
        Caller {
            window_label: "main".into(),
            url: "http://tauri.localhost/".into(),
        }
    }

    fn ctx() -> OpCtx {
        OpCtx {
            services: Arc::new(Services::new()),
            events: Arc::new(NullSink),
        }
    }

    fn echo_registry() -> OpRegistry {
        let mut r = OpRegistry::new();
        r.blocking("echo", OpClass::Blocking, |_, a| Ok(a));
        r
    }

    #[tokio::test]
    async fn main_window_on_app_origin_may_call() {
        let d = Dispatcher::new(echo_registry(), ctx(), false);
        assert_eq!(
            d.call(&main_caller(), "echo", json!(1)).await.unwrap(),
            json!(1)
        );
    }

    #[tokio::test]
    async fn other_windows_and_origins_are_refused_without_naming_the_op() {
        let d = Dispatcher::new(echo_registry(), ctx(), false);
        let pet = Caller {
            window_label: "pet".into(),
            url: "http://tauri.localhost/".into(),
        };
        assert_eq!(
            d.call(&pet, "echo", json!(1)).await,
            Err(ApiError::refused())
        );
        let web = Caller {
            window_label: "main".into(),
            url: "https://example.com/".into(),
        };
        assert_eq!(
            d.call(&web, "echo", json!(1)).await,
            Err(ApiError::refused())
        );
        let unknown = d.call(&main_caller(), "nope", json!(1)).await.unwrap_err();
        assert_eq!(unknown, ApiError::refused());
        assert!(!unknown.message.contains("nope"));
    }

    #[tokio::test]
    async fn auxiliary_windows_get_only_their_allowlist() {
        let mut r = echo_registry();
        r.blocking("other", OpClass::Blocking, |_, a| Ok(a));
        let mut d = Dispatcher::new(r, ctx(), false);
        d.allow_window_ops("pet", &["echo"]);
        let pet = Caller {
            window_label: "pet".into(),
            url: "http://tauri.localhost/".into(),
        };
        assert!(d.call(&pet, "echo", json!(1)).await.is_ok());
        assert_eq!(
            d.call(&pet, "other", json!(1)).await,
            Err(ApiError::refused())
        );
    }

    /// Records the highest number of handlers running at once.
    fn concurrency_probe(class: OpClass) -> (OpRegistry, Arc<AtomicUsize>) {
        let running = Arc::new(AtomicUsize::new(0));
        let peak = Arc::new(AtomicUsize::new(0));
        let (r2, p2) = (running.clone(), peak.clone());
        let mut reg = OpRegistry::new();
        reg.blocking("slow", class, move |_, _| {
            let now = r2.fetch_add(1, Ordering::SeqCst) + 1;
            p2.fetch_max(now, Ordering::SeqCst);
            std::thread::sleep(Duration::from_millis(100));
            r2.fetch_sub(1, Ordering::SeqCst);
            Ok(Value::Null)
        });
        (reg, peak)
    }

    async fn run_five(d: Arc<Dispatcher>) {
        let mut tasks = Vec::new();
        for _ in 0..5 {
            let d = d.clone();
            tasks.push(tokio::spawn(async move {
                d.call(&main_caller(), "slow", Value::Null).await.unwrap();
            }));
        }
        for t in tasks {
            t.await.unwrap();
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn maintenance_ops_never_overlap() {
        let (reg, peak) = concurrency_probe(OpClass::Maintenance);
        run_five(Arc::new(Dispatcher::new(reg, ctx(), false))).await;
        assert_eq!(peak.load(Ordering::SeqCst), 1);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn heavy_reads_share_two_permits() {
        let (reg, peak) = concurrency_probe(OpClass::Heavy);
        run_five(Arc::new(Dispatcher::new(reg, ctx(), false))).await;
        assert_eq!(peak.load(Ordering::SeqCst), INSPECTION_PERMITS);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn plain_blocking_ops_run_in_parallel() {
        let (reg, peak) = concurrency_probe(OpClass::Blocking);
        run_five(Arc::new(Dispatcher::new(reg, ctx(), false))).await;
        assert!(peak.load(Ordering::SeqCst) > INSPECTION_PERMITS);
    }

    #[tokio::test]
    async fn a_panicking_op_is_an_error_not_a_crash() {
        let mut r = OpRegistry::new();
        r.blocking("boom", OpClass::Blocking, |_, _| panic!("boom"));
        let d = Dispatcher::new(r, ctx(), false);
        let e = d
            .call(&main_caller(), "boom", Value::Null)
            .await
            .unwrap_err();
        assert_eq!(e.code, "internal");
    }
}
