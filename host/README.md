# host/

The Rust side of Helm: a Cargo workspace (root `Cargo.toml`) of library crates under `host/crates/`
and the Tauri app binary under `host/app/`.

| Crate | Path | Holds |
|---|---|---|
| `helm-platform` | `crates/platform` | OS traits: credential key store (Windows Credential Manager), owner-only ACLs, reparse-point refusal, atomic replace with retry, single-instance pipe, quit dialog; no-op power, badge and sound stubs |
| `helm-ipc` | `crates/ipc` | The one `api(op, args)` command: op registry, dispatcher (window label, trusted origin, `maintenance` lock, 2 `inspections` permits), the tagged event channel |
| `helm-core` | `crates/core` | Profile and `app.lock`, private atomic files, SQLite (WAL, one writer, snapshot readers), migrations, revisions, preferences, credential envelopes, startup recovery, quit |
| `helm-chats` | `crates/chats` | Chats: the record shapes, op arguments and events the UI sees (exported to `ui/src/api/generated/` with ts-rs; `bindings_are_current` keeps them in step), the chat ops, and the send-behaviour defaults |
| `helm-workers` | `crates/workers` | Worker supervision: one kill-on-close Job Object per worker tree, the registry environment, Node resolution (no shims, Node 22.19+), `worker/src` staged per content hash, `launch.json`, `claude-runtime.json`, the debug-only `HELM_TEST_CLAUDE` seam, and the worker process with its 96 MiB record limit and 16 KB stderr tail |
| `helm-app` | `app` | The Tauri 2 binary `helm`: main window, CSP, capabilities, registration file |

Any new crate under `host/crates/` joins the workspace by itself (the members list is a glob).

## Extension seams

A later crate adds IPC ops and SQLite tables **by touching only its own crate plus one registration
line each** in `host/app/src/registrations.rs` (and a dependency line in `host/app/Cargo.toml`).

### Adding `api` ops

In your crate, depend on `helm-ipc` and expose:

```rust
pub fn register_ops(reg: &mut helm_ipc::OpRegistry) {
    reg.blocking("thing_get", helm_ipc::OpClass::Heavy, thing_get)
        .blocking("thing_save", helm_ipc::OpClass::Maintenance, thing_save)
        .async_op("thing_send", thing_send);
}
```

Then add `my_crate::register_ops(reg);` to `registrations::ops`.

- **Classes**: `Async` runs on the async runtime; `Blocking` on the blocking pool;
  `Maintenance` on the blocking pool holding the global, non-reentrant `maintenance` lock (admission,
  dispatch, queue edits, settings saves, forks, deletion, shutdown); `Heavy` holding one of the two
  `inspections` permits (previews, history snapshots).
- **Handlers** get an `OpCtx`: `ctx.services.need::<T>()` reaches a shared service (the database,
  profile, platform...) registered by the app; `ctx.events.emit(AppEvent::new("kind").with(...))`
  sends on the one event channel (mostly signals; the UI re-reads through `api`).
- **Errors** are `ApiError {code, message}`. Unknown ops and unauthorised windows all get the same
  `refused` error, which names no op.
- **Names** are global. Registering a name twice panics when the registry is built, which the app's
  `every_registration_line_is_distinct` test catches.
- **Windows**: only `main`, on the app origin, may call ops. An auxiliary window (pets, say) gets an
  explicit list via `Dispatcher::allow_window_ops`.

### Adding SQLite tables

Core tables change only through the versioned migrations in `crates/core/src/db/schema.rs`
(`PRAGMA user_version`; a newer database is refused). Features never bump the version.
In your crate, depend on `helm-core` and expose:

```rust
pub fn register_migrations(reg: &mut helm_core::db::MigrationRegistry) {
    reg.initializer("things", init_things)           // idempotent DDL, every open
        .one_time("things-backfill-v1", backfill)     // once; marked in `counters`
        .recovery_step("things-intents", recover);    // inside the startup recovery transaction
}

fn init_things(tx: &rusqlite::Transaction<'_>) -> rusqlite::Result<()> {
    tx.execute_batch("CREATE TABLE IF NOT EXISTS things (chat_id TEXT PRIMARY KEY REFERENCES chats(id) ON DELETE CASCADE)")
}
```

Then add `my_crate::register_migrations(reg);` to `registrations::migrations`.

- New features add **side tables** keyed by chat or project id, not columns on core tables.
- An initializer must be safe to run on every launch (`IF NOT EXISTS`). A one-time migration's marker
  is set only after it succeeds, and a marker name is never reused.
- A recovery step runs inside the one recovery transaction: finish or roll back your
  feature's half-done intent rows there. If it fails, the whole recovery rolls back.

### Adding a shared service

A crate whose ops need state of their own (a client, a cache, a background task) exposes
`pub fn register_services(services: &mut helm_ipc::Services, events: &Arc<dyn helm_ipc::EventSink>)`
and adds one line to `registrations::services`. It runs once at startup after the app's own services
are in place, so it may read the database or profile from `services`.

### Generated TypeScript bindings (one folder per crate)

A crate that exports ts-rs types writes them to `ui/src/api/generated/<crate>/` (for example
`generated/projects/`), never to the root of `generated/`, which stays `helm-chats`' own. Each such
crate has its own `bindings_are_current` test copied from `helm-chats`' pattern, and
`HELM_UPDATE_BINDINGS=1 cargo test -p <crate> bindings_are_current` regenerates only its folder.
Only a crate's own generator writes its folder.

### Adding a preference

Add one line to `CLASSIFIED` in `crates/core/src/prefs/classes.rs`, then declare the key in your crate
as `const MY_KEY: PrefKey = PrefKey::classified("myKey");`. An unclassified name **fails the build**.
Write through `helm_core::prefs::set`; never write SQL against `preferences` (a test scans
every host source file and allows only `prefs/store.rs`).

## Design decisions

- **Fresh baseline schema** (version 1): see the header of `crates/core/src/db/schema.rs`. Notably:
  `projects.kind` from the first version, no soft-delete flag, harness `claude`/`codex` only,
  status left unconstrained, retry markers in `chat_retry_markers`.
- **Intent tables**: owned by each feature as side tables, finished through recovery steps.
- **Global revision counter key**: `counters.revision`.
- **Recovery and `queued`**: only `dispatching` submissions become `uncertain`; queued ones stay
  queued.
- **Database file name**: `helm.sqlite3`.
- **Credential envelope**: `HCE1` magic, version byte, 96-bit nonce, ciphertext and tag; AAD =
  `helm-credential\0` + profile hash + `\0` + identity. The Credential Manager entry is filed under
  service "Helm credential key" with the profile hash as the account.

## Platform behaviour on Windows

| POSIX behaviour | Windows equivalent |
|---|---|
| folders 0700, files 0600 | a protected DACL with one ACE for the current user (inherited by children of profile folders) |
| `O_NOFOLLOW` strict open | every path component is checked for a reparse point, and the opened handle is re-checked |
| rename + fsync parent | `MoveFileExW(REPLACE_EXISTING \| WRITE_THROUGH)`, retried up to 10 times on sharing or access errors (antivirus) |
| OS keychain | Windows Credential Manager via the `keyring` crate; no plaintext fallback |
| per-user profile folder | `%LOCALAPPDATA%\Helm` (`HELM_PROFILE_DIR` overrides) |
| `app.lock` + focus | `app.lock` with an OS file lock; the second launch signals `\\.\pipe\helm-<hash>` and exits |
| native quit confirmation | a `MessageBoxW` OK/Cancel |

Linux: POSIX modes and `O_NOFOLLOW`, a Unix socket in the profile for the focus signal, and no
credential store yet (credentials refuse rather than fall back to plaintext).
