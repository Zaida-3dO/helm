// The Claude Code driver: one claude CLI child per worker, spoken to over its stream-json control
// channel. Submissions, stop, model/effort, interactions and process control.

import { spawn, type ChildProcess } from "node:child_process";
import { randomUUID } from "node:crypto";
import type { HostChannel, ResponseBody } from "../../protocol/channel.ts";
import { describeFault, LineReader, MAX_RECORD_BYTES } from "../../protocol/lines.ts";
import type { Driver } from "../../protocol/server.ts";
import { DEFAULT_EFFORT, isEffort, type Json, type Launch, type UiResponse } from "../../protocol/types.ts";
import { isAlive, treeKill } from "../../process/kill.ts";
import { buildClaudeArgs, modelTakesEffort } from "./args.ts";
import { buildClaudeEnv } from "./env.ts";
import { Interactions } from "./interactions.ts";
import { isLongCommandLevel, LongCommandPolicy, PROMPT_RULE } from "./longcmd.ts";
import { assertNotShim, resolveClaude, type Resolved } from "./resolve.ts";
import { StreamTranslator } from "./translate.ts";

/** The largest prompt text the worker will submit. */
export const MAX_SUBMISSION_TEXT_BYTES = 2 * 1024 * 1024;
const STDERR_KEEP = 16 * 1024;
const DEFAULTS = {
  promptAckMs: 30_000,
  stopSettleMs: 30_000,
  controlTimeoutMs: 60_000,
  modelAckMs: 10_000,
  shutdownGraceMs: 3_000,
};
/** Short limits for the send-behaviour controls: a slow answer must not hold up the host. */
const QUICK_CONTROL_MS = 5_000;
const VERSION_PROBE_MS = 3_000;
/** A stop_tool line older than this is refused. */
const STOP_TOOL_MAX_AGE_MS = 5_000;
const DEFAULT_MIN_RUN_MS = 10_000;
/** The callback id the worker registers for its PreToolUse hook (long-command rule). */
export const LONG_COMMAND_HOOK_ID = "helm-long-commands";

const UUID_RE = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i;

interface Submission {
  id: string;
  state: "dispatching" | "accepted" | "uncertain" | "rejected";
  result: Promise<ResponseBody>;
  settle?: (b: ResponseBody) => void;
  timer?: NodeJS.Timeout;
  text: string;
  /** Claude has read it (lifecycle `started`, or the echo on a CLI without lifecycle records). */
  delivered: boolean;
  /** Withdrawn by the user, or dropped by the CLI on an interrupt. */
  dropped: boolean;
  /** The prompt's response has been written to the host. */
  answered: boolean;
  /** Read before the response went out: `submission_delivered` waits for the response. */
  deliveryHeld: boolean;
}

/** A root tool call that is running. */
interface LiveTool {
  id: string;
  name: string;
  startedAt: number;
  /** Set once the CLI has registered the call as a foreground task (about 3–4 s in). */
  taskId?: string;
  taskKind?: "shell" | "agent";
  background: boolean;
  command?: string;
}

interface BackgroundTask {
  id: string;
  title: string;
  command?: string;
  kind: "shell" | "agent";
  startedAt: number;
}

interface PendingControl {
  resolve: (r: Json) => void;
  reject: (e: Error) => void;
  timer: NodeJS.Timeout;
}

export interface DriverDeps {
  env: Record<string, string | undefined>;
  diag: (msg: string) => void;
  /** Called after a fatal error has been reported; the worker should exit with this code. */
  onFatal: (code: number) => void;
}

export class ClaudeDriver implements Driver {
  private readonly launch: Launch;
  private readonly ch: HostChannel;
  private readonly deps: DriverDeps;
  private readonly limits: typeof DEFAULTS & { maxRecordBytes: number };
  private child: ChildProcess | undefined;
  private exited: Promise<void> = Promise.resolve();
  private readyP: Promise<Json> | undefined;
  private stderrTail = "";
  private nextControl = 1;
  private readonly controls = new Map<string, PendingControl>();
  private readonly subs = new Map<string, Submission>();
  private readonly translator: StreamTranslator;
  private readonly interactions: Interactions;
  private turnOpen = false;
  private stopping = false;
  private stopTimer: NodeJS.Timeout | undefined;
  private shuttingDown = false;
  private dead = false;
  private sessionId: string | undefined;
  private model: string | undefined;
  private effort: string;
  private pendingModel: string | undefined;
  private pendingEffort: string | undefined;
  private resolved: Resolved | undefined;
  private readonly longCommands: LongCommandPolicy;
  private readonly liveTools = new Map<string, LiveTool>();
  /** task id → tool call id, from task_started. */
  private readonly taskTools = new Map<string, string>();
  /** Tool call ids that belong to subagents (exempt from the long-command rule). */
  private readonly subagentTools = new Set<string>();
  private readonly backgroundTasks = new Map<string, BackgroundTask>();
  /** A send-now interrupt is in flight: the turn ends superseded, not stopped by the user. */
  private superseding = false;
  private compacting: { boundary: boolean } | undefined;

  constructor(launch: Launch, channel: HostChannel, deps: DriverDeps) {
    this.launch = launch;
    this.ch = channel;
    this.deps = deps;
    this.limits = { ...DEFAULTS, maxRecordBytes: MAX_RECORD_BYTES, ...stripUndefined(launch.limits ?? {}) };
    const c = launch.claude ?? {};
    this.model = c.model;
    this.effort = isEffort(c.effort) ? c.effort : DEFAULT_EFFORT;
    const level = launch.sendBehavior?.longCommands;
    this.longCommands = new LongCommandPolicy(isLongCommandLevel(level) ? level : "off");
    this.translator = new StreamTranslator({ emit: (e) => this.onTranslated(e), diag: deps.diag });
    this.interactions = new Interactions({
      emit: (e) => this.ch.emit(e),
      reply: (requestId, response) => this.controlReply(requestId, response),
      onStopTurn: () => this.markStopping(),
      skipPermissions: c.skipPermissions === true,
    });
  }

  // ---- lifecycle -------------------------------------------------------------------------

  ready(): Promise<Json> {
    if (!this.readyP) this.readyP = this.start();
    return this.readyP;
  }

  private async start(): Promise<Json> {
    const c = this.launch.claude ?? {};
    let command: string;
    if (c.command) {
      command = assertNotShim(c.command, "launch claude.command");
      this.resolved = { path: command, source: "launch" };
    } else {
      this.resolved = resolveClaude({ env: this.deps.env });
      command = this.resolved.path;
    }
    const args = [...(c.commandArgs ?? []), ...buildClaudeArgs(c, { promptRule: this.longCommands.enabled ? PROMPT_RULE : undefined })];
    const env = buildClaudeEnv(this.deps.env, { apiKeyMode: c.apiKeyMode === true, chatId: this.launch.chatId });
    const child = spawn(command, args, {
      cwd: this.launch.cwd ?? process.cwd(),
      env,
      stdio: ["pipe", "pipe", "pipe"],
      windowsHide: true,
      detached: process.platform !== "win32", // own process group on POSIX for tree kill
    });
    this.child = child;
    this.exited = new Promise((resolve) => child.once("exit", () => resolve()));
    child.once("error", (e) => this.fatal(`could not start claude (${command}): ${e.message}`));
    // "close" (not "exit"): fires after stdout has drained, so a torn last line is reported as
    // such rather than as a bare unexpected exit.
    child.once("close", (code, signal) => {
      if (this.shuttingDown || this.dead) return;
      const tail = this.stderrTail.trim();
      this.fatal(`claude exited unexpectedly (code ${code ?? "none"}${signal ? `, signal ${signal}` : ""})${tail ? `: ${tail.slice(-2000)}` : ""}`);
    });
    child.stderr?.on("data", (d: Buffer) => {
      this.stderrTail = (this.stderrTail + d.toString("utf8")).slice(-STDERR_KEEP);
    });
    const reader = new LineReader(
      {
        onRecord: (r) => this.onClaudeRecord(r),
        onFault: (f) => this.fatal(`claude sent a bad record: ${describeFault(f)}`),
      },
      this.limits.maxRecordBytes,
    );
    child.stdout?.on("data", (d: Buffer) => reader.push(d));
    child.stdout?.on("end", () => reader.end());
    child.stdin?.on("error", (e) => this.deps.diag(`claude stdin: ${e.message}`));

    const initRequest: Json = { subtype: "initialize" };
    if (this.longCommands.enabled) initRequest.hooks = { PreToolUse: [{ matcher: "Bash", hookCallbackIds: [LONG_COMMAND_HOOK_ID] }] };
    const init = await this.control(initRequest, this.limits.controlTimeoutMs);
    const models = Array.isArray(init.models)
      ? (init.models as Json[]).map((m) => ({ id: m.value ?? m.id, name: m.displayName ?? m.value, description: m.description }))
      : [];
    let cliVersion: string | null = null;
    try {
      const v = await this.control({ subtype: "get_binary_version" }, VERSION_PROBE_MS);
      if (typeof v.version === "string") cliVersion = v.version;
    } catch {
      // An older CLI without the request: the version stays unknown.
    }
    return {
      harness: "claude",
      executable: this.resolved.path,
      executableSource: this.resolved.source,
      cliVersion,
      model: this.model ?? null,
      effort: modelTakesEffort(this.model) ? this.effort : null,
      models,
      // The CLI's own commands and skills, invoked as "/name" at the start of a message.
      skills: Array.isArray(init.commands)
        ? (init.commands as Json[])
            .filter((c) => typeof c.name === "string" && c.name)
            .slice(0, 500)
            .map((c) => ({ name: String(c.name), description: typeof c.description === "string" ? c.description.slice(0, 500) : "" }))
        : [],
      // All four were observed working on claude 2.1.289 (worker/PROTOCOL-NOTES.md).
      capabilities: { withdraw: true, deliverNow: true, stopTool: true, compact: true },
    };
  }

  /** Events from the translator, with the tool bookkeeping the send-behaviour controls need. */
  private onTranslated(e: Json): void {
    if (e.type === "tool_execution_start") {
      const id = String(e.toolCallId);
      const args = obj(e.args);
      this.liveTools.set(id, {
        id,
        name: String(e.toolName ?? ""),
        startedAt: Date.now(),
        background: false,
        command: typeof args.command === "string" ? args.command : undefined,
      });
      this.ch.emit({ ...e, stoppable: false });
      return;
    }
    if (e.type === "tool_execution_end") {
      const t = this.liveTools.get(String(e.toolCallId));
      if (t?.taskId) this.taskTools.delete(t.taskId);
      this.liveTools.delete(String(e.toolCallId));
    }
    if (e.type === "compaction_end") {
      if (this.compacting) this.compacting.boundary = true;
      this.ch.emit({ ...e, ok: true });
      return;
    }
    this.ch.emit(e);
  }

  async shutdown(reason: string): Promise<void> {
    if (this.shuttingDown) return this.exited;
    this.shuttingDown = true;
    this.deps.diag(`claude driver shutting down: ${reason}`);
    clearTimeout(this.stopTimer);
    for (const s of this.subs.values()) {
      if (s.state === "dispatching") this.settleSub(s, { success: false, uncertain: true, error: "worker shut down before Claude acknowledged the message" }, "uncertain");
    }
    const child = this.child;
    if (!child || child.exitCode !== null || this.dead) return;
    if (this.turnOpen) this.write({ type: "control_request", request_id: this.newId(), request: { subtype: "interrupt" } });
    child.stdin?.end();
    const timeout = new Promise<"timeout">((r) => setTimeout(() => r("timeout"), this.limits.shutdownGraceMs).unref());
    const how = await Promise.race([this.exited.then(() => "exited" as const), timeout]);
    if (how === "timeout" || isAlive(child.pid)) treeKill(child.pid);
    await Promise.race([this.exited, new Promise((r) => setTimeout(r, 2000).unref())]);
  }

  /** Reports a fatal error once, ends the harness tree, and asks the worker to exit. */
  private fatal(message: string): void {
    if (this.dead || this.shuttingDown) return;
    this.dead = true;
    clearTimeout(this.stopTimer);
    this.translator.abandon("The worker lost Claude Code before this finished.");
    this.interactions.dismissAll();
    for (const c of this.controls.values()) {
      clearTimeout(c.timer);
      c.reject(new Error(message));
    }
    this.controls.clear();
    for (const s of this.subs.values()) {
      if (s.state === "dispatching") this.settleSub(s, { success: false, uncertain: true, error: message }, "uncertain");
    }
    if (this.turnOpen) {
      this.turnOpen = false;
      this.ch.emit({ type: "agent_settled", outcome: "interrupted", error: message });
    }
    this.ch.emit({ type: "worker_error", error: message });
    if (this.child && this.child.exitCode === null) treeKill(this.child.pid);
    // Let pending responses (resolved above) reach the channel before the worker exits.
    setImmediate(() => this.deps.onFatal(1));
  }

  // ---- wire helpers ----------------------------------------------------------------------

  private newId(): string {
    return `helm-${this.nextControl++}`;
  }

  private write(rec: Json): void {
    const stdin = this.child?.stdin;
    if (!stdin || stdin.destroyed || this.dead) throw new Error("claude is not running");
    stdin.write(JSON.stringify(rec) + "\n");
  }

  private control(request: Json, timeoutMs: number): Promise<Json> {
    const request_id = this.newId();
    return new Promise((resolve, reject) => {
      const timer = setTimeout(() => {
        this.controls.delete(request_id);
        reject(new Error(`claude did not answer ${String(request.subtype)} within ${timeoutMs} ms`));
      }, timeoutMs);
      this.controls.set(request_id, { resolve, reject, timer });
      try {
        this.write({ type: "control_request", request_id, request });
      } catch (e) {
        clearTimeout(timer);
        this.controls.delete(request_id);
        reject(e as Error);
      }
    });
  }

  private controlReply(requestId: string, response: Json): void {
    try {
      this.write({ type: "control_response", response: { subtype: "success", request_id: requestId, response } });
    } catch (e) {
      this.deps.diag(`could not answer ${requestId}: ${(e as Error).message}`);
    }
  }

  private controlError(requestId: string, error: string): void {
    try {
      this.write({ type: "control_response", response: { subtype: "error", request_id: requestId, error } });
    } catch (e) {
      this.deps.diag(`could not answer ${requestId}: ${(e as Error).message}`);
    }
  }

  // ---- inbound from claude ---------------------------------------------------------------

  private onClaudeRecord(rec: Json): void {
    if (this.dead) return;
    if (typeof rec.session_id === "string" && rec.session_id && rec.session_id !== this.sessionId) {
      this.sessionId = rec.session_id;
      this.ch.emit({ type: "native_session", sessionId: this.sessionId });
    }
    switch (rec.type) {
      case "control_response":
        return this.onControlResponse(obj(rec.response));
      case "control_request":
        return this.onControlRequest(String(rec.request_id ?? ""), obj(rec.request));
      case "control_cancel_request":
        return this.interactions.cancel(String(rec.request_id ?? ""));
      case "user":
        if (this.onUserEcho(rec)) return;
        this.openTurnIfNeeded();
        this.translator.handle(rec);
        return;
      case "result":
        return this.onResult(rec);
      case "command_lifecycle":
        return this.onLifecycle(rec);
      case "system":
        if (rec.subtype === "init" && typeof rec.model === "string") this.model = rec.model;
        this.onTaskRecord(rec);
        this.translator.handle(rec);
        return;
      case "stream_event":
      case "assistant":
        if (rec.parent_tool_use_id) this.noteSubagentTools(rec);
        else this.openTurnIfNeeded();
        this.translator.handle(rec);
        return;
      default:
        this.translator.handle(rec);
    }
  }

  private onControlResponse(r: Json): void {
    const id = String(r.request_id ?? "");
    const p = this.controls.get(id);
    if (!p) return;
    this.controls.delete(id);
    clearTimeout(p.timer);
    if (r.subtype === "success") p.resolve(obj(r.response));
    else p.reject(new Error(String(r.error ?? "control request failed")));
  }

  private onControlRequest(requestId: string, req: Json): void {
    switch (req.subtype) {
      case "can_use_tool":
        if (this.stopping) return this.controlReply(requestId, { behavior: "deny", message: "The turn is stopping." });
        return this.interactions.onCanUseTool(requestId, req);
      case "hook_callback":
        return this.onHook(requestId, req);
      case "elicitation":
        return this.controlReply(requestId, { action: "decline" });
      default:
        return this.controlError(requestId, `unsupported control request: ${String(req.subtype)}`);
    }
  }

  /**
   * A replayed user message carrying one of our submission ids. The CLI only replays a message
   * when it reads it, so on a CLI without lifecycle records the echo is both ack and delivery.
   */
  private onUserEcho(rec: Json): boolean {
    const uuid = typeof rec.uuid === "string" ? rec.uuid : undefined;
    if (!uuid) return false;
    const sub = this.subs.get(uuid);
    if (!sub) return false;
    this.ackSub(sub);
    this.deliverSub(sub);
    return true;
  }

  /** `command_lifecycle`: queued is the acknowledgement, started is the read (worker/PROTOCOL-NOTES.md). */
  private onLifecycle(rec: Json): void {
    const sub = this.subs.get(String(rec.command_uuid ?? ""));
    if (!sub) return; // internal messages (/compact) and uuids we never sent
    switch (rec.state) {
      case "queued":
        return this.ackSub(sub);
      case "started":
        this.ackSub(sub);
        return this.deliverSub(sub);
      case "cancelled":
        sub.dropped = true;
        return;
      default:
        return;
    }
  }

  private ackSub(sub: Submission): void {
    if (sub.state !== "dispatching") return;
    this.settleSub(sub, { success: true, data: { submissionId: sub.id, state: "accepted" } }, "accepted");
  }

  /** Claude has read the message: its user row goes into the transcript here, in reading order. */
  private deliverSub(sub: Submission): void {
    if (sub.delivered || sub.dropped) return;
    sub.delivered = true;
    this.openTurnIfNeeded();
    this.ch.emit({ type: "message_start", message: { id: sub.id, role: "user", content: [] } });
    this.ch.emit({ type: "message_end", message: { id: sub.id, role: "user", content: [{ type: "text", text: sub.text }], status: "complete", submissionId: sub.id } });
    // The host only moves an accepted submission to delivered, so the acknowledgement must reach
    // it first; for a turn-opening prompt the read can land in the same tick as the ack.
    if (sub.answered) this.ch.emit({ type: "submission_delivered", submissionId: sub.id });
    else sub.deliveryHeld = true;
  }

  promptAnswered(submissionId: unknown): void {
    const sub = typeof submissionId === "string" ? this.subs.get(submissionId) : undefined;
    if (!sub || sub.answered) return;
    sub.answered = true;
    if (sub.deliveryHeld) {
      sub.deliveryHeld = false;
      this.ch.emit({ type: "submission_delivered", submissionId: sub.id });
    }
  }

  /** task_started / task_updated / background_tasks_changed (worker/PROTOCOL-NOTES.md, "Observed CLI behaviour"). */
  private onTaskRecord(rec: Json): void {
    switch (rec.subtype) {
      case "task_started": {
        const taskId = String(rec.task_id ?? "");
        const toolId = String(rec.tool_use_id ?? "");
        const t = this.liveTools.get(toolId);
        if (!taskId || !t) return;
        t.taskId = taskId;
        t.taskKind = rec.task_type === "local_agent" ? "agent" : "shell";
        t.background = rec.is_backgrounded === true;
        this.taskTools.set(taskId, toolId);
        if (!t.background && t.taskKind === "shell") this.ch.emit({ type: "tool_execution_update", toolCallId: toolId, stoppable: true });
        return;
      }
      case "task_updated": {
        const toolId = this.taskTools.get(String(rec.task_id ?? ""));
        const t = toolId ? this.liveTools.get(toolId) : undefined;
        if (t && obj(rec.patch).is_backgrounded === true && !t.background) {
          t.background = true;
          if (t.taskKind === "shell") this.ch.emit({ type: "tool_execution_update", toolCallId: t.id, stoppable: false });
        }
        return;
      }
      case "background_tasks_changed": {
        const now = Date.now();
        const next = new Map<string, BackgroundTask>();
        for (const raw of Array.isArray(rec.tasks) ? rec.tasks : []) {
          const x = obj(raw);
          if (x.ambient === true) continue;
          const id = String(x.task_id ?? "");
          if (!id) continue;
          const prev = this.backgroundTasks.get(id);
          const toolId = this.taskTools.get(id);
          next.set(id, {
            id,
            title: String(x.description ?? ""),
            command: prev?.command ?? (toolId ? this.liveTools.get(toolId)?.command : undefined),
            kind: x.task_type === "local_agent" ? "agent" : "shell",
            startedAt: prev?.startedAt ?? now,
          });
        }
        this.backgroundTasks.clear();
        for (const [k, v] of next) this.backgroundTasks.set(k, v);
        this.ch.emit({ type: "background_tasks", tasks: [...next.values()] });
        return;
      }
      default:
        return;
    }
  }

  private noteSubagentTools(rec: Json): void {
    if (rec.type !== "assistant") return;
    const content = obj(rec.message).content;
    for (const b of Array.isArray(content) ? content : []) {
      const block = obj(b);
      if (block.type === "tool_use" && typeof block.id === "string") this.subagentTools.add(block.id);
    }
  }

  /** PreToolUse hook: the long-command rule. Anything else is allowed. */
  private onHook(requestId: string, req: Json): void {
    const input = obj(req.input);
    const toolUseId = String(req.tool_use_id ?? input.tool_use_id ?? "");
    const isRootBash = req.callback_id === LONG_COMMAND_HOOK_ID && input.tool_name === "Bash" && !input.agent_id && !this.subagentTools.has(toolUseId);
    if (!isRootBash) return this.controlReply(requestId, {});
    const d = this.longCommands.decide(obj(input.tool_input));
    switch (d.kind) {
      case "allow":
        return this.controlReply(requestId, {});
      case "escape":
        this.ch.emit({ type: "long_command", toolCallId: toolUseId, action: "escaped", reason: d.reason });
        return this.controlReply(requestId, {});
      case "remind":
        this.ch.emit({ type: "long_command", toolCallId: toolUseId, action: "reminded", reason: d.reason });
        return this.controlReply(requestId, { hookSpecificOutput: { hookEventName: "PreToolUse", additionalContext: d.text } });
      case "deny":
        this.ch.emit({ type: "long_command", toolCallId: toolUseId, action: "denied", reason: d.reason });
        return this.controlReply(requestId, { hookSpecificOutput: { hookEventName: "PreToolUse", permissionDecision: "deny", permissionDecisionReason: d.text } });
    }
  }

  private openTurnIfNeeded(): void {
    if (this.turnOpen) return;
    this.turnOpen = true;
    this.ch.emit({ type: "agent_start" });
  }

  private onResult(rec: Json): void {
    const wasStopping = this.stopping;
    const superseded = this.superseding && !wasStopping;
    this.superseding = false;
    const outcome = this.translator.onResult(rec, wasStopping || superseded);
    clearTimeout(this.stopTimer);
    this.stopping = false;
    this.interactions.dismissAll();
    this.longCommands.endTurn();
    if (this.compacting) {
      if (!this.compacting.boundary) this.ch.emit({ type: "compaction_end", trigger: "manual", ok: false });
      this.compacting = undefined;
    }
    const wasOpen = this.turnOpen;
    this.turnOpen = false;
    if (!wasOpen && !wasStopping) return;
    const e: Json = { type: "agent_settled", outcome };
    // Send now ended the turn: the host shows it as superseded by the user's message.
    if (superseded) e.superseded = true;
    if (outcome === "error") e.error = typeof rec.result === "string" && rec.result ? rec.result : String(rec.subtype ?? "error");
    if (this.sessionId) e.sessionId = this.sessionId;
    this.ch.emit(e);
  }

  private settleSub(s: Submission, body: ResponseBody, state: Submission["state"]): void {
    s.state = state;
    clearTimeout(s.timer);
    const settle = s.settle;
    s.settle = undefined;
    settle?.(body);
  }

  // ---- host commands ---------------------------------------------------------------------

  async prompt(cmd: Json): Promise<ResponseBody> {
    if (this.shuttingDown || this.dead) return { success: false, error: "task is finishing, send again in a moment" };
    const sid = typeof cmd.submissionId === "string" ? cmd.submissionId : undefined;
    if (!sid || !UUID_RE.test(sid)) return { success: false, error: "prompt needs a submissionId that is a UUID" };
    const existing = this.subs.get(sid);
    if (existing) return existing.result; // idempotent: never sent twice
    const text = typeof cmd.text === "string" ? cmd.text : "";
    if (Buffer.byteLength(text, "utf8") > MAX_SUBMISSION_TEXT_BYTES) {
      return { success: false, error: "message text is over the 2 MB limit" };
    }

    const content: Json[] = [];
    if (text) content.push({ type: "text", text });
    // The panel-context block: its own content block after the text.
    if (typeof cmd.context === "string" && cmd.context.length > 0) content.push({ type: "text", text: cmd.context });
    for (const img of Array.isArray(cmd.images) ? (cmd.images as Json[]) : []) {
      if (typeof img.data === "string") {
        content.push({ type: "image", source: { type: "base64", media_type: String(img.mimeType ?? "image/png"), data: img.data } });
      }
    }
    if (content.length === 0) return { success: false, error: "prompt has no text or images" };

    let settle!: (b: ResponseBody) => void;
    const result = new Promise<ResponseBody>((r) => (settle = r));
    const sub: Submission = { id: sid, state: "dispatching", result, settle, text, delivered: false, dropped: false, answered: false, deliveryHeld: false };
    this.subs.set(sid, sub);

    if (!this.turnOpen) await this.applyPendingSettings();

    const msg: Json = {
      type: "user",
      message: { role: "user", content },
      parent_tool_use_id: null,
      session_id: this.sessionId ?? "",
      uuid: sid,
    };
    const now = cmd.priority === "now";
    if (now) msg.priority = "now";
    try {
      this.write(msg);
      // The CLI's priority alone did not interrupt a running tool (observed live): a plain interrupt
      // ends the turn and this message, still queued, opens the next one.
      if (now && this.turnOpen && !this.stopping) this.sendNowInterrupt();
    } catch (e) {
      // Nothing reached Claude: a plain refusal, not uncertain.
      this.subs.delete(sid);
      this.settleSub(sub, { success: false, error: (e as Error).message }, "rejected");
      return result;
    }
    sub.timer = setTimeout(() => {
      if (sub.state !== "dispatching") return;
      // Never resent: the host marks it uncertain for manual recovery.
      this.settleSub(sub, { success: false, uncertain: true, error: `Claude did not acknowledge the message within ${this.limits.promptAckMs / 1000} s` }, "uncertain");
    }, this.limits.promptAckMs);
    return result;
  }

  async abort(cmd: Json): Promise<ResponseBody> {
    const data = { continueQueue: cmd.continueQueue === true, userOnly: cmd.userOnly === true };
    if (this.dead || !this.child) return { success: true, data: { ...data, state: "idle", cancelledSubmissionIds: [] } };
    const inFlight = [...this.subs.values()].some((s) => s.state === "dispatching");
    if (!this.turnOpen && !inFlight) return { success: true, data: { ...data, state: "idle", cancelledSubmissionIds: [] } };
    this.markStopping();
    // An interrupt waits for a running foreground shell (observed live); move those out of the way
    // first by sending running shells to the background.
    if ([...this.liveTools.values()].some((t) => t.taskId && !t.background)) {
      try {
        await this.control({ subtype: "background_tasks" }, QUICK_CONTROL_MS);
      } catch (e) {
        this.deps.diag(`background before stop failed: ${(e as Error).message}`);
      }
    }
    const cancelledSubmissionIds: string[] = [];
    try {
      const r = await this.control({ subtype: "interrupt", cancel_queued: true }, this.limits.controlTimeoutMs);
      for (const id of Array.isArray(r.cancelled) ? r.cancelled : []) {
        const sub = this.subs.get(String(id));
        if (!sub || sub.delivered) continue; // ignore uuids we never sent (the CLI may list its own)
        sub.dropped = true;
        cancelledSubmissionIds.push(sub.id);
      }
    } catch (e) {
      this.deps.diag(`interrupt failed: ${(e as Error).message}`);
    }
    return { success: true, data: { ...data, state: "stopping", cancelledSubmissionIds } };
  }

  private sendNowInterrupt(): void {
    this.superseding = true;
    this.control({ subtype: "interrupt" }, this.limits.controlTimeoutMs).catch((e: Error) => this.deps.diag(`send-now interrupt failed: ${e.message}`));
  }

  /** The wait limit and Send now (worker/PROTOCOL-NOTES.md, "Delivery, steering and tool control"). */
  async deliverNow(cmd: Json): Promise<ResponseBody> {
    const sid = typeof cmd.submissionId === "string" ? cmd.submissionId : "";
    const reason = cmd.reason === "send_now" ? "send_now" : "wait_limit";
    const minRunMs = typeof cmd.minRunMs === "number" && cmd.minRunMs >= 0 ? cmd.minRunMs : DEFAULT_MIN_RUN_MS;
    if (this.dead || !this.turnOpen || this.stopping) return { success: true, data: { result: "stopped" } };
    const sub = this.subs.get(sid);
    if (!sub || sub.state !== "accepted" || sub.delivered || sub.dropped) return { success: true, data: { result: "nothing_waiting" } };
    // Never act while the task waits on the user (approval, question or plan).
    if (this.interactions.size > 0) return { success: true, data: { result: "waiting" } };
    const now = Date.now();
    const eligible = [...this.liveTools.values()].filter((t) => t.taskId && !t.background && now - t.startedAt >= minRunMs);
    if (eligible.length === 0) {
      if (reason === "send_now") {
        this.sendNowInterrupt();
        return { success: true, data: { result: "delivering" } };
      }
      return { success: true, data: { result: "waiting" } };
    }
    const moved: string[] = [];
    for (const t of eligible) {
      try {
        const r = await this.control({ subtype: "background_tasks", tool_use_id: t.id }, QUICK_CONTROL_MS);
        if (r.backgrounded === true) {
          t.background = true;
          moved.push(t.id);
        }
      } catch (e) {
        this.deps.diag(`background_tasks for ${t.id} failed: ${(e as Error).message}`);
      }
    }
    return { success: true, data: moved.length > 0 ? { result: "unblocked", moved } : { result: "waiting" } };
  }

  /** Takes back a steer Claude has not read yet (cancel async message). */
  async withdraw(cmd: Json): Promise<ResponseBody> {
    const sub = this.subs.get(typeof cmd.submissionId === "string" ? cmd.submissionId : "");
    if (!sub || sub.delivered || sub.dropped || this.dead) return { success: true, data: { withdrawn: false } };
    try {
      const r = await this.control({ subtype: "cancel_async_message", message_uuid: sub.id }, QUICK_CONTROL_MS);
      if (r.cancelled === true && !sub.delivered) {
        sub.dropped = true;
        this.ackSub(sub); // written and now withdrawn: the prompt itself succeeded
        return { success: true, data: { withdrawn: true } };
      }
      return { success: true, data: { withdrawn: false } };
    } catch (e) {
      return { success: false, error: (e as Error).message };
    }
  }

  /** Fails one running foreground Bash call (stop task). */
  async stopTool(cmd: Json): Promise<ResponseBody> {
    const sentAt = typeof cmd.sentAtMs === "number" ? cmd.sentAtMs : NaN;
    if (!(Date.now() - sentAt <= STOP_TOOL_MAX_AGE_MS)) return { success: false, error: "stop_tool line is missing sentAtMs or is more than 5 s old" };
    const t = this.liveTools.get(typeof cmd.toolCallId === "string" ? cmd.toolCallId : "");
    if (!t || !t.taskId || t.background || t.taskKind !== "shell" || this.dead) return { success: true, data: { stopped: false } };
    try {
      await this.control({ subtype: "stop_task", task_id: t.taskId }, QUICK_CONTROL_MS);
      return { success: true, data: { stopped: true } };
    } catch (e) {
      return { success: false, error: (e as Error).message };
    }
  }

  /** Manual compaction: an internal /compact message, refused while a turn runs. */
  async compact(_cmd: Json): Promise<ResponseBody> {
    if (this.shuttingDown || this.dead) return { success: false, error: "task is finishing" };
    const inFlight = [...this.subs.values()].some((s) => s.state === "dispatching");
    if (this.turnOpen || inFlight || this.compacting) return { success: false, error: "cannot compact while a turn is running" };
    this.compacting = { boundary: false };
    this.ch.emit({ type: "compaction_start" });
    try {
      this.write({ type: "user", message: { role: "user", content: [{ type: "text", text: "/compact" }] }, parent_tool_use_id: null, session_id: this.sessionId ?? "", uuid: randomUUID() });
    } catch (e) {
      this.compacting = undefined;
      this.ch.emit({ type: "compaction_end", trigger: "manual", ok: false });
      return { success: false, error: (e as Error).message };
    }
    return { success: true, data: { state: "compacting" } };
  }

  /** Stop requested (abort, or Stop turn on an approval). Arms the settle deadline. */
  private markStopping(): void {
    if (this.stopping) return;
    this.stopping = true;
    if (!this.turnOpen) this.turnOpen = true; // still starting: the settle below must still be reported
    this.interactions.dismissAll();
    clearTimeout(this.stopTimer);
    this.stopTimer = setTimeout(() => this.stopDeadline(), this.limits.stopSettleMs);
  }

  /** Claude never settled after a stop: close it; the run ends interrupted. */
  private stopDeadline(): void {
    if (!this.stopping || this.dead) return;
    this.deps.diag("claude did not settle after interrupt; ending its process tree");
    this.stopping = false;
    this.translator.abandon("Stopped; Claude Code did not finish cleanly.");
    this.turnOpen = false;
    this.ch.emit({ type: "agent_settled", outcome: "interrupted", error: "Claude Code did not settle after Stop, so it was closed." });
    this.fatal("Claude Code did not settle after Stop and was closed; the chat needs a new worker.");
  }

  async setModel(cmd: Json): Promise<ResponseBody> {
    const model = typeof cmd.model === "string" ? cmd.model.trim() : "";
    if (!model) return { success: false, error: "set_model needs a model" };
    if (this.turnOpen) {
      this.pendingModel = model;
      return { success: true, data: { model, applies: "next_turn" } };
    }
    try {
      await this.control({ subtype: "set_model", model }, this.limits.modelAckMs);
      this.model = model;
      this.pendingModel = undefined;
      return { success: true, data: { model, applies: "next_turn" } };
    } catch (e) {
      return { success: false, error: (e as Error).message };
    }
  }

  async setThinkingLevel(cmd: Json): Promise<ResponseBody> {
    const level = cmd.level;
    if (!isEffort(level)) return { success: false, error: `unknown effort ${JSON.stringify(level)}; use low, medium, high, xhigh or max` };
    if (this.turnOpen) {
      this.pendingEffort = level;
      return { success: true, data: { effort: level, applies: "next_turn" } };
    }
    try {
      await this.applyEffort(level);
      return { success: true, data: { effort: level, applies: "next_turn" } };
    } catch (e) {
      return { success: false, error: (e as Error).message };
    }
  }

  private async applyEffort(level: string): Promise<void> {
    this.effort = level;
    this.pendingEffort = undefined;
    if (!modelTakesEffort(this.model)) return; // remembered for when the model changes
    await this.control({ subtype: "apply_flag_settings", settings: { effortLevel: level } }, this.limits.modelAckMs);
  }

  /** Changes made during a turn apply before the next prompt. */
  private async applyPendingSettings(): Promise<void> {
    if (this.pendingModel) {
      const model = this.pendingModel;
      this.pendingModel = undefined;
      try {
        await this.control({ subtype: "set_model", model }, this.limits.modelAckMs);
        this.model = model;
      } catch (e) {
        this.ch.emit({ type: "notice", text: `Could not switch to model ${model}: ${(e as Error).message}` });
      }
    }
    if (this.pendingEffort) {
      const level = this.pendingEffort;
      try {
        await this.applyEffort(level);
      } catch (e) {
        this.ch.emit({ type: "notice", text: `Could not change effort to ${level}: ${(e as Error).message}` });
      }
    }
  }

  uiResponse(resp: UiResponse): void {
    this.interactions.onResponse(resp);
  }
}

function obj(v: unknown): Json {
  return v && typeof v === "object" && !Array.isArray(v) ? (v as Json) : {};
}

function stripUndefined<T extends object>(o: T): Partial<T> {
  return Object.fromEntries(Object.entries(o).filter(([, v]) => v !== undefined)) as Partial<T>;
}
