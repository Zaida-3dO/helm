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
| `compact`, `deliver_now`, `stop_tool`, `withdraw` | See "Delivery, steering and tool control" below. |

Every command carrying an `id` gets exactly one `response`; a second answer for the same id is
suppressed in `HostChannel` and logged to stderr.

## Events

| Event | Fields |
|---|---|
| `agent_start` | — |
| `message_start` | `message {id, role, content:[], model?}` |
| `message_update` | `messageId, index, delta {type: text_delta \| thinking_delta \| toolcall_delta, text}` |
| `message_end` | `message {id, role, content, model?, stopReason?, usage?, status: complete \| error \| interrupted, errorMessage?, submissionId? (user rows)}`; `usage` is the request's own, in the API's spelling (`input_tokens`, `output_tokens`, `cache_read_input_tokens`, `cache_creation_input_tokens`): input and cache counts from `message_start`, the output count from `message_delta` |
| `tool_execution_start` | `toolCallId, toolName, args` |
| `tool_execution_end` | `toolCallId, toolName, result {text, content?}, isError` |
| `usage` | `inputTokens, outputTokens, cacheReadTokens, cacheWriteTokens, costUsd, durationMs, numTurns, running?`. The token counts are the result line's `usage`: this turn's main conversation only. `costUsd` is `total_cost_usd`, a running total for the whole process. `running {inputTokens, outputTokens, cacheReadTokens, cacheWriteTokens, costUsd}` is the process's running totals, subagents included (`total_cost_usd` and the sum of `modelUsage`); a resumed session starts from its saved totals, and a `/clear` starts them over. The host records each turn's share of them |
| `auto_retry_start` | `attempt, maxAttempts, delayMs, errorStatus, errorMessage, waiting:false` |
| `auto_retry_end` | `state: recovered \| retried \| cancelled \| failed, attempt` |
| `agent_settled` | `outcome: complete \| error \| stopped \| interrupted, error?, sessionId?` |
| `extension_ui_request` | `id, method: select \| confirm \| input \| editor \| dismiss, attention?, title?, header?, message?, context?, options?, optionDescriptions?, allowOther?, secret?, step?, steps?, answer?, prefill?`; `step` is **0-based**; a dismissal carries `remaining`. |
| `permission_mode` | `mode` (from the CLI's init/status records) |
| `native_session` | `sessionId`, emitted whenever the CLI's session id is first seen or changes (the host needs it to resume) |
| `compaction_end` | `trigger`; after a `compact` command it also carries `ok` (`false` when the turn ended without a compaction boundary) |
| `notice` | `text` |
| `worker_error` | `error` |

Content blocks: `{type:"text",text}`, `{type:"thinking",thinking}`,
`{type:"toolCall",id,name,arguments}`, `{type:"image",mimeType?}`.

User rows: when Claude reads a submission (delivery: lifecycle `started`, or the echo) the worker
emits `message_start` and `message_end` for it (role `user`, id = submission id); see "Delivery,
steering and tool control" for why it is delivery and not the acknowledgement.

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
- **Subagent traffic** (records with `parent_tool_use_id`) never reaches the parent transcript; it
  feeds the subagent events instead (see "Native subagents").
- **Retry:** `system/api_retry` records start a retry. A later message start ends it `recovered`;
  another retry ends the earlier one `retried`; an error result ends it `failed`; a stop ends it
  `cancelled`. A failed request shows up as a synthetic assistant message, projected as an
  `error` row.
- **Other CLI control requests:** `hook_callback` goes to the landing guard (see "Landing guard") or the long-command guard when its
  callback id is the one registered in `initialize` (see "Delivery, steering and tool control"),
  and is answered empty otherwise; `elicitation` declined; anything else answered with an error.

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

## Observed CLI behaviour (claude.exe 2.1.289, 2026-10-17)

Observed by driving the real CLI over stream-json (haiku, a neutral temp folder, `CLAUDECODE`
stripped, setting sources `project,local`). Control-request names were cross-checked against the
public Agent SDK type definitions; no SDK code is used or copied.

- **Message lifecycle.** The CLI reports capability `msg_lifecycle_v1` in `system/init` and writes
  `{type:"command_lifecycle", command_uuid, state}` for every user message that carries a uuid.
  States: `queued` (written within milliseconds of receipt), `started` (the CLI took the message
  into a turn), `completed` (that turn ended) and `cancelled`.
  - A message sent while idle goes `queued` then `started` at once; the `--replay-user-messages`
    echo follows about a second later.
  - A message sent **during** a turn goes `queued` and then waits. At the next tool boundary (after
    the running tool calls return) the CLI writes the echo and `started`, and the same turn's next
    model request sees it. A message still waiting when the turn ends opens the next turn.
  - So `started` is the "read" signal, distinct from the acknowledgement (`queued`). The first
    rule, "the echo is the ack", is too late for a mid-turn message: its echo only arrives when it
    is read, which can be minutes behind a long tool call.
- **Foreground tasks.** A foreground Bash call becomes a CLI task about 3–4 s after it starts:
  `system/task_started {task_id, tool_use_id, is_backgrounded:false, task_type:"local_bash"}`.
  Before that there is no task to move or stop. Subagent calls report `task_type:"local_agent"`.
- **`background_tasks {tool_use_id?}`** answers `{backgrounded:true}`. The blocked call returns at
  once with a tool result saying it was moved to the background (with its task id) and the turn
  carries on. With no `tool_use_id` every foreground task moves. The CLI then writes
  `system/background_tasks_changed {tasks:[{task_id, task_type, description}]}` (the full list
  each time) and `system/task_updated {task_id, patch}`; on completion `task_notification`.
- **`stop_task {task_id}`** works on a foreground Bash task too: that call fails (`Exit code 137`,
  `[killed]`, `is_error`) and the turn carries on. So per-tool Stop is available for Bash once the
  call is a task.
- **`cancel_async_message {message_uuid}`** answers `{cancelled:true}` and writes lifecycle
  `cancelled` for a message that is still waiting; `{cancelled:false}` once it has been read.
- **`interrupt {cancel_queued?}`** answers `{still_queued:[uuid...]}`; with `cancel_queued:true`
  it answers `{still_queued:[], cancelled:[uuid...]}` and writes lifecycle `cancelled` for each.
  An interrupt that lands during a foreground Bash call did **not** end the turn until that shell
  finished (30 s in that run).
- **`priority:"now"`** on a user message sent during a Bash call did not interrupt anything in
  2.1.289; it was only queued.
- **Hooks.** `initialize` takes `hooks: {PreToolUse: [{matcher:"Bash", hookCallbackIds:[id]}]}`.
  Each matching call sends `control_request {subtype:"hook_callback", callback_id, tool_use_id,
  input:{hook_event_name:"PreToolUse", tool_name, tool_input, tool_use_id, permission_mode, ...}}`.
  Answering `{hookSpecificOutput:{hookEventName:"PreToolUse", permissionDecision:"deny",
  permissionDecisionReason}}` fails the call; the model sees an error tool result starting
  `PreToolUse:Bash hook error:` followed by the reason. `additionalContext` adds a note to the call.
  Hooks fire in bypass-permissions mode too. The CLI echoes each `control_response` we send back
  on stdout; the worker ignores responses it did not ask for.
- **Native tools to disallow** (from the `init` tool list): `CronCreate`, `CronDelete`,
  `CronList`, `ScheduleWakeup`, `RemoteTrigger` (schedules), `EnterWorktree`, `ExitWorktree`
  (worktrees).

## Delivery, steering and tool control

**Launch.** `sendBehavior {longCommands: "off"|"warn"|"redirect", optIn: bool}`. When the level is
not `off`, the worker appends its own short "Long commands" rule to `appendSystemPrompt` and
registers the PreToolUse hook in `initialize`.

**Acknowledgement and delivery.**
- A prompt is acknowledged (`{success:true, data:{submissionId, state:"accepted"}}`) on lifecycle
  `queued` for its uuid, or on the echo, whichever comes first. Still 30 s, still never resent.
- Event `submission_delivered {submissionId}` when lifecycle `started` arrives (or the echo, for a
  CLI without lifecycle records). For a prompt that opens a turn it follows the acknowledgement at
  once; for a steer it marks the tool boundary at which Claude read it.
- The user row (`message_start`/`message_end`, role `user`) is now emitted at delivery rather
  than at the acknowledgement, so a steer lands in the transcript where Claude read it. For a
  turn-opening prompt the two moments are milliseconds apart.
- `prompt {streamingBehavior:"steer"}` is written as is. `priority:"now"` is written and, when a
  turn is running, followed by a plain `interrupt` so the message survives and opens the next turn
  (the CLI's own `priority` alone did not interrupt, see the observations above). That turn settles with
  `agent_settled {outcome:"stopped", superseded:true}`, so the host can label it "Superseded by
  your message" rather than as a user Stop.

**Commands.**

| Command | Shape and answer |
|---|---|
| `get_state` | `data` adds `capabilities {withdraw, deliverNow, stopTool, compact}` (all `true` for Claude Code 2.1.289) and `cliVersion` (the CLI's `get_binary_version` answer; `null` if it does not answer within 3 s). |
| `abort` | Before the interrupt, any foreground tasks are moved to the background (otherwise the interrupt waits for the shell). The interrupt is sent with `cancel_queued:true`; `data` adds `cancelledSubmissionIds`: our submissions the CLI dropped, which the host returns to the front of its queue. |
| `withdraw` | `{id, submissionId}` → `data {withdrawn}`; `false` when Claude has already read it (or it is unknown). Uses `cancel_async_message`. |
| `deliver_now` | `{id, submissionId, reason:"wait_limit"\|"send_now", minRunMs}` → `data {result, moved?}`. `stopped`: no turn is running. `nothing_waiting`: that submission is no longer unread. `waiting`: an approval, question or plan is open, or no foreground Bash or subagent call has run for `minRunMs` yet. `unblocked` with `moved:[toolCallId...]`: those calls were moved to the background. It never ends the turn. With `reason:"send_now"` and nothing to move, a plain interrupt is sent instead and the answer is `delivering`. |
| `stop_tool` | `{id, toolCallId, sentAtMs}` → `data {stopped}`. A missing or more than 5 s old `sentAtMs` is refused (`success:false`). A call that is not a stoppable foreground Bash task or a running subagent's Agent call answers `stopped:false`. Uses `stop_task`. |
| `compact` | `{id}`; refused while a turn runs. Sends an internal `/compact` user message and answers `data {state:"compacting"}`; emits `compaction_start {}` then `compaction_end {trigger, ok}` (`ok:false` when the turn ends without a compaction boundary). The compaction runs as a CLI turn, so the host also sees `agent_start` / `agent_settled` for it. |

**Events.**

| Event | Fields |
|---|---|
| `submission_delivered` | `submissionId` |
| `background_tasks` | `tasks: [{id, title, command?, kind:"shell"\|"agent", startedAt}]`, the full list each time |
| `shell_changes` | `toolCallId, paths` (absolute, forward slashes; at most 200): the files a Bash call changed, from a git comparison before and after it; sent after the call's `tool_execution_end`, only when there are some |
| `long_command` | `toolCallId, action:"denied"\|"reminded"\|"escaped", reason` |
| `tool_execution_start` | adds `stoppable: false` |
| `tool_execution_update` | `toolCallId, stoppable: true` once the call is a foreground task that `stop_task` can end |
| `compaction_start` | — |

`deliver_now`, `withdraw`, `stop_tool` and `compact` are refused, like `prompt`, until `get_state`
has been answered.

**Long-command rule.** The classifier and the deny-once policy are in
`worker/src/drivers/claude/longcmd.ts`; the hook answers as in the observations above. Choices made
here: the test and build script names also include `lint`; `-v` is not treated as a
version flag (it means verbose to most test runners); a `timeout N cmd` prefix is looked through,
and Windows `timeout /t N` counts as a sleep; `bash -c`, `cmd /c` and `powershell -Command`
strings are parsed as nested commands. Subagent calls are recognised by the hook input's
`agent_id`, or by a tool call id the worker saw in a subagent's assistant record.

**Native tools.** `--disallowedTools` names the seven tools in the observations above.

**Fake claude.** `fakes/claude/fake-claude.ts` reproduces the lifecycle above (queued at once;
read at once when idle, otherwise at a `{"read": true}` step or the turn's end) and answers
`cancel_async_message` and `interrupt {cancel_queued}` from its list of unread messages. Its header
documents the fixture steps (hooks, replies to control requests, conditions, loops,
shorthand records).

## Forks, context and fast mode

- **Fork launch field.** `claude.forkSession: true` together with `claude.resumeSessionId` adds
  `--fork-session`: the CLI resumes the source chat's session as a new session, so a whole-chat
  fork carries Claude's history without the worker touching any transcript file. The host sets it
  only on a fork's first launch; afterwards the fork's own session id is resumed as usual. A
  message fork cannot be cut that way, so it starts a fresh session and its first prompt carries
  the history up to the chosen reply as a `context` block.
- **`prompt.context`** (string, optional): sent to Claude as a second text content block after
  the message text. The host uses it for the panel-context block and a fork's carried history.
- **`claude.fastMode`** (boolean): always sent as `--settings {"fastMode": …}`; a `fastMode` key
  inside `claude.settings` is dropped, and anything but `true` is off.

## Native subagents

Observed on claude.exe 2.1.289 (haiku, a neutral temp folder, two subagents in parallel), and
implemented in `worker/src/drivers/claude/translate.ts`.

- **Agent calls run in the background by default.** The parent's `Agent` tool call (older CLIs:
  `Task`) gets `system/task_started {task_id, tool_use_id, task_type:"local_agent",
  is_backgrounded:true, description, subagent_type, prompt}` and its tool result at once ("Async
  agent launched successfully…"). The parent's turn can settle while its agents keep working; when
  one finishes the CLI starts a new turn on its own to tell the parent.
- **Inside a subagent** the CLI writes whole `assistant` and `user` records carrying
  `parent_tool_use_id` (the Agent call's id) plus `subagent_type` and `task_description`. No partial
  stream events were seen for subagents. A subagent that spawns its own agent uses that inner call's
  id, which the parent transcript never saw.
- **Progress:** `system/task_progress {task_id, tool_use_id, description, last_tool_name,
  usage:{total_tokens, tool_uses, duration_ms}}` after each subagent tool call.
- **End:** `system/task_updated {task_id, patch:{status}}`, then `system/task_notification {task_id,
  tool_use_id, status: completed | failed | stopped, summary, usage?}`. For a completed agent the
  summary was its final message; for a stopped one it was the description.
- **`stop_task {task_id}` stops one subagent:** answered with success, then `task_updated
  {status:"killed"}`, `task_notification {status:"stopped"}` and an "[Request interrupted by
  user]" user record inside the subagent. The other agents and the parent carry on. The worker's
  `stop_tool` with the Agent call's id routes to it: the driver looks the task up
  with the translator's `subagentTaskId(toolCallId)` and sends `stop_task`; `get_state`'s
  capabilities carry `stopSubagent: true`, which shows the per-agent Stop.

**Events** (keyed by the parent's Agent tool call id; the Agent call is still an ordinary tool row):

| Event | Fields |
|---|---|
| `subagent_start` | `toolCallId, agentType, name, description, assignment, startedAt` |
| `subagent_step` | `toolCallId, text, tool?, tokens?, toolUses?` |
| `subagent_end` | `toolCallId, status: completed \| failed \| stopped, durationMs, report?, error?, tokens?, toolUses?` |

Choices made here:
- `agentType` is the call's `subagent_type` (`general-purpose` when absent); `name` is the call's
  `name` argument when it has one, else its `description`. `assignment` is the prompt's first three
  non-empty lines, at most 400 characters.
- A step is the subagent's newest tool call (`tool` plus its command, path, pattern, URL or
  description, first line, at most 200 characters) or the first line of its newest narration.
  `task_progress` supplies the token and tool-use counts. Steps are paced to at most two a second
  per subagent; within a window the newest state wins and goes out at the window's end. A pending
  step is dropped when the agent ends.
- The report is the subagent's last text message (a message split over several records is joined),
  else the task notification's summary; for a foreground agent, the Agent call's result. At most
  64 KiB.
- A foreground agent (no background flag and a result that is not the launch receipt) ends with its
  call: an error result fails it. When the turn settles, foreground agents still open end `stopped`
  (after a Stop) or `failed`; background agents run on. When the worker loses the CLI every open
  agent ends `failed`.
- Records for an unknown parent (nested agents, or agents of an earlier worker) are ignored.

## The composer's permission-mode pick

| Line | Direction | Fields and behaviour |
|---|---|---|
| `set_permission_mode` | host → worker, no id, no response | `mode`: `bypassPermissions`, `default`, `acceptEdits` or `plan` (anything else is logged and dropped). Sent to a live worker when the user picks, and as a start line to the chat's next worker. The driver sends Claude Code's `set_permission_mode` control request at once when no turn is open, else before the next prompt (with model and effort). |
| `permission_mode` | worker → host | Unchanged meaning; now also sent by the driver when a pick is accepted, and read first-hand from the CLI's `system` init/status records (the translator's duplicate is dropped). |
| `permission_mode_kept` | worker → host | `mode` or `null`. After a pick: the mode the chat is really in, so the next worker starts there. Sent when a pick is accepted, when Claude Code moves to another mode by itself (leaving plan mode), and on a refused pick (the last mode that took, or `null` to drop the pick). |

Skip permission prompts (the launch flag) auto-allows tool requests only until a pick that asks;
after `default`, `acceptEdits` or `plan` the CLI's requests become approval cards.

## Sub-agent limits and the tool output cap

Launch fields (`claude`): `subagentSoftTurns` (0 or absent: no limits), `subagentHardStop` (true:
refuse past the hard limit; otherwise only remind) and `toolOutputCap` (true: cut long tool
results). They come from Settings → Runtime through `claude-runtime.json`, read when a worker
starts. The limits default to chats with Orchestrator on (the host sets `subagentSoftTurns` to 0
for any other chat unless the setting is "all chats"), with notes only; a chat with Orchestrator
off launches exactly as before.

**Sub-agent limits** (`worker/src/drivers/claude/limits.ts`). With a non-zero `subagentSoftTurns`
the worker registers one more `PreToolUse` hook, matcher `.*`, callback `helm-subagent-budget`.
Calls with no `agent_id` and whose tool-use id was not seen in a sub-agent's assistant record (the
chat's own agent) are always allowed and never counted. For a sub-agent:
- Turns are its tool calls, counted from the hook. Context is read from the usage of its latest
  assistant record (`parent_tool_use_id` set): `input_tokens + cache_read_input_tokens +
  cache_creation_input_tokens`.
- First call: `additionalContext` asks for a short final report (a few lines, detail in a file).
- At the soft limit (default 100 calls, or 300k tokens of context): `additionalContext` telling it
  to finish its step, write a short hand-off and stop; repeated every five calls.
- Past 130% of either limit: with `subagentHardStop`, `permissionDecision: "deny"` with the same
  instruction (the sub-agent's reply is the way out); without it, the reminder continues.
- Only when limits apply, the chat's own agent's instructions get one paragraph saying so (and to ask sub-agents for short
  reports), appended with the long-command rule.
Not verified against a live CLI: that a sub-agent sees a PreToolUse `additionalContext` note (it is
documented for hooks in general; the fake CLI does not model sub-agent hooks).

**Tool output cap** (`worker/src/hosttools/cap.ts`). A result of the helm server's tools (host
tools and the MCP tools it proxies, including through `app_mcp_call`) longer than 5,000 characters
keeps its first ~3,250 and last ~1,750 characters around a marker saying how much was cut and
where the whole text is (a file under the system temp folder, `helm-tool-output`, cleared after a
day; if it cannot be written the marker says to ask for less). The structured copy of the result is
dropped when it is cut. Tool discovery (`app_list_tools`, `app_mcp_tools`) is exempt.

What cannot be done: Claude Code's built-in tools (Read, Bash, Grep and the rest) produce their
output inside the CLI. A `PostToolUse` hook may add context but, per the hook documentation, can
only replace the output of MCP tools (`updatedMCPToolOutput`), not of built-in ones. So built-in
tool output is not capped by Helm; the CLI's own limits apply.

## Landing guard

Registered in every chat with host tools: one `PreToolUse` entry, matcher `Bash|PowerShell|mcp__.*`,
callback `helm-landing-guard`, after the other entries (Claude Code runs every matching hook; any
deny wins).

| Line | Direction | Fields and behaviour |
|---|---|---|
| `guard_check` | worker → host | `id`, `kind` (`bash`, `powershell`, `mcp`), `root` (false when the hook input has an `agent_id` or the call came in a subagent's message), `turn` (changes when a turn ends), `cwd` (the hook input's), and `command` (cut just past 1 MiB) or `tool` and `input` (an input over 256 KiB is sent as `{}`). |
| `guard_verdict` | host → worker, no response | `id`, `allow`, `reason?` (the deny reason the agent reads), `context?` (added to an allowed call as `additionalContext`), `active` (the guard is in force for this chat). |

The host answers at once with `allow:true, active:false` in any chat without Orchestrator on and
an active task, or with the guard switched off. No answer within `limits.guardTimeoutMs` (10 s):
the call is refused with a try-again note if the last answer said `active`, and let through with a
logged warning otherwise.
