# CLAUDE.md — Helm

Helm is a desktop workspace for running and supervising AI coding agents: a Tauri 2 app with a Rust
host, a React renderer and one Node worker per active chat that drives Claude Code. It is
Windows-first; Linux is a secondary target and macOS is not supported. `README.md` says what the app
does; this file is what an agent working on the code needs.

## Layout

| Path | What |
|---|---|
| `host/` | Rust workspace crates under `host/crates/` (`core`, `ipc`, `platform`, `chats`, `workers`, `hosttools`, `consent`, `mcp`, `usage`, `workflow`, `schedules`, `relay` and more) and the Tauri app under `host/app/`. How to add ops, tables, services and preferences: `host/README.md` |
| `ui/` | React + TypeScript renderer (Vite), loaded by Tauri 2. Stable e2e hooks: `ui/TESTIDS.md` |
| `worker/` | Node chat worker: one process per active root chat, the Claude Code driver, guard hooks. Line shapes and observed CLI behaviour: `worker/PROTOCOL-NOTES.md` |
| `helpers/` | Short-lived Node helpers the host starts per job (titles, autocomplete, usage) |
| `fakes/` | Fake claude, fake host, fake MCP server, fake Telegram API and other test doubles |
| `checks/` | End-to-end checks over WebView2's DevTools port, live checks, a renderer benchmark |
| `hub/` | The optional home-server service (shared memory MCP, relay line, Telegram bot), its own Cargo workspace. See `hub/README.md` |
| `docs/roadmap.md` | Where the app is going next |

Other notes kept beside the code: `host/crates/chats/SESSION-FORMAT.md` (the per-chat session log)
and `host/crates/usage/NOTES.md` (what the plan-usage meter reads).

## Windows dev environment

- On Windows, the Bash tool runs **Git Bash** syntax.
- If `cargo` is not on `PATH` in the shell, run `export PATH="$HOME/.cargo/bin:$PATH"` first in
  every Bash call that uses it.
- Node 22.19+ (CI uses 24) and npm. **No pnpm**: use npm workspaces.
- The Visual Studio 2022 C++ build tools and WebView2 are the Tauri 2 prerequisites.
- Keep paths short (MAX_PATH). Put worktrees beside the checkout with short names
  (`../helm-wt-<name>`).
- Run test builds with `HELM_PROFILE_DIR` set to a scratch folder, so an installed Helm's profile
  in `%LOCALAPPDATA%\Helm` is never touched.

## Build and test

| Part | Build | Test |
|---|---|---|
| Rust host | `cargo build --workspace` | `cargo test --workspace` (+ `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings`). After changing a type in `host/crates/chats/src/types.rs`, regenerate the UI bindings: `HELM_UPDATE_BINDINGS=1 cargo test -p helm-chats bindings_are_current` |
| UI | `npm run build -w ui`; to look at it without Tauri: `npx vite --port <free port>` in `ui/` opens against the dev fake host (`?seed=250` preloads a long chat; prompts containing "approve", "question", "plan" or "long" script those cards). Stable e2e hooks: `ui/TESTIDS.md` | `npm test -w ui` (+ `npm run typecheck -w ui`) |
| Tauri app (dev run) | `npm run dev` (Vite on 127.0.0.1:1420 + `cargo run`); production-CSP debug exe: `npm run build:debug` → `target/debug/helm.exe`; set `HELM_PROFILE_DIR` to a scratch folder to keep `%LOCALAPPDATA%\Helm` untouched | — |
| Worker + fakes | none: Node runs the TypeScript directly | `npm test` in `worker/` and in `fakes/`; live smoke (spends plan usage, once per checkpoint): `HELM_LIVE_CLAUDE=1 npm run test:live` in `worker/` |
| Side panel (host `helm-panel`, `ui/src/panel`, `ui/src/markdown`) | part of the Rust host and UI builds | `cargo test -p helm-panel` (staging tests need git on `PATH`); `node --test "src/panel/**/*.test.ts" "src/markdown/*.test.ts"` in `ui/`; e2e `checks/e2e/w3-panel.e2e.ts`. `HELM_TEST_OBSIDIAN_CONFIG=<obsidian.json>` (debug builds only) points the vault list at a test registry |
| End-to-end (fake claude) | `npm run build:debug` first (the check drives `target/debug/helm.exe`) | `npm run e2e -w checks`: E1–E8 over WebView2's DevTools port with playwright-core, scratch profile, `HELM_TEST_CLAUDE`; not part of `npm test`. `HELM_E2E_OUT=<dir>` keeps screenshots, `HELM_E2E_KEEP=1` the profile |
| Hub (`hub/`, its own Cargo workspace, not in `--workspace`) + relay (`host/crates/relay`, `fakes/telegram`) | `cd hub && cargo build`; image: `docker build -t helm-hub:latest .` in `hub/` (built on the server that runs it; the dev machine needs no Docker) | `cd hub && cargo test` (+ `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`); `cargo test -p helm-relay`; `node --test "telegram/*.test.ts"` in `fakes/`; e2e `checks/e2e/w7-relay.e2e.ts` (needs `npm run build:debug` and `cargo build` in `hub/`); details in `hub/README.md` |
| Host tools + consent (`host/crates/{hosttools,consent}`, `worker/src/hosttools`, `ui/src/hosttools`) | as the Rust host and UI rows | `cargo test -p helm-hosttools -p helm-consent`; `node --test test/hosttools-*.test.ts` in `worker/`; `node --test src/hosttools/*.test.ts` in `ui/`; e2e `checks/e2e/w4-hosttools.e2e.ts`; live (spends plan usage) `HELM_LIVE_CLAUDE=1 HELM_LIVE_MCP_JSON=<a .mcp.json with the servers named in the check's header> npm run live -w checks` (`w4.live.ts`; the hub part needs `HELM_LIVE_HUB_URL` and `HELM_LIVE_HUB_TOKEN`). A new host tool's name goes in `host/crates/hosttools/reserved.json`, and the tool-surface baseline in `host/app/src/registrations.rs` pins the list |
| MCP client (`host/crates/mcp`, `worker/src/mcp`, `fakes/mcp-server`, `ui/src/settings/mcp`) | none beyond the Rust host and UI rows | `cargo test -p helm-mcp`; `node --test test/mcp-*.test.ts` in `worker/` (fake MCP server, redaction, 10 servers, the connect-time address rule); `node --test src/settings/mcp/*.test.ts` in `ui/`; Settings → MCP end to end: `checks/e2e/w4-mcp.e2e.ts` |
| Plan usage + item links (`host/crates/{usage,itemlinks}`, `helpers/usage`, `worker/src/usage`, `ui/src/{usage,itemlinks,settings/usage}`) | as the Rust host and UI rows | `cargo test -p helm-usage -p helm-itemlinks`; `npm test -w helpers`; `node --test test/usage-*.test.ts` in `worker/`; `node --test src/usage/*.test.ts src/itemlinks/*.test.ts` in `ui/`; e2e `checks/e2e/w4-usage.e2e.ts`; live (reads real usage from the binary, sends no prompt) `HELM_LIVE_CLAUDE=1 node --test e2e/w4-usage.live.ts` in `checks/` |
| Schedules + AFK + governor (`host/crates/{schedules,afk,governor}`, `ui/src/{schedules,settings/schedules,afk}`) | as the Rust host and UI rows | `cargo test -p helm-schedules -p helm-afk -p helm-governor` (regenerate the schedules bindings with `HELM_UPDATE_BINDINGS=1 cargo test -p helm-schedules bindings_are_current`); `node --test src/schedules/*.test.ts src/afk/*.test.ts` in `ui/`; e2e `checks/e2e/w7-schedules.e2e.ts` and `checks/e2e/w7-afk.e2e.ts`. AFK and governor settings live in each crate's own table, not in preferences (their classes are in `host/crates/core/src/db/tables.rs`). Scheduled prompts are sent with `helm_chats::submit::host_send` (origin `schedule`, gated), never the `chat_send` op |
| Workflow + orchestrator (`host/crates/workflow`, `fakes/standup-writes`) | as the Rust host row | `cargo test -p helm-workflow`; after changing a type in `host/crates/workflow/src/types.rs`, regenerate `ui/src/api/generated/workflow/` with `HELM_UPDATE_BINDINGS=1 cargo test -p helm-workflow bindings_are_current`; e2e `checks/e2e/w6-workflow*.e2e.ts` |
| Repositories + worktrees (`host/crates/{worktrees,gitshim}`, `host/crates/projects/src/{repos,repo_ops,guard}.rs`, `ui/src/settings/runtime`) | as the Rust host and UI rows | `cargo test -p helm-worktrees -p helm-gitshim -p helm-projects` (the removal and stand-in tests need git on `PATH` and make real junctions in a temp folder); `node --test test/env-args.test.ts` in `worker/`. The git stand-in is the `helm-git` binary: the host copies it to `agent-bin/git.exe` in the profile and puts that folder first on a chat's `PATH` only while a repository has worktrees switched off; it is found beside `helm.exe` (or at `HELM_GIT_SHIM`), so the installer must carry `helm-git.exe` next to `helm.exe`. |
| Installer (`host/app/tauri.conf.json`, `host/app/windows/`) | `npm run build:installer` writes `target/release/bundle/nsis/Helm_<version>_x64-setup.exe` (per-user NSIS, no updater; built locally, not published). It bundles `worker/src` and every `helpers/<name>/src` as resources; a new helper must be added to `bundle.resources`. `bundle.windows.allowDowngrades` stays `true`, so a setup installs in place over a build with a higher version number | `node --test e2e/w5-installer.e2e.ts` in `checks/` (what the bundle must carry); smoke from the INSTALLED app: install with `/S`, launch `Programs\Helm\helm.exe` with `HELM_PROFILE_DIR` set to a scratch folder, check the usage meter shows numbers, a new chat gets an automatic title and a chat round-trips; then uninstall (`uninstall.exe /S`; the profile stays) |
| Declared waits + agent tools (`host/crates/waits`, `host/crates/usage/src/summary.rs`, `host/crates/chats/src/flags.rs`) | as the Rust host row | `cargo test -p helm-waits -p helm-usage`; `cargo test -p helm-chats flag_attention`. A wait's firing rules are one table in `waits/src/model.rs`; pull-request waits read through `gh`, which the tests replace with a fake |
| Web search (`host/crates/webtools`, `ui/src/settings/websearch`) | as the Rust host and UI rows | `cargo test -p helm-webtools` (a stand-in for Brave on a loopback port: the key, the tool, the Claude-chats switch and every refusal; no test reaches the real service). In debug builds `HELM_TEST_BRAVE_URL` points searches at a loopback address, nothing else. The key lives only in the credential store; no op, tool result, log line or error may carry it |
| Full checkpoint (all of the above) | `npm ci` (CI does the same), then `cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings && npm run build:debug` | `cargo test --workspace && npm run typecheck -w ui && npm test` (`npm test` runs every workspace's tests), then `npm run e2e -w checks` |

### Writing tests: fewer tests that cover more

Prefer a few tests that each cover many cases over many tiny ones. Every new test file and every slow test
adds to every run, locally and in CI, and slows everyone down.

- Add a case to an existing table-driven test before writing a new test. One table with twenty rows beats
  twenty one-case tests.
- One end-to-end check that walks a whole flow beats a separate test per step.
- Do not add a new test file for a small fix; put the case in the file that already tests that code.
- Test behaviour a user or another component relies on, not private helpers or copy text.

## Hard rules

1. **Auth.** Never read `~/.claude/.credentials.json` or any token store. Never call
   `api.anthropic.com/api/oauth/*`. Claude is used only through the unmodified `claude` binary under
   the user's own sign-in. The worker strips `ANTHROPIC_API_KEY`, `ANTHROPIC_AUTH_TOKEN`,
   `ANTHROPIC_BASE_URL`, `CLAUDE_CODE_OAUTH_TOKEN` and `CLAUDECODE` from the claude child's
   environment unless API-key mode is on.
2. **Windows hazards.**
   - Never `rm -rf` or `git worktree remove` a directory containing a `node_modules` junction:
     `rmdir` the junction first.
   - Never create files named `nul`, `con`, `aux` or `prn` (watch `> nul` in Git Bash: use
     `/dev/null`).
   - Spawn real `.exe` files, never `.cmd`/`.bat` shims.
3. **No secrets in output.** Never print environment variables, tokens or credential files
   (`env`, `printenv` and the like), and never make a test or log line echo a token. If a hook
   blocks such a command, do not work around it.
4. **Stage by path.** `git add <paths>`, never `git add -A`/`.`. Run `git status` before committing
   and confirm every staged path is one you meant to change.

## CI

Actions minutes are limited and **Windows minutes bill double**. CI therefore runs **only** on
`workflow_dispatch` or on PRs labelled `checkpoint`. Never add a `push` or plain `pull_request`
trigger. Ordinary PRs are verified locally; checkpoint PRs get the `checkpoint` label. The e2e step
is non-blocking on GitHub's runner, where WebView2's DevTools port never opens, so a local
`npm run e2e -w checks` run is the e2e gate.
