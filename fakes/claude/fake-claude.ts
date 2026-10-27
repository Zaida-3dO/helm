// Fake Claude Code CLI.
// Usage: node fake-claude.ts <fixture.json> [the real CLI's flags, which are logged and mostly ignored]
//
// It speaks stream-json on stdin/stdout and replays a scripted fixture. It answers the control
// requests a real CLI answers (initialize, set_model, apply_flag_settings, set_permission_mode,
// interrupt, cancel_async_message) and, when started with --replay-user-messages, acknowledges
// user messages the way claude 2.1.289 does (worker/PROTOCOL-NOTES.md, "Observed CLI behaviour"):
//   - every new uuid gets `command_lifecycle queued` at once;
//   - when no turn is running, the message is read at once: `command_lifecycle started` plus the
//     replayed user record, and a turn is now running;
//   - during a turn the message waits, unread, until the script reaches a `{"read": true}` step
//     (a tool boundary), or the turn's `result` (it then opens the next turn);
//   - `result` ends the turn and sends `command_lifecycle completed` for the messages it read.
// Everything it receives is appended to $FAKE_CLAUDE_LOG.
//
// Fixture: { "description", "replay"?: bool, "lifecycle"?: bool, "autoInterrupt"?: bool,
//            "autoBackground"?: bool (answer background_tasks with {backgrounded:true}), "steps": Step[] }
// Raw steps:
//   { "await": <partial record>, "as"?: name, "timeoutMs"?: n }  wait for a matching stdin record
//   { "emit": <record> }                    write one record; "{{name.path}}" strings are filled in
//                                           from saved records, "{{repeat:x:N}}" becomes N x's
//   { "emitRaw": text, "newline"?: bool }   write raw text (torn or non-JSON lines)
//   { "emitOversize": bytes }               write one valid JSON record of about this many bytes
//   { "sleep": ms }
//   { "exit": code }                        exit at once (a crash, mid-turn if you like)
//   { "hang": true }                        stop answering anything (a CLI that never settles)
// Control steps:
//   { "read": true }                        tool boundary: read every waiting message
//   { "reply": name, "response": {...} }    answer a saved control_request with success
//   { "if": Cond, "then": Step[], "else"?: Step[] }
//        Cond = { "saved": name, "match": pattern } | { "saved": name, "contains": text }
//             | { "flag": "--resume" } | { "unread": true }
//   { "repeat": Step[], "times"?: n }       loop (forever when no times: until stdin closes)
// Shorthand steps (session "fake-session" unless "sessionId" is set at the fixture top level):
//   { "init": { "tools"?: [], "permissionMode"?: s } }       system/init
//   { "text": s, "id"?: s }                 a streamed assistant text message
//   { "tool": { "id", "name", "input", "parent"? } }          an assistant tool_use message
//   { "toolResult": { "id", "text", "isError"?, "parent"? } }  the tool's result
//   { "taskStarted": { "taskId", "toolUseId", "background"?, "type"?, "prompt"?, "subagentType"? } }
//   { "taskProgress": { "taskId", "toolUseId", "description"?, "lastTool"?, "tokens"?, "toolUses"?, "durationMs"? } }
//   { "taskNotification": { "taskId", "toolUseId", "status", "summary"?, "tokens"?, "toolUses"?, "durationMs"? } }
//        a native subagent's task records, shaped like claude 2.1.289's (worker/PROTOCOL-NOTES.md, "Native subagents")
//   { "text": s, "parent": toolUseId }      a subagent's text message: one assistant record with
//                                           parent_tool_use_id and no partial stream, as the CLI sends
//   { "backgroundTasks": [ { "taskId", "description", "type"? } ] }
//   { "hook": { "toolUseId", "tool"?, "input", "callback"?, "agent"?, "cwd"? }, "as"?: name }
//        a PreToolUse hook_callback for the first PreToolUse callback the client registered in
//        initialize (or the one named by "callback", when the client registered it); with agent_id
//        when "agent" is set. The client's answer is saved under "as". No registration: nothing
//        is sent and "as" is saved as {"none": true}.
//   { "result": s, "isError"?: bool, "subtype"?: s }          end the turn
//   { "mcp": { "server"?: s, "message": <JSON-RPC>, "hook"?: { "toolUseId", "agent"? } }, "as"?: name }
//        an `mcp_message` control request to an SDK MCP server (default "helm"); with "hook", a
//        PreToolUse hook_callback for the tool (`mcp__<server>__<params.name>`) goes first, to the
//        callback the client registered for that matcher (with agent_id when "agent" is set).
//        The client's control_response is saved under "as" (default "mcp").

import fs from "node:fs";

type Json = Record<string, unknown>;
type Step = Json;

const fixturePath = process.argv[2];
if (!fixturePath) {
  process.stderr.write("fake-claude: fixture path required\n");
  process.exit(64);
}
const fixture = JSON.parse(fs.readFileSync(fixturePath, "utf8")) as {
  steps: Step[];
  replay?: boolean;
  lifecycle?: boolean;
  autoInterrupt?: boolean;
  autoBackground?: boolean;
  sessionId?: string;
};
const flags = process.argv.slice(3);
const logPath = process.env.FAKE_CLAUDE_LOG;
const SESSION = fixture.sessionId ?? "fake-session";

function log(entry: Json): void {
  if (logPath) fs.appendFileSync(logPath, JSON.stringify(entry) + "\n");
}
log({ kind: "start", argv: flags, envKeys: Object.keys(process.env).sort(), pid: process.pid });

const replay = fixture.replay !== false && flags.includes("--replay-user-messages");
const lifecycle = replay && fixture.lifecycle !== false;
const autoInterrupt = fixture.autoInterrupt !== false;
let hung = false;
let stdinClosed = false;
const seenUuids = new Set<string>();
const inbox: Json[] = [];
let waiter: { match: Json; resolve: (r: Json) => void } | undefined;
const saved: Record<string, Json> = {};
/** A turn is running: new user messages wait unread until a tool boundary. */
let busy = false;
/** Messages that arrived during a turn and have not been read. */
let unread: Json[] = [];
/** Messages read during the current turn (completed when it ends). */
let readThisTurn: string[] = [];
let msgSeq = 0;

/** While set, records are collected and written together (one chunk, as the real CLI often does). */
let batch: string[] | undefined;

function out(rec: Json): void {
  const line = JSON.stringify(rec) + "\n";
  if (batch) batch.push(line);
  else process.stdout.write(line);
}

function life(uuid: string, state: string): void {
  if (lifecycle) out({ type: "command_lifecycle", command_uuid: uuid, state, uuid: `life-${state}-${uuid}`, session_id: SESSION });
}

function readMessage(rec: Json): void {
  const uuid = String(rec.uuid ?? "");
  if (uuid) {
    life(uuid, "started");
    readThisTurn.push(uuid);
  }
  if (replay) out({ ...rec, isReplay: true });
}

function readAll(): void {
  const batch = unread;
  unread = [];
  for (const r of batch) readMessage(r);
}

function matches(rec: unknown, pattern: unknown): boolean {
  if (pattern === null || typeof pattern !== "object") return rec === pattern;
  if (rec === null || typeof rec !== "object") return false;
  for (const [k, v] of Object.entries(pattern as Json)) {
    if (!matches((rec as Json)[k], v)) return false;
  }
  return true;
}

function lookup(path: string): unknown {
  const [name, ...rest] = path.split(".");
  let cur: unknown = saved[name];
  for (const k of rest) cur = cur && typeof cur === "object" ? (cur as Json)[k] : undefined;
  return cur;
}

function fill(v: unknown): unknown {
  if (typeof v === "string") {
    const rep = /^\{\{repeat:(.):(\d+)\}\}$/.exec(v);
    if (rep) return rep[1].repeat(Number(rep[2]));
    const whole = /^\{\{([\w.]+)\}\}$/.exec(v);
    if (whole) return lookup(whole[1]);
    return v.replace(/\{\{([\w.]+)\}\}/g, (_m, p: string) => { const v = lookup(p); return v !== null && typeof v === "object" ? JSON.stringify(v) : String(v ?? ""); });
  }
  if (Array.isArray(v)) return v.map(fill);
  if (v && typeof v === "object") return Object.fromEntries(Object.entries(v).map(([k, x]) => [k, fill(x)]));
  return v;
}

function onUser(rec: Json): void {
  batch = [];
  try {
    onUserRecords(rec);
  } finally {
    const lines = batch;
    batch = undefined;
    if (lines.length) process.stdout.write(lines.join(""));
  }
}

function onUserRecords(rec: Json): void {
  if (!replay) return;
  const uuid = typeof rec.uuid === "string" ? rec.uuid : undefined;
  if (uuid && seenUuids.has(uuid)) return;
  if (uuid) {
    seenUuids.add(uuid);
    life(uuid, "queued");
  }
  if (!lifecycle) {
    // Lifecycle off: echo at once.
    out({ ...rec, isReplay: true });
    return;
  }
  if (busy) unread.push(rec);
  else {
    busy = true;
    readMessage(rec);
  }
}

function autoAnswer(rec: Json): void {
  if (rec.type === "user") return onUser(rec);
  if (rec.type !== "control_request") return;
  const req = (rec.request ?? {}) as Json;
  const ok = (response: Json) => out({ type: "control_response", response: { subtype: "success", request_id: rec.request_id, response } });
  switch (req.subtype) {
    case "initialize":
      saved.init = req;
      return ok({
        commands: Array.isArray((fixture as { commands?: unknown }).commands) ? (fixture as { commands: unknown[] }).commands : [],
        models: [
          { value: "default", displayName: "Default", description: "fake default" },
          { value: "haiku", displayName: "Haiku", description: "fake haiku" },
        ],
      });
    case "get_usage": {
      // The plan-usage meter's control request: answered from the fixture's top-level
      // "usage" (a response body, or {"error": text}); with none, nothing is answered.
      const u = (fixture as { usage?: unknown }).usage;
      if (u === undefined) return;
      if (u && typeof u === "object" && "error" in u) {
        return out({ type: "control_response", response: { subtype: "error", request_id: rec.request_id, error: String((u as { error: unknown }).error) } });
      }
      return ok(u as Json);
    }
    case "get_binary_version":
      return ok({ version: "0.0.0-fake" });
    case "set_model":
    case "apply_flag_settings":
    case "set_permission_mode":
      return ok({});
    case "background_tasks":
      if (fixture.autoBackground === true) return ok({ backgrounded: true });
      return;
    case "cancel_async_message": {
      const id = String(req.message_uuid ?? "");
      const i = unread.findIndex((u) => u.uuid === id);
      if (i === -1) return ok({ cancelled: false });
      unread.splice(i, 1);
      life(id, "cancelled");
      return ok({ cancelled: true });
    }
    case "interrupt": {
      if (!autoInterrupt) return;
      if (req.cancel_queued === true) {
        const cancelled = unread.map((u) => String(u.uuid ?? "")).filter(Boolean);
        unread = [];
        for (const id of cancelled) life(id, "cancelled");
        return ok({ still_queued: [], cancelled });
      }
      return ok({ still_queued: unread.map((u) => String(u.uuid ?? "")).filter(Boolean) });
    }
    default:
      return;
  }
}

function onRecord(rec: Json): void {
  log({ kind: "in", rec });
  if (hung) return;
  autoAnswer(rec);
  if (waiter && matches(rec, waiter.match)) {
    const w = waiter;
    waiter = undefined;
    w.resolve(rec);
    return;
  }
  inbox.push(rec);
}

function awaitRecord(match: Json, timeoutMs: number): Promise<Json> {
  const i = inbox.findIndex((r) => matches(r, match));
  if (i !== -1) return Promise.resolve(inbox.splice(i, 1)[0]);
  return new Promise((resolve, reject) => {
    const t = setTimeout(() => reject(new Error(`fake-claude: timed out awaiting ${JSON.stringify(match)}`)), timeoutMs);
    waiter = {
      match,
      resolve: (r) => {
        clearTimeout(t);
        resolve(r);
      },
    };
  });
}

let buf = "";
process.stdin.setEncoding("utf8");
process.stdin.on("data", (chunk: string) => {
  buf += chunk;
  let nl: number;
  while ((nl = buf.indexOf("\n")) !== -1) {
    const line = buf.slice(0, nl).trim();
    buf = buf.slice(nl + 1);
    if (!line) continue;
    try {
      onRecord(JSON.parse(line) as Json);
    } catch {
      log({ kind: "bad_input", line: line.slice(0, 200) });
    }
  }
});
process.stdin.on("end", () => {
  stdinClosed = true;
  log({ kind: "stdin_end" });
  if (!hung) process.exit(0);
});

// ---- shorthand records -------------------------------------------------------------------

const se = (event: Json, parent: unknown = null) => ({ type: "stream_event", event, parent_tool_use_id: parent, session_id: SESSION });

// Usage as the real CLI reports it. Each request's usage is the API's (snake_case): input and cache
// counts on message_start, the output count on message_delta. A result's usage is that turn's main
// conversation only, while total_cost_usd and modelUsage are running totals for the whole process.
const REQUEST_USAGE = { input_tokens: 10, output_tokens: 1, cache_read_input_tokens: 2, cache_creation_input_tokens: 1 };
const TURN_COST_USD = 0.001;
let running = { inputTokens: 0, outputTokens: 0, cacheReadInputTokens: 0, cacheCreationInputTokens: 0, costUSD: 0 };
const start = (id: string) => se({ type: "message_start", message: { id, model: "claude-fake", content: [], usage: REQUEST_USAGE } });

function endTurn(rec: Json): void {
  out(rec);
  const done = readThisTurn;
  readThisTurn = [];
  for (const id of done) life(id, "completed");
  busy = false;
  if (lifecycle && unread.length > 0) {
    // Messages still waiting open the next turn, as the real CLI's queue does.
    busy = true;
    readAll();
  }
}

function shorthand(step: Step): boolean {
  if (step.init) {
    const i = step.init as Json;
    out({ type: "system", subtype: "init", session_id: SESSION, model: "claude-fake", permissionMode: i.permissionMode ?? "default", tools: i.tools ?? [], capabilities: ["msg_lifecycle_v1", "interrupt_receipt_v1", "interrupt_cancel_queued_v1"] });
    return true;
  }
  if (typeof step.text === "string") {
    const id = typeof step.id === "string" ? step.id : `msg_fake_${++msgSeq}`;
    const text = fill(step.text) as string;
    if (typeof step.parent === "string") {
      out({ type: "assistant", message: { id, model: "claude-fake", content: [{ type: "text", text }] }, parent_tool_use_id: step.parent, session_id: SESSION });
      return true;
    }
    out(start(id));
    out(se({ type: "content_block_start", index: 0, content_block: { type: "text", text: "" } }));
    out(se({ type: "content_block_delta", index: 0, delta: { type: "text_delta", text } }));
    out(se({ type: "content_block_stop", index: 0 }));
    out(se({ type: "message_delta", delta: { stop_reason: "end_turn" }, usage: { output_tokens: 3 } }));
    out(se({ type: "message_stop" }));
    out({ type: "assistant", message: { id, model: "claude-fake", content: [{ type: "text", text }], usage: REQUEST_USAGE }, parent_tool_use_id: null, session_id: SESSION });
    return true;
  }
  if (step.tool) {
    const t = fill(step.tool) as Json;
    const id = `msg_fake_${++msgSeq}`;
    const parent = t.parent ?? null;
    if (!parent) out(start(id));
    out({ type: "assistant", message: { id, model: "claude-fake", content: [{ type: "tool_use", id: t.id, name: t.name, input: t.input ?? {} }] }, parent_tool_use_id: parent, session_id: SESSION });
    if (!parent) out(se({ type: "message_stop" }));
    return true;
  }
  if (step.toolResult) {
    const r = fill(step.toolResult) as Json;
    out({
      type: "user",
      message: { role: "user", content: [{ type: "tool_result", tool_use_id: r.id, content: [{ type: "text", text: r.text ?? "" }], is_error: r.isError === true }] },
      parent_tool_use_id: r.parent ?? null,
      session_id: SESSION,
    });
    return true;
  }
  if (step.taskStarted) {
    const t = fill(step.taskStarted) as Json;
    const rec: Json = { type: "system", subtype: "task_started", task_id: t.taskId, tool_use_id: t.toolUseId, description: t.description ?? "task", is_backgrounded: t.background === true, task_type: t.type ?? "local_bash", session_id: SESSION };
    if (t.prompt !== undefined) rec.prompt = t.prompt;
    if (t.subagentType !== undefined) rec.subagent_type = t.subagentType;
    out(rec);
    return true;
  }
  if (step.taskProgress || step.taskNotification) {
    const t = fill(step.taskProgress ?? step.taskNotification) as Json;
    const usage = { total_tokens: t.tokens ?? 0, tool_uses: t.toolUses ?? 0, duration_ms: t.durationMs ?? 0 };
    if (step.taskProgress) {
      out({ type: "system", subtype: "task_progress", task_id: t.taskId, tool_use_id: t.toolUseId, description: t.description ?? "", usage, last_tool_name: t.lastTool, session_id: SESSION });
    } else {
      out({ type: "system", subtype: "task_updated", task_id: t.taskId, patch: { status: t.status === "stopped" ? "killed" : t.status }, session_id: SESSION });
      out({ type: "system", subtype: "task_notification", task_id: t.taskId, tool_use_id: t.toolUseId, status: t.status, summary: t.summary ?? "", usage, session_id: SESSION });
    }
    return true;
  }
  if (Array.isArray(step.backgroundTasks)) {
    const tasks = (fill(step.backgroundTasks) as Json[]).map((t) => ({ task_id: t.taskId, task_type: t.type ?? "local_bash", description: t.description ?? "" }));
    out({ type: "system", subtype: "background_tasks_changed", tasks, session_id: SESSION });
    return true;
  }
  if (typeof step.result === "string") {
    const isError = step.isError === true;
    const turn = { input_tokens: 10, output_tokens: 5, cache_read_input_tokens: 0, cache_creation_input_tokens: 0 };
    running = {
      inputTokens: running.inputTokens + turn.input_tokens,
      outputTokens: running.outputTokens + turn.output_tokens,
      cacheReadInputTokens: running.cacheReadInputTokens + turn.cache_read_input_tokens,
      cacheCreationInputTokens: running.cacheCreationInputTokens + turn.cache_creation_input_tokens,
      costUSD: Math.round((running.costUSD + TURN_COST_USD) * 1e6) / 1e6,
    };
    endTurn({
      type: "result",
      subtype: typeof step.subtype === "string" ? step.subtype : isError ? "error_during_execution" : "success",
      is_error: isError,
      result: fill(step.result),
      session_id: SESSION,
      usage: turn,
      total_cost_usd: running.costUSD,
      modelUsage: { "claude-fake": running },
      duration_ms: 5,
      num_turns: 1,
    });
    return true;
  }
  return false;
}

function hookCallbackId(tool?: string): string | undefined {
  const hooks = (saved.init?.hooks ?? {}) as Json;
  const pre = Array.isArray(hooks.PreToolUse) ? (hooks.PreToolUse as Json[]) : [];
  for (const m of pre) {
    if (tool !== undefined && !(typeof m.matcher === "string" && new RegExp(`^(?:${m.matcher})$`).test(tool))) continue;
    const ids = m.hookCallbackIds;
    if (Array.isArray(ids) && typeof ids[0] === "string") return ids[0];
  }
  return undefined;
}

function registeredCallback(id: string): boolean {
  const hooks = (saved.init?.hooks ?? {}) as Json;
  const pre = Array.isArray(hooks.PreToolUse) ? (hooks.PreToolUse as Json[]) : [];
  return pre.some((m) => Array.isArray(m.hookCallbackIds) && (m.hookCallbackIds as unknown[]).includes(id));
}

function cond(c: Json): boolean {
  if (typeof c.flag === "string") return flags.includes(c.flag);
  if (c.unread === true) return unread.length > 0;
  const s = typeof c.saved === "string" ? saved[c.saved] : undefined;
  if (typeof c.contains === "string") return s !== undefined && JSON.stringify(s).includes(c.contains);
  if (c.match !== undefined) return s !== undefined && matches(s, c.match);
  return false;
}

let hookSeq = 0;

/** Runs steps; resolves false when the script must stop (exit or hang). */
async function run(steps: Step[]): Promise<boolean> {
  for (const step of steps) {
    if (step.await) {
      const rec = await awaitRecord(fill(step.await) as Json, Number(step.timeoutMs ?? 10_000));
      saved.last = rec;
      if (typeof step.as === "string") saved[step.as] = rec;
    } else if (step.emit) {
      const rec = fill(step.emit) as Json;
      if (rec.type === "result") endTurn(rec);
      else out(rec);
    } else if (typeof step.emitRaw === "string") {
      process.stdout.write(step.emitRaw + (step.newline === false ? "" : "\n"));
    } else if (typeof step.emitOversize === "number") {
      const pad = "x".repeat(Math.max(0, step.emitOversize));
      out({ type: "system", subtype: "fake_padding", pad });
    } else if (typeof step.sleep === "number") {
      await new Promise((r) => setTimeout(r, step.sleep as number));
    } else if (typeof step.exit === "number") {
      log({ kind: "exit", code: step.exit });
      // Flush stdout before dying so the worker sees what came before the crash.
      process.stdout.write("", () => process.exit(step.exit as number));
      return false;
    } else if (step.hang) {
      hung = true;
      log({ kind: "hang" });
      setInterval(() => {}, 60_000); // keep the process alive until killed
      return false;
    } else if (step.read) {
      readAll();
    } else if (typeof step.reply === "string") {
      const req = saved[step.reply];
      if (!req) throw new Error(`fake-claude: reply names "${step.reply}", which was never saved`);
      out({ type: "control_response", response: { subtype: "success", request_id: req?.request_id, response: fill(step.response ?? {}) } });
    } else if (step.mcp) {
      const m = fill(step.mcp) as Json;
      const server = typeof m.server === "string" ? m.server : "helm";
      const message = (m.message ?? {}) as Json;
      const params = (message.params ?? {}) as Json;
      const as = typeof step.as === "string" ? step.as : "mcp";
      const hk = m.hook as Json | undefined;
      if (hk) {
        const tool = `mcp__${server}__${String(params.name ?? "")}`;
        const cb = hookCallbackId(tool);
        if (cb) {
          const request_id = `fake-hook-${++hookSeq}`;
          const input: Json = { session_id: SESSION, hook_event_name: "PreToolUse", tool_name: tool, tool_input: params.arguments ?? {}, tool_use_id: hk.toolUseId, permission_mode: "default" };
          if (typeof hk.agent === "string") input.agent_id = hk.agent;
          out({ type: "control_request", request_id, request: { subtype: "hook_callback", callback_id: cb, input, tool_use_id: hk.toolUseId } });
          await awaitRecord({ type: "control_response", response: { request_id } }, Number(step.timeoutMs ?? 10_000));
        }
      }
      const request_id = `fake-mcp-${++hookSeq}`;
      out({ type: "control_request", request_id, request: { subtype: "mcp_message", server_name: server, message } });
      saved[as] = await awaitRecord({ type: "control_response", response: { request_id } }, Number(step.timeoutMs ?? 30_000));
    } else if (step.hook) {
      const h = fill(step.hook) as Json;
      const as = typeof step.as === "string" ? step.as : "hook";
      const cb = typeof h.callback === "string" ? (registeredCallback(h.callback) ? h.callback : undefined) : hookCallbackId();
      if (!cb) {
        saved[as] = { none: true };
        continue;
      }
      const request_id = `fake-hook-${++hookSeq}`;
      out({
        type: "control_request",
        request_id,
        request: {
          subtype: "hook_callback",
          callback_id: cb,
          input: {
            session_id: SESSION,
            hook_event_name: "PreToolUse",
            tool_name: h.tool ?? "Bash",
            tool_input: h.input ?? {},
            tool_use_id: h.toolUseId,
            permission_mode: "default",
            ...(typeof h.agent === "string" ? { agent_id: h.agent } : {}),
            ...(typeof h.cwd === "string" ? { cwd: h.cwd } : {}),
          },
          tool_use_id: h.toolUseId,
        },
      });
      saved[as] = await awaitRecord({ type: "control_response", response: { request_id } }, Number(step.timeoutMs ?? 10_000));
    } else if (step.if) {
      const branch = cond(step.if as Json) ? step.then : step.else;
      if (Array.isArray(branch) && !(await run(branch as Step[]))) return false;
    } else if (Array.isArray(step.repeat)) {
      const times = typeof step.times === "number" ? step.times : Infinity;
      for (let i = 0; i < times && !stdinClosed; i++) {
        if (!(await run(step.repeat as Step[]))) return false;
      }
    } else if (!shorthand(step)) {
      throw new Error(`fake-claude: unknown step ${JSON.stringify(step).slice(0, 200)}`);
    }
  }
  return true;
}

run(fixture.steps)
  .then((finished) => {
    if (finished) log({ kind: "script_done" });
  })
  .catch((e: Error) => {
    log({ kind: "script_error", message: e.message });
    process.stderr.write(`${e.message}\n`);
    process.exit(70);
  });
