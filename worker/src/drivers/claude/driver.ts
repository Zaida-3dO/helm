// The Claude Code driver: one claude CLI child per worker, spoken to over its stream-json control
// channel. Submissions, stop, model/effort, interactions and process control.

import { spawn, type ChildProcess } from "node:child_process";
import type { HostChannel, ResponseBody } from "../../protocol/channel.ts";
import { describeFault, LineReader, MAX_RECORD_BYTES } from "../../protocol/lines.ts";
import type { Driver } from "../../protocol/server.ts";
import { DEFAULT_EFFORT, isEffort, type Json, type Launch, type UiResponse } from "../../protocol/types.ts";
import { isAlive, treeKill } from "../../process/kill.ts";
import { buildClaudeArgs, modelTakesEffort } from "./args.ts";
import { buildClaudeEnv } from "./env.ts";
import { Interactions } from "./interactions.ts";
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

const UUID_RE = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i;

interface Submission {
  id: string;
  state: "dispatching" | "accepted" | "uncertain" | "rejected";
  result: Promise<ResponseBody>;
  settle?: (b: ResponseBody) => void;
  timer?: NodeJS.Timeout;
  text: string;
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

  constructor(launch: Launch, channel: HostChannel, deps: DriverDeps) {
    this.launch = launch;
    this.ch = channel;
    this.deps = deps;
    this.limits = { ...DEFAULTS, maxRecordBytes: MAX_RECORD_BYTES, ...stripUndefined(launch.limits ?? {}) };
    const c = launch.claude ?? {};
    this.model = c.model;
    this.effort = isEffort(c.effort) ? c.effort : DEFAULT_EFFORT;
    this.translator = new StreamTranslator({ emit: (e) => this.ch.emit(e), diag: deps.diag });
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
    const args = [...(c.commandArgs ?? []), ...buildClaudeArgs(c)];
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

    const init = await this.control({ subtype: "initialize" }, this.limits.controlTimeoutMs);
    const models = Array.isArray(init.models)
      ? (init.models as Json[]).map((m) => ({ id: m.value ?? m.id, name: m.displayName ?? m.value, description: m.description }))
      : [];
    return {
      harness: "claude",
      executable: this.resolved.path,
      executableSource: this.resolved.source,
      model: this.model ?? null,
      effort: modelTakesEffort(this.model) ? this.effort : null,
      models,
    };
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
      case "system":
        if (rec.subtype === "init" && typeof rec.model === "string") this.model = rec.model;
        this.translator.handle(rec);
        return;
      case "stream_event":
      case "assistant":
        if (!rec.parent_tool_use_id) this.openTurnIfNeeded();
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
        return this.controlReply(requestId, {});
      case "elicitation":
        return this.controlReply(requestId, { action: "decline" });
      default:
        return this.controlError(requestId, `unsupported control request: ${String(req.subtype)}`);
    }
  }

  /** A replayed user message carrying one of our submission ids is that submission's ack. */
  private onUserEcho(rec: Json): boolean {
    const uuid = typeof rec.uuid === "string" ? rec.uuid : undefined;
    if (!uuid) return false;
    const sub = this.subs.get(uuid);
    if (!sub) return false;
    if (sub.state === "dispatching") {
      this.openTurnIfNeeded();
      this.settleSub(sub, { success: true, data: { submissionId: sub.id, state: "accepted" } }, "accepted");
      this.ch.emit({ type: "message_start", message: { id: sub.id, role: "user", content: [] } });
      this.ch.emit({ type: "message_end", message: { id: sub.id, role: "user", content: [{ type: "text", text: sub.text }], status: "complete", submissionId: sub.id } });
    }
    return true;
  }

  private openTurnIfNeeded(): void {
    if (this.turnOpen) return;
    this.turnOpen = true;
    this.ch.emit({ type: "agent_start" });
  }

  private onResult(rec: Json): void {
    const wasStopping = this.stopping;
    const outcome = this.translator.onResult(rec, wasStopping);
    clearTimeout(this.stopTimer);
    this.stopping = false;
    this.interactions.dismissAll();
    const wasOpen = this.turnOpen;
    this.turnOpen = false;
    if (!wasOpen && !wasStopping) return;
    const e: Json = { type: "agent_settled", outcome };
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
    for (const img of Array.isArray(cmd.images) ? (cmd.images as Json[]) : []) {
      if (typeof img.data === "string") {
        content.push({ type: "image", source: { type: "base64", media_type: String(img.mimeType ?? "image/png"), data: img.data } });
      }
    }
    if (content.length === 0) return { success: false, error: "prompt has no text or images" };

    let settle!: (b: ResponseBody) => void;
    const result = new Promise<ResponseBody>((r) => (settle = r));
    const sub: Submission = { id: sid, state: "dispatching", result, settle, text };
    this.subs.set(sid, sub);

    if (!this.turnOpen) await this.applyPendingSettings();

    const msg: Json = {
      type: "user",
      message: { role: "user", content },
      parent_tool_use_id: null,
      session_id: this.sessionId ?? "",
      uuid: sid,
    };
    if (cmd.priority === "now") msg.priority = "now";
    try {
      this.write(msg);
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
    if (this.dead || !this.child) return { success: true, data: { ...data, state: "idle" } };
    const inFlight = [...this.subs.values()].some((s) => s.state === "dispatching");
    if (!this.turnOpen && !inFlight) return { success: true, data: { ...data, state: "idle" } };
    this.markStopping();
    try {
      await this.control({ subtype: "interrupt" }, this.limits.controlTimeoutMs);
    } catch (e) {
      this.deps.diag(`interrupt failed: ${(e as Error).message}`);
    }
    return { success: true, data: { ...data, state: "stopping" } };
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
