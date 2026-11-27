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
| `tools/` | Small add-ons that live outside the app, built without the rest of the repo: `vscode-editor-context/` (the VS Code extension behind `app_ide_context`) |
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

Build and test commands for every part (Rust host, UI, worker, e2e, installer and the rest): see
`docs/build-and-test.md`. Read the row for the part you change before building or testing it.

The everyday ones, kept inline so nobody has to open the file for the basics:

- Rust host: `cargo build --workspace`; test with `cargo test --workspace`, plus
  `cargo fmt --all --check` and `cargo clippy --workspace --all-targets -- -D warnings`.
- UI: `npm run build -w ui`; test with `npm test -w ui`.
- Worker: `npm test` in `worker/`.

### Writing tests: fewer tests that cover more

Prefer a few tests that each cover many cases over many tiny ones. Every new test file and every slow test
adds to every run, locally and in CI, and slows everyone down.

- Add a case to an existing table-driven test before writing a new test. One table with twenty rows beats
  twenty one-case tests.
- One end-to-end check that walks a whole flow beats a separate test per step.
- Do not add a new test file for a small fix; put the case in the file that already tests that code.
- Test behaviour a user or another component relies on, not private helpers or copy text.

## Hard rules

1. **Auth.** Never read `~/.claude/.credentials.json` or any other token store of Claude Code's, and
   never call `api.anthropic.com/api/oauth/*`. Claude is used only through the unmodified `claude`
   binary under the user's own account. The worker strips `ANTHROPIC_API_KEY`,
   `ANTHROPIC_AUTH_TOKEN`, `ANTHROPIC_BASE_URL`, `CLAUDE_CODE_OAUTH_TOKEN` and `CLAUDECODE` from the
   claude child's environment unless API-key mode is on, a saved sign-in token is handed over (next
   paragraph), or the chat runs on a local model server (the paragraph after).
   - **The saved sign-in token.** The user may save a long-lived token made with
     `claude setup-token` in Helm, which keeps it sealed in its own credential store. The host hands
     it to a worker or helper in `HELM_CLAUDE_TOKEN`; that process removes it from its own
     environment once read and sets `CLAUDE_CODE_OAUTH_TOKEN` on the claude child only. Who receives
     it: the worker or helper that launches claude, and that claude child with everything the child
     starts (its hooks, its Bash commands and, with tools set to Full, its own native MCP servers
     all inherit the child's environment). Who never does: Helm's own MCP client and the servers it
     starts (`HELM_CLAUDE_TOKEN` is stripped from them), a chat on a local model, an API-key launch,
     any command line or log line, and the usage meter.
   - **A local model server.** A chat that runs on a local model server configured in Helm on this
     machine gets that server's loopback address (plain http to `127.0.0.1`, `localhost` or `::1`;
     anything else is refused) as the base URL and that server's own key as the token, handed over
     in `HELM_LOCAL_MODEL_KEY` and removed from the worker's environment once read. It never gets
     the user's sign-in, the saved sign-in token or an Anthropic key, and the key is never logged.
     The claude child holds that key as `ANTHROPIC_AUTH_TOKEN`, so its hooks, its Bash commands and
     (with tools set to Full) its own native MCP servers inherit it; Helm's own MCP client never
     passes it on. It is only ever the key to a server on this machine.
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

## Access

Permissions are never a blocker. They only add roadblocks to productivity. New features bias
toward access: every setting and every tool is usable by chats directly, with a notice for the
user and an Undo. Put something behind an approval click **only** when the owner explicitly asks
to restrict it, or when it is highly sensitive and dangerous (it hands over a secret, sends a
recording of the user's voice somewhere, starts a program, bypasses permissions, or changes what
chats may decide on their own). The bar is very high and almost no setting qualifies.

- **Two ways, and no third.** A chat reaches a setting through the `Access` type
  (`helm_core::prefs::classes`): `Direct` (the chat applies it, with a notice and Undo) or
  `Approval(reason)` (the chat stages it with `app_stage_settings` and the user clicks Approve on
  one card). That type is the only way to express access: there is no "only the user" value, and
  an extension's `apply` only validates and stores, so it cannot answer "locked". A part of a
  Direct key that needs approval is declared in the extension's `staged_parts`; the one decision is
  `settings_allow::route`, and `stage_message` is the one place that words the staging answer. An
  approval needs a one-line reason that names the danger, and a test caps how many there may be:
  raising the cap is a decision the commit explains.
- **A flat refusal is never allowed.** No text a chat reads may tell it to ask the user to do the
  thing themselves ("ask the user to change it", "yours to change only"). The only alternative to
  direct access is a staged change the user approves, and the answer names the exact next call. A
  test scans the sources for such text. A secret never passes through a chat: the chat stages
  `credentials` naming a Settings page, and Approve opens it for the user to type the secret.
- **No person on an install has a right another lacks.** What differs between people or between
  computers (rules, memories, skills, agents, settings, chats, schedules, saved items, MCP
  servers, per-device state) is either Shared (visible and used by everyone by default) or
  Personal (shown and applied only for the person or computer it was made for). Personal is only a
  default view so that nothing irrelevant clutters another person's screen; it is never a
  permission. Any other person or computer can still list, read, edit, copy, move or delete a
  Personal item by naming the person or device in the call, with the same notice and Undo as any
  direct change. Defaults may differ per person as long as each person can change their own.

## CI

Actions minutes are limited and **Windows minutes bill double**. CI therefore runs **only** on
`workflow_dispatch` or on PRs labelled `checkpoint`. Never add a `push` or plain `pull_request`
trigger. Ordinary PRs are verified locally; checkpoint PRs get the `checkpoint` label. The e2e step
is non-blocking on GitHub's runner, where WebView2's DevTools port never opens, so a local
`npm run e2e -w checks` run is the e2e gate.
