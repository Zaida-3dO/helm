# Helm

Helm is a desktop workspace for running and supervising AI coding agents. Each chat drives a real
Claude Code session in a project folder, and Helm keeps those sessions organised, visible and
recoverable: many chats side by side, a live view of what each agent and its subagents are doing,
and the tools around them (files, diffs, a terminal, search) in the same window.

It is Windows-first. Linux is a secondary target (CI checks that the host compiles there); macOS
is not supported.

## What it does

- **Chats with Claude Code.** One worker process per active chat drives the unmodified `claude`
  binary under the user's own sign-in. A chat survives a quit, a crash or a relaunch, and resumes
  its Claude Code session.
- **A composer built for agents.** Model and effort, permission mode (including plan mode),
  attachments and dropped files, references to other chats, a `/` menu, quick replies and action
  buttons.
- **Visibility.** Streaming replies, a timeline of the work, approval, question and plan cards,
  subagents shown as cards that can be stopped one at a time, a Needs attention inbox, and a
  plan-usage meter.
- **The side panel.** Files, Changes with diffs, staging and review comments, notes from local
  vaults, and quick open. A terminal dock sits under the transcript.
- **Host tools and MCP.** Tools Helm offers the agent, each behind a consent step, and an MCP
  client for the user's own servers.
- **Running unattended.** Schedules that send a prompt into a chat, an away mode, a usage governor
  and a run-slot queue.
- **An orchestrator workflow.** A per-chat switch and a workflow panel for a chat that coordinates
  other agents through plan, build, review and landing.
- **The hub** (optional, `hub/`): a small always-on service for a home server. It holds a shared
  memory store served over MCP, relays a few Helm tools to other agents over an outbound line, and
  runs a Telegram bot that talks to Helm's chats.

## How it is built

| Part | Technology |
|---|---|
| App shell and host | Rust and Tauri 2. One `api(op, args)` command, SQLite (WAL) for state, a per-user profile folder |
| Renderer | React and TypeScript, built with Vite, in WebView2 on Windows |
| Chat worker | Node, run straight from TypeScript (erasable syntax only, no build step), speaking line-delimited JSON over stdio |
| Helpers | Short-lived Node processes for chat titles, composer suggestions and plan usage |
| Hub | A separate Rust service, built and shipped as a container image |

## Requirements

- Windows (developed on Windows 11).
- Rust stable with `rustfmt` and `clippy` (`rust-toolchain.toml`; minimum 1.89).
- Node 22.19 or newer, with npm. The repo uses npm workspaces, not pnpm.
- The Tauri 2 prerequisites: on Windows, the Visual Studio 2022 C++ build tools and WebView2.
- Claude Code installed and signed in. Helm finds it through `HELM_CLAUDE_PATH`, `claude.exe` on
  `PATH`, `~/.local/bin/claude.exe`, or the VS Code extension's native binary. Wrapper scripts
  (`.cmd`, `.bat`, `.ps1`) are refused.

## Build, test and run

```sh
npm ci                              # install every workspace

npm run dev                         # run the app: Vite on 127.0.0.1:1420 plus `cargo run`
npm run build:debug                 # debug app with the production CSP: target/debug/helm.exe
npm run build:installer             # per-user NSIS installer under target/release/bundle/nsis/

cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace              # the Rust host
npm run typecheck -w ui
npm test                            # every workspace's tests
npm run e2e -w checks               # end to end against the debug app (build it first)
```

The UI also runs on its own, without Tauri, against a fake host: `npx vite --port <port>` in `ui/`.
Add `?seed=250` to the URL to preload a long chat.

Helm keeps its data in `%LOCALAPPDATA%\Helm` on Windows. Set `HELM_PROFILE_DIR` to a scratch folder
to run a build without touching it.

The hub builds from its own folder: `cd hub && cargo test`, and `docker build -t helm-hub:latest .`
for the image. See `hub/README.md`.

Tests that talk to the real Claude Code are opt-in, because they spend plan usage:
`HELM_LIVE_CLAUDE=1 npm run test:live` in `worker/`.

## Layout

| Path | What |
|---|---|
| `host/` | The Rust host: library crates under `host/crates/` and the Tauri app under `host/app/`. See `host/README.md` |
| `ui/` | The React renderer. Stable test hooks are listed in `ui/TESTIDS.md` |
| `worker/` | The Node chat worker and its Claude Code driver. See `worker/PROTOCOL-NOTES.md` |
| `helpers/` | Short-lived Node helpers: `titles`, `autocomplete`, `usage` |
| `fakes/` | Test doubles: a fake `claude` that replays stream-json fixtures, a fake host, a fake MCP server, a fake Telegram API and others |
| `checks/` | End-to-end checks (Playwright over WebView2's DevTools port), live checks and a renderer benchmark |
| `hub/` | The optional home-server service, its own Cargo workspace |
| `docs/` | The roadmap |

## CI

The `checkpoint` workflow runs only when started by hand or on a pull request labelled
`checkpoint`. It runs the full check set on Windows, a Linux `cargo check`, and the hub's own build.
