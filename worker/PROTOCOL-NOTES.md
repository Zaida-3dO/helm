# Worker protocol notes

How the chat worker (`worker/src`) talks to the host over stdio and drives Claude Code: the line
shapes, the choices behind them, and what was observed of the real CLI.

## Launch and framing

- **Launcher:** `node worker/src/main.ts <launch.json>`. TypeScript with erasable syntax only, run
  directly by Node (no build step). The worker keeps `worker/src/<area>/` subfolders, so the host
  stages the whole `src` tree into the per-chat folder rather than flattening it.
- **`launch.json` fields:** `chatId` (required), `harness` (`"claude"` or absent),
  `cwd`, `claude {command?, commandArgs?, model?, effort?, skipPermissions?, apiKeyMode?,
  resumeSessionId?, appendSystemPrompt?, settings?, settingSources?, extraArgs?}`, and a test-only
  `limits {maxRecordBytes, promptAckMs, stopSettleMs, controlTimeoutMs, shutdownGraceMs}`. A launch
  that cannot be read or has no `chatId` gets one `worker_error` and exit code 2.
- **Record limit:** 96 MiB both ways. One `LineReader` is
  shared by the host side and the claude side and reports three faults: `too_long` (limit passed
  before a newline), `truncated` (stream ended mid-line) and `bad_json` (complete line that is not
  a JSON object). Any fault fails the run: a `worker_error`, then exit 1. Blank lines are ignored
  and a trailing `\r` is tolerated.
- **stderr:** diagnostics only, and the worker stops writing after 16 KiB of its own output.
- **Exit codes:** 0 after `shutdown` or the host closing stdin; 1 after a fatal error; 2 for a bad
  launch description.

## Commands

| Command | Our shape and answer |
|---|---|
| `get_state` | `{id}`. Answered once the CLI's `initialize` control request has been acknowledged. `data`: `{harness, executable, executableSource, model, effort, models:[{id,name,description}]}`. Local ready timeout: the control timeout, 60 s. |
| `prompt` | `{id, submissionId (UUID, required), text, images?:[{data (base64), mimeType}], streamingBehavior?, priority?}`. Refused before `get_state` has been answered. Text over 2 MiB (UTF-8 bytes) is refused. `data` on success: `{submissionId, state:"accepted"}`. |
| `steer` | Same fields as `prompt`; treated as a prompt with `streamingBehavior: "steer"`. Claude queues it natively and merges it after running tool calls. |
| `abort` | `{id?, continueQueue?, userOnly?}`. The two flags are host-side queue policy; the worker echoes them in `data` and always interrupts. `data.state` is `"idle"` (nothing to stop) or `"stopping"`. |
| `set_model` | `{id, model}`. `data: {model, applies:"next_turn"}`. |
| `set_thinking_level` | `{id, level}`; `level` is one of `low medium high xhigh max`. `data: {effort, applies:"next_turn"}`. |
| `extension_ui_response` | `{id, value?, confirmed?, cancelled?, goto?}`; no response. An unknown or stale `id` is ignored. |
| `shutdown` | No id. Interrupts a running turn, closes the CLI's stdin, waits up to 3 s, then kills the tree. |
| `compact`, `deliver_now`, `stop_tool`, `withdraw` | Answered with a failure ("not supported by this worker yet"). Wait limit and per-tool Stop come later. |

Every command carrying an `id` gets exactly one `response`; a second answer for the same id is
suppressed in `HostChannel` and logged to stderr.

## Events

| Event | Fields |
|---|---|
| `agent_start` | — |
| `message_start` | `message {id, role, content:[], model?}` |
| `message_update` | `messageId, index, delta {type: text_delta \| thinking_delta \| toolcall_delta, text}` |
| `message_end` | `message {id, role, content, model?, stopReason?, usage?, status: complete \| error \| interrupted, errorMessage?, submissionId? (user rows)}` |
| `tool_execution_start` | `toolCallId, toolName, args` |
| `tool_execution_end` | `toolCallId, toolName, result {text, content?}, isError` |
| `usage` | `inputTokens, outputTokens, cacheReadTokens, cacheWriteTokens, costUsd, durationMs, numTurns` |
| `auto_retry_start` | `attempt, maxAttempts, delayMs, errorStatus, errorMessage, waiting:false` |
| `auto_retry_end` | `state: recovered \| retried \| cancelled \| failed, attempt` |
| `agent_settled` | `outcome: complete \| error \| stopped \| interrupted, error?, sessionId?` |
| `extension_ui_request` | `id, method: select \| confirm \| input \| editor \| dismiss, attention?, title?, header?, message?, context?, options?, optionDescriptions?, allowOther?, secret?, step?, steps?, answer?, prefill?`; `step` is **0-based**; a dismissal carries `remaining`. |
| `permission_mode` | `mode` (from the CLI's init/status records) |
| `native_session` | `sessionId`, emitted whenever the CLI's session id is first seen or changes (the host needs it to resume) |
| `compaction_end` | `trigger` |
| `notice` | `text` |
| `worker_error` | `error` |

Content blocks: `{type:"text",text}`, `{type:"thinking",thinking}`,
`{type:"toolCall",id,name,arguments}`, `{type:"image",mimeType?}`.

User rows: when Claude acknowledges a submission the worker emits `message_start` and
`message_end` for it (role `user`, id = submission id).

## Claude Code driver

- **Flags:** `-p`, stream-json in and out, `--verbose`, `--include-partial-messages`,
  `--replay-user-messages`, `--permission-prompt-tool stdio`, `--setting-sources user,project,local`
  (overridable), `--model`, `--effort` (omitted for haiku models), and when asked
  `--permission-mode bypassPermissions`, `--resume`, `--append-system-prompt`, `--settings`.
- **Submission ack:** the CLI echoes each user message (with our uuid) because of
  `--replay-user-messages`; that echo is the acknowledgement. No echo within 30 s: the prompt is
  answered `{success:false, uncertain:true}` and never written again. A repeated `prompt` with a
  submission id the worker has already seen returns the first answer and writes nothing.
- **Model and effort:** idle, the worker sends `set_model` or `apply_flag_settings
  {effortLevel}` control requests and answers after the CLI acknowledges (10 s limit). During a
  turn it answers at once with `applies:"next_turn"` and sends the control requests just before the
  next prompt. Both were exercised against the real CLI (2.1.289) by the live smoke test. Effort
  for a haiku model is remembered but not sent.
- **Stop:** `abort` sends an `interrupt` control request. A `result` that follows a stop settles
  as `stopped`, even when the CLI labels it an error. If no `result`
  arrives within 30 s the worker kills the CLI's process tree, settles the run as `interrupted`,
  emits `worker_error` and exits (the host's 60 s deadline stays the outer bound). **Stop turn** on
  an approval denies with `interrupt:true` and is treated as a user Stop.
- **Crash:** the CLI closing unexpectedly ends any open message as `interrupted`, ends open tool
  calls with an error, settles a running turn as `interrupted`, answers any unacknowledged prompt
  as uncertain, emits `worker_error` and exits 1.
- **Subagent traffic** (records with `parent_tool_use_id`) is dropped from the parent transcript
  for now; the subagent UI comes later.
- **Retry:** `system/api_retry` records start a retry. A later message start ends it `recovered`;
  another retry ends the earlier one `retried`; an error result ends it `failed`; a stop ends it
  `cancelled`. A failed request shows up as a synthetic assistant message, projected as an
  `error` row.
- **Other CLI control requests:** `hook_callback` answered empty (no hooks registered yet),
  `elicitation` declined, anything else answered with an error.

## Interactions

- **Approvals** (`can_use_tool`): options `Allow once`, `Allow for this session` (only when the CLI
  sent permission suggestions; the suggestions come back with `destination:"session"`, so no
  settings file is written), `Deny`, `Stop turn`. If the context rows add up to more than 64 KiB,
  only `Deny` and `Stop turn` are offered and each row is cut to 4 KiB for display. An answer that
  was not offered counts as `Deny`.
- **Skip permission prompts** (`skipPermissions`): approvals are allowed automatically, except a
  Bash/PowerShell command that looks like it deletes a drive root, a home folder or the working
  folder itself, which still asks. The check is a simple pattern, not a parser (follow-up).
- **Questions** (`AskUserQuestion`): 1–4 questions, at most 32 options each, otherwise denied
  without asking. Asked one step at a time with `allowOther:true`; `goto` revisits a step (its
  earlier answer comes back as `prefill`); `cancelled` denies with "no answer was given". Answers
  return as `updatedInput.answers` keyed by question text.
- **Plan approval** (`ExitPlanMode`): `confirm` with `Approve plan` / `Keep planning`; approve
  allows, anything else denies so Claude keeps planning. A plan over 64 KiB offers only
  `Keep planning`.
- Pending interactions are dismissed when a turn settles or is stopped, and when the CLI withdraws
  a request (`control_cancel_request`).

## Environment and executable

- **Credentials stay with the CLI:** `ANTHROPIC_API_KEY`, `ANTHROPIC_AUTH_TOKEN`, `ANTHROPIC_BASE_URL`,
  `CLAUDE_CODE_OAUTH_TOKEN` and `CLAUDECODE` are removed from the claude child's environment unless
  `claude.apiKeyMode` is true (compared case-insensitively, as Windows does). `CLAUDE_CODE_ENTRYPOINT`
  and `CLAUDE_CODE_SSE_PORT` are always removed. `CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC=1` and
  `DISABLE_AUTOUPDATER=1` are always set.
- **Finding claude:** it is resolved as `HELM_CLAUDE_PATH`, then `claude.exe` on PATH, then
  `~/.local/bin/claude.exe`, then the newest `~/.vscode/extensions/anthropic.claude-code-*`
  native binary. Wrapper scripts (`.cmd`, `.bat`, `.ps1`, ...) are refused, including an explicit
  `claude.command` in `launch.json`. The CLI's process tree is ended with `taskkill /T /F` on
  Windows (a process group kill on Linux); the host's Job Object is the backstop.
