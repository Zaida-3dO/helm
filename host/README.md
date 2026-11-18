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

**A new preference is agent-settable by default.** Classify it `Registry(Base)` and add its entry
(shape, default, one sentence) to `hosttools/src/settings_options.rs`: chats then change it
themselves with `app_update_settings`, with the Undo notice, and a test fails until the entry
exists. Only two other classes exist, and each needs a one-line reason beside it. `High`: the change
changes what runs, what agents may read or who may reach this PC, so a chat only stages it
(`app_stage_settings`) and it applies after the user clicks Approve; it still needs its options
entry, which is how the staged value is checked. `UserOnly`: no tool at all, for the few settings
that change what approval means, and for secrets. Settings that are not preferences (an extension's
keys, a file's) are classed in `NAMED` in `hosttools/src/settings_allow.rs`; a test in the app
crate pins that every key has exactly one class and that every `High` one can be staged.

### Editor selection for agents

`app_ide_context` (`crates/hosttools/src/ide.rs`) gives an agent the code editor's open file and
selection. Helm never connects to the editor: the extension in `tools/vscode-editor-context` writes one
JSON file per VS Code window to `<profile>/ide-context/` (see its README for what it holds and how to
install it: copy the folder into `.vscode/extensions`, no build). The tool reads those files. It
answers `available: false` with a reason when the `ideContext` preference (Settings > Chat; a chat can only stage a change,
for the user to approve; off by default) is off, or when no file was written or touched in the last two minutes. Selections are cut
to 12 KiB each and the reply keeps to 40 KiB of selected text.

## Design decisions

- **Fresh baseline schema** (version 1): see the header of `crates/core/src/db/schema.rs`. Notably:
  `projects.kind` from the first version, no soft-delete flag, harness `claude`/`codex` only,
  status left unconstrained, retry markers in `chat_retry_markers`.
- **Intent tables**: owned by each feature as side tables, finished through recovery steps.
- **Global revision counter key**: `counters.revision`.
- **Recovery and unsent messages**: only `dispatching` or `accepted` submissions become
  `uncertain`; a message never handed to Claude stays `saved` and is sent again at the next start.
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

## When Helm stops: the trail in `diagnostics`

The profile's `diagnostics` folder holds small files that say how the host's last run ended.
All of them hold numbers, ids and the app version — never message text or tokens — and none of
them leaves the machine.

| File | What it says |
|---|---|
| `host.log` | the host's own log (rolling, one older file kept) |
| `run.json` | this run: when it started, its pid, the version. An orderly end adds an `exit` line with the time and the reason (user quit, update restart, sign-out or shutdown). A record without that line when the next run starts means the run before was cut short |
| `heartbeat.json` | rewritten every 45 s while the app runs: the time, pid, uptime and how many chats were working — it pins the moment of a silent death to about a minute |
| `unclean.json` | written at a start that follows a cut-short run: the previous run's record and its last heartbeat, kept together |
| `host-gone.log` | the workers' side: each worker appends one line when its host's stdin closes without a shutdown — an outside kill, seen from the workers (512 KiB cap, one older file) |
| `crash-<time>.dmp` | on a native fault (Windows only): the smallest kind of crash dump — thread stacks and the module list. It holds the process's memory at that moment (that is its purpose), so it stays here, rolling, three newest kept |
| `ui-process.log` | one line per web view fault: the page stopped answering (the watchdog reloads it), or WebView2 reported a failed process, with what was done about it |

A panic on any thread is written to `host.log` with its backtrace. A command that names the
app's own executable, or ends processes by image name rather than by pid, gets a line in
`host.log` from the landing guard (noted, never blocked). Start-up after an unclean end logs
`previous run ended without a clean exit` with the last heartbeat time and the previous
version.
