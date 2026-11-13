# Setting up Helm

How to build, test and run Helm from source. What the app does is in the [README](../README.md).

## Requirements

- Windows (developed on Windows 11). Linux is a secondary target; macOS is not supported.
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

The installer writes `Helm_<version>_x64-setup.exe`. Run it to install Helm for your user; it needs
no admin rights, and running a newer one updates Helm in place.

The UI also runs on its own, without Tauri, against a fake host: `npx vite --port <port>` in `ui/`.
Add `?seed=250` to the URL to preload a long chat.

Helm keeps its data in `%LOCALAPPDATA%\Helm` on Windows. Set `HELM_PROFILE_DIR` to a scratch folder
to run a build without touching it.

The hub builds from its own folder: `cd hub && cargo test`, and `docker build -t helm-hub:latest .`
for the image. See `hub/README.md`.

Tests that talk to the real Claude Code are opt-in, because they spend plan usage:
`HELM_LIVE_CLAUDE=1 npm run test:live` in `worker/`.

## How it is built

| Part | Technology |
|---|---|
| App shell and host | Rust and Tauri 2. One `api(op, args)` command, SQLite (WAL) for state, a per-user profile folder |
| Renderer | React and TypeScript, built with Vite, in WebView2 on Windows |
| Chat worker | Node, run straight from TypeScript (erasable syntax only, no build step), speaking line-delimited JSON over stdio |
| Helpers | Short-lived Node processes for chat titles, composer suggestions and plan usage |
| Hub | A separate Rust service, built and shipped as a container image |

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
| `tools/` | Small add-ons built on their own, such as the VS Code extension behind the editor-selection tool |
| `docs/` | This guide and the roadmap |

## CI

The `checkpoint` workflow runs only when started by hand or on a pull request labelled
`checkpoint`. It runs the full check set on Windows, a Linux `cargo check`, and the hub's own build.
