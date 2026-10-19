// Normalises Claude Code stream-json output into the common event stream. Stateful per worker; one
// native turn at a time.

import type { ContentBlock, Json, RetryEndState, SettleOutcome } from "../../protocol/types.ts";
import { toolSource } from "../../hosttools/source.ts";

interface LiveMessage {
  id: string;
  model?: string;
  content: ContentBlock[];
  toolJson: Map<number, string>;
  stopReason?: string;
  usage?: Json;
}

export interface TranslatorHooks {
  emit: (e: Json) => void;
  diag: (msg: string) => void;
  /** Clock for subagent timing and step pacing (tests replace it). */
  now?: () => number;
  /** The shortest gap between two step events of one subagent; default 500 ms (at most 2/s). */
  subagentStepMs?: number;
}

/** The native tool that starts a subagent: `Agent` in Claude Code 2.1.x, `Task` before it. */
export function isSubagentTool(name: string): boolean {
  return name === "Agent" || name === "Task";
}

/** Longest report carried by `subagent_end`; the host cuts it further for the live thread. */
export const SUBAGENT_REPORT_MAX = 64 * 1024;
const ASSIGNMENT_LINES = 3;
const ASSIGNMENT_MAX = 400;
const STEP_TEXT_MAX = 200;

/** One native subagent, keyed by the parent's Agent tool call id. */
interface LiveSubagent {
  toolCallId: string;
  startedAt: number;
  /** The CLI task id (system/task_started), which `stop_task` takes. */
  taskId?: string;
  /** The Agent call returned at once and the agent runs on (the 2.1.289 default). */
  background: boolean;
  tool?: string;
  text?: string;
  tokens?: number;
  toolUses?: number;
  /** The newest text-bearing assistant message of the subagent: its report when it finishes. */
  lastText?: { messageId: string; text: string };
  lastStepAt: number;
  stepDirty: boolean;
  stepTimer?: NodeJS.Timeout;
}

/** The first few non-empty lines of an assignment prompt, bounded. */
export function assignmentPreview(prompt: string): string {
  const lines = prompt
    .split(/\r?\n/)
    .map((l) => l.trim())
    .filter((l) => l.length > 0)
    .slice(0, ASSIGNMENT_LINES);
  const joined = lines.join("\n");
  return joined.length > ASSIGNMENT_MAX ? `${joined.slice(0, ASSIGNMENT_MAX - 1)}…` : joined;
}

function firstLine(text: string): string {
  const line = text.split(/\r?\n/).map((l) => l.trim()).find((l) => l.length > 0) ?? "";
  return line.length > STEP_TEXT_MAX ? `${line.slice(0, STEP_TEXT_MAX - 1)}…` : line;
}

/** What a subagent's tool call is about, for its card's "latest step" line. */
function stepTarget(input: Json): string {
  for (const k of ["command", "file_path", "path", "pattern", "url", "query", "description", "prompt"]) {
    const v = input[k];
    if (typeof v === "string" && v.trim()) return firstLine(v);
  }
  return "";
}

function num(v: unknown): number | undefined {
  return typeof v === "number" && Number.isFinite(v) ? v : undefined;
}

function obj(v: unknown): Json {
  return v && typeof v === "object" && !Array.isArray(v) ? (v as Json) : {};
}

/** Converts native content blocks to ours. */
export function convertContent(content: unknown): ContentBlock[] {
  if (typeof content === "string") return [{ type: "text", text: content }];
  if (!Array.isArray(content)) return [];
  const out: ContentBlock[] = [];
  for (const raw of content) {
    const b = obj(raw);
    switch (b.type) {
      case "text":
        out.push({ type: "text", text: String(b.text ?? "") });
        break;
      case "thinking":
        out.push({ type: "thinking", thinking: String(b.thinking ?? "") });
        break;
      case "redacted_thinking":
        out.push({ type: "thinking", thinking: "" });
        break;
      case "tool_use":
      case "server_tool_use":
        out.push({ type: "toolCall", id: String(b.id ?? ""), name: String(b.name ?? ""), arguments: b.input ?? {} });
        break;
      case "image":
        out.push({ type: "image", mimeType: typeof obj(b.source).media_type === "string" ? String(obj(b.source).media_type) : undefined });
        break;
      default:
        break;
    }
  }
  return out;
}

function resultText(content: unknown): string {
  if (typeof content === "string") return content;
  if (!Array.isArray(content)) return "";
  return content
    .map((c) => obj(c))
    .filter((c) => c.type === "text")
    .map((c) => String(c.text ?? ""))
    .join("\n");
}

export class StreamTranslator {
  private live: LiveMessage | undefined;
  private readonly streamed = new Set<string>();
  private readonly toolNames = new Map<string, string>();
  private readonly openTools = new Set<string>();
  private retry: { attempt: number } | undefined;
  private permissionMode: string | undefined;
  private readonly h: TranslatorHooks;
  private readonly subagents = new Map<string, LiveSubagent>();
  private readonly now: () => number;
  private readonly stepMs: number;

  constructor(hooks: TranslatorHooks) {
    this.h = hooks;
    this.now = hooks.now ?? Date.now;
    this.stepMs = hooks.subagentStepMs ?? 500;
  }

  get hasOpenRetry(): boolean {
    return this.retry !== undefined;
  }

  /** The CLI task id of a running subagent, by its Agent tool call id (what `stop_task` takes). */
  subagentTaskId(toolCallId: string): string | undefined {
    return this.subagents.get(toolCallId)?.taskId;
  }

  /** Subagents that have started and not ended. */
  get runningSubagents(): number {
    return this.subagents.size;
  }

  /** Returns true if the record was something the translator understands. */
  handle(rec: Json): boolean {
    // Subagent traffic (parent_tool_use_id set) is not part of the parent transcript;
    // it feeds the subagent events instead.
    if (rec.parent_tool_use_id) {
      this.onSubagentRecord(String(rec.parent_tool_use_id), rec);
      return true;
    }
    switch (rec.type) {
      case "stream_event":
        this.onStreamEvent(obj(rec.event));
        return true;
      case "assistant":
        this.onAssistant(rec);
        return true;
      case "user":
        this.onUser(rec);
        return true;
      case "system":
        return this.onSystem(rec);
      default:
        return false;
    }
  }

  private onStreamEvent(ev: Json): void {
    switch (ev.type) {
      case "message_start": {
        const m = obj(ev.message);
        const id = String(m.id ?? `live-${Date.now()}`);
        this.closeLive("interrupted"); // a new message while one is open: the old one never finished
        this.resolveRetry("recovered");
        this.live = { id, model: typeof m.model === "string" ? m.model : undefined, content: [], toolJson: new Map() };
        this.streamed.add(id);
        this.h.emit({ type: "message_start", message: { id, role: "assistant", content: [], model: this.live.model } });
        return;
      }
      case "content_block_start": {
        if (!this.live) return;
        const i = Number(ev.index ?? this.live.content.length);
        const b = obj(ev.content_block);
        if (b.type === "text") this.live.content[i] = { type: "text", text: String(b.text ?? "") };
        else if (b.type === "thinking" || b.type === "redacted_thinking")
          this.live.content[i] = { type: "thinking", thinking: String(b.thinking ?? "") };
        else if (b.type === "tool_use" || b.type === "server_tool_use") {
          this.live.content[i] = { type: "toolCall", id: String(b.id ?? ""), name: String(b.name ?? ""), arguments: {} };
          this.live.toolJson.set(i, "");
          this.toolNames.set(String(b.id ?? ""), String(b.name ?? ""));
        }
        return;
      }
      case "content_block_delta": {
        if (!this.live) return;
        const i = Number(ev.index ?? 0);
        const d = obj(ev.delta);
        const block = this.live.content[i];
        if (d.type === "text_delta" && block?.type === "text") {
          const t = String(d.text ?? "");
          block.text += t;
          this.h.emit({ type: "message_update", messageId: this.live.id, index: i, delta: { type: "text_delta", text: t } });
        } else if (d.type === "thinking_delta" && block?.type === "thinking") {
          const t = String(d.thinking ?? "");
          block.thinking += t;
          this.h.emit({ type: "message_update", messageId: this.live.id, index: i, delta: { type: "thinking_delta", text: t } });
        } else if (d.type === "input_json_delta" && block?.type === "toolCall") {
          const t = String(d.partial_json ?? "");
          this.live.toolJson.set(i, (this.live.toolJson.get(i) ?? "") + t);
          this.h.emit({ type: "message_update", messageId: this.live.id, index: i, delta: { type: "toolcall_delta", text: t } });
        }
        return;
      }
      case "content_block_stop": {
        if (!this.live) return;
        const i = Number(ev.index ?? 0);
        const block = this.live.content[i];
        if (block?.type === "toolCall") {
          const raw = this.live.toolJson.get(i) ?? "";
          try {
            block.arguments = raw.trim() === "" ? {} : JSON.parse(raw);
          } catch {
            block.arguments = { _unparsed: raw };
          }
        }
        return;
      }
      case "message_delta": {
        if (!this.live) return;
        const d = obj(ev.delta);
        if (typeof d.stop_reason === "string") this.live.stopReason = d.stop_reason;
        if (ev.usage) this.live.usage = { ...(this.live.usage ?? {}), ...obj(ev.usage) };
        return;
      }
      case "message_stop":
        this.closeLive("complete");
        return;
      default:
        return;
    }
  }

  private closeLive(status: "complete" | "error" | "interrupted", errorMessage?: string): void {
    const m = this.live;
    if (!m) return;
    this.live = undefined;
    const message: Json = {
      id: m.id,
      role: "assistant",
      content: m.content.filter(Boolean),
      model: m.model,
      stopReason: m.stopReason,
      usage: m.usage,
      status,
    };
    if (errorMessage) message.errorMessage = errorMessage;
    this.h.emit({ type: "message_end", message });
  }

  private onAssistant(rec: Json): void {
    const m = obj(rec.message);
    const id = String(m.id ?? rec.uuid ?? `msg-${Date.now()}`);
    const content = convertContent(m.content);
    const isError = m.model === "<synthetic>" || typeof rec.error === "string";
    if (isError) {
      // A failed request: Claude Code reports it as a synthetic assistant message.
      const text = content.filter((b) => b.type === "text").map((b) => (b as { text: string }).text).join("\n");
      this.h.emit({ type: "message_start", message: { id, role: "assistant", content: [] } });
      this.h.emit({
        type: "message_end",
        message: { id, role: "assistant", content, status: "error", errorMessage: text || String(rec.error ?? "request failed") },
      });
      return;
    }
    if (!this.streamed.has(id)) {
      // No partial stream for this message (partials off, or replayed): emit it whole.
      this.streamed.add(id);
      this.resolveRetry("recovered");
      this.h.emit({ type: "message_start", message: { id, role: "assistant", content: [], model: m.model } });
      this.h.emit({
        type: "message_end",
        message: { id, role: "assistant", content, model: m.model, stopReason: m.stop_reason, usage: m.usage, status: "complete" },
      });
    }
    for (const b of content) {
      if (b.type !== "toolCall" || !b.id || this.openTools.has(b.id)) continue;
      this.openTools.add(b.id);
      this.toolNames.set(b.id, b.name);
      this.h.emit({ type: "tool_execution_start", toolCallId: b.id, toolName: b.name, args: b.arguments, toolSource: toolSource(b.name, b.arguments) });
      if (isSubagentTool(b.name)) this.startSubagent(b.id, obj(b.arguments));
    }
  }

  private onUser(rec: Json): void {
    const m = obj(rec.message);
    if (!Array.isArray(m.content)) return;
    for (const raw of m.content) {
      const b = obj(raw);
      if (b.type !== "tool_result") continue;
      const toolCallId = String(b.tool_use_id ?? "");
      this.openTools.delete(toolCallId);
      const text = resultText(b.content);
      this.h.emit({
        type: "tool_execution_end",
        toolCallId,
        toolName: this.toolNames.get(toolCallId) ?? "",
        result: { text, content: Array.isArray(b.content) ? convertContent(b.content) : undefined },
        isError: b.is_error === true,
      });
      this.onSubagentToolResult(toolCallId, text, b.is_error === true);
    }
  }

  // ---- subagents --------------------------------------------------------------------------

  private startSubagent(toolCallId: string, args: Json): void {
    if (this.subagents.has(toolCallId)) return;
    const now = this.now();
    this.subagents.set(toolCallId, { toolCallId, startedAt: now, background: false, lastStepAt: 0, stepDirty: false });
    const description = typeof args.description === "string" ? args.description.trim() : "";
    const agentType = typeof args.subagent_type === "string" && args.subagent_type.trim() ? args.subagent_type.trim() : "general-purpose";
    const name = typeof args.name === "string" && args.name.trim() ? args.name.trim() : description || agentType;
    this.h.emit({
      type: "subagent_start",
      toolCallId,
      agentType,
      name,
      description,
      assignment: assignmentPreview(typeof args.prompt === "string" ? args.prompt : ""),
      startedAt: now,
    });
  }

  /** A record from inside a subagent: its latest tool call or narration becomes its step. */
  private onSubagentRecord(parent: string, rec: Json): void {
    const sa = this.subagents.get(parent);
    if (!sa || rec.type !== "assistant") return; // nested agents' own records and tool results
    const m = obj(rec.message);
    const messageId = String(m.id ?? rec.uuid ?? "");
    let step: { tool?: string; text: string } | undefined;
    for (const b of convertContent(m.content)) {
      if (b.type === "toolCall") step = { tool: b.name, text: stepTarget(obj(b.arguments)) };
      else if (b.type === "text" && b.text.trim()) {
        const prev = sa.lastText && sa.lastText.messageId === messageId ? `${sa.lastText.text}\n\n` : "";
        sa.lastText = { messageId, text: prev + b.text };
        step = { text: firstLine(b.text) };
      }
    }
    if (!step) return;
    sa.tool = step.tool;
    sa.text = step.text;
    this.queueStep(sa);
  }

  /** At most one step event per subagent per `stepMs`; the latest state wins (trailing edge). */
  private queueStep(sa: LiveSubagent): void {
    sa.stepDirty = true;
    if (sa.stepTimer) return;
    const wait = sa.lastStepAt + this.stepMs - this.now();
    if (wait <= 0) return this.flushStep(sa);
    sa.stepTimer = setTimeout(() => {
      sa.stepTimer = undefined;
      if (this.subagents.get(sa.toolCallId) === sa) this.flushStep(sa);
    }, wait);
    sa.stepTimer.unref?.();
  }

  private flushStep(sa: LiveSubagent): void {
    if (!sa.stepDirty) return;
    sa.stepDirty = false;
    sa.lastStepAt = this.now();
    const e: Json = { type: "subagent_step", toolCallId: sa.toolCallId, text: sa.text ?? "" };
    if (sa.tool) e.tool = sa.tool;
    if (sa.tokens !== undefined) e.tokens = sa.tokens;
    if (sa.toolUses !== undefined) e.toolUses = sa.toolUses;
    this.h.emit(e);
  }

  private endSubagent(sa: LiveSubagent, status: "completed" | "failed" | "stopped", opts: { report?: string; error?: string; durationMs?: number } = {}): void {
    if (this.subagents.get(sa.toolCallId) !== sa) return;
    this.subagents.delete(sa.toolCallId);
    clearTimeout(sa.stepTimer);
    sa.stepTimer = undefined;
    const e: Json = { type: "subagent_end", toolCallId: sa.toolCallId, status, durationMs: opts.durationMs ?? Math.max(0, this.now() - sa.startedAt) };
    const report = opts.report?.trim();
    if (report) e.report = report.length > SUBAGENT_REPORT_MAX ? report.slice(0, SUBAGENT_REPORT_MAX) : report;
    if (opts.error) e.error = opts.error;
    if (sa.tokens !== undefined) e.tokens = sa.tokens;
    if (sa.toolUses !== undefined) e.toolUses = sa.toolUses;
    this.h.emit(e);
  }

  /**
   * The Agent call's own result. A foreground agent ends here and the result is its report; a
   * background agent's call returns at once ("Async agent launched…") and it ends with its
   * task notification instead.
   */
  private onSubagentToolResult(toolCallId: string, text: string, isError: boolean): void {
    const sa = this.subagents.get(toolCallId);
    if (!sa) return;
    if (!isError && (sa.background || /^Async agent launched/i.test(text.trimStart()))) {
      sa.background = true;
      return;
    }
    if (isError) this.endSubagent(sa, "failed", { error: firstLine(text) || "The agent failed.", report: sa.lastText?.text });
    else this.endSubagent(sa, "completed", { report: text || sa.lastText?.text });
  }

  /** task_started / task_progress / task_updated / task_notification for local agents. */
  private onTaskRecord(rec: Json): boolean {
    const byTool = this.subagents.get(String(rec.tool_use_id ?? ""));
    const taskId = String(rec.task_id ?? "");
    const sa = byTool ?? [...this.subagents.values()].find((s) => taskId !== "" && s.taskId === taskId);
    if (!sa) return true; // shells, nested agents, or agents of an earlier worker
    switch (rec.subtype) {
      case "task_started":
        if (rec.task_type !== undefined && rec.task_type !== "local_agent") return true;
        if (taskId) sa.taskId = taskId;
        if (rec.is_backgrounded === true) sa.background = true;
        return true;
      case "task_updated":
        if (obj(rec.patch).is_backgrounded === true) sa.background = true;
        return true;
      case "task_progress": {
        const u = obj(rec.usage);
        sa.tokens = num(u.total_tokens) ?? sa.tokens;
        sa.toolUses = num(u.tool_uses) ?? sa.toolUses;
        if (sa.text === undefined && typeof rec.description === "string") sa.text = firstLine(rec.description);
        this.queueStep(sa);
        return true;
      }
      case "task_notification": {
        const u = obj(rec.usage);
        sa.tokens = num(u.total_tokens) ?? sa.tokens;
        sa.toolUses = num(u.tool_uses) ?? sa.toolUses;
        const summary = typeof rec.summary === "string" ? rec.summary : "";
        const durationMs = num(u.duration_ms);
        if (rec.status === "completed") this.endSubagent(sa, "completed", { report: sa.lastText?.text || summary, durationMs });
        else if (rec.status === "failed") this.endSubagent(sa, "failed", { report: sa.lastText?.text, error: summary || "The agent failed.", durationMs });
        else this.endSubagent(sa, "stopped", { report: sa.lastText?.text, durationMs });
        return true;
      }
      default:
        return true;
    }
  }

  /** The turn ended: agents that were blocking it cannot outlive it; background agents run on. */
  private endForegroundSubagents(status: "failed" | "stopped", reason: string): void {
    for (const sa of [...this.subagents.values()]) {
      if (!sa.background) this.endSubagent(sa, status, { error: reason, report: sa.lastText?.text });
    }
  }

  private onSystem(rec: Json): boolean {
    switch (rec.subtype) {
      case "init": {
        const mode = typeof rec.permissionMode === "string" ? rec.permissionMode : undefined;
        if (mode && mode !== this.permissionMode) {
          this.permissionMode = mode;
          this.h.emit({ type: "permission_mode", mode });
        }
        return true;
      }
      case "status": {
        const mode = typeof rec.permissionMode === "string" ? rec.permissionMode : undefined;
        if (mode && mode !== this.permissionMode) {
          this.permissionMode = mode;
          this.h.emit({ type: "permission_mode", mode });
        }
        return true;
      }
      case "api_retry": {
        if (this.retry) this.resolveRetry("retried");
        const attempt = Number(rec.attempt ?? 1);
        this.retry = { attempt };
        this.h.emit({
          type: "auto_retry_start",
          attempt,
          maxAttempts: rec.max_retries ?? null,
          delayMs: rec.retry_delay_ms ?? null,
          errorStatus: rec.error_status ?? null,
          errorMessage: typeof rec.error === "string" ? rec.error : JSON.stringify(rec.error ?? null),
          waiting: false,
        });
        return true;
      }
      case "compact_boundary":
        this.h.emit({ type: "compaction_end", trigger: obj(rec.compact_metadata).trigger ?? null });
        return true;
      case "task_started":
      case "task_progress":
      case "task_updated":
      case "task_notification":
        return this.onTaskRecord(rec);
      default:
        return false;
    }
  }

  resolveRetry(state: RetryEndState): void {
    if (!this.retry) return;
    const { attempt } = this.retry;
    this.retry = undefined;
    this.h.emit({ type: "auto_retry_end", state, attempt });
  }

  /** Handles the `result` record; returns the settle outcome. */
  onResult(rec: Json, stopping: boolean): SettleOutcome {
    const isError = rec.is_error === true || (typeof rec.subtype === "string" && rec.subtype !== "success");
    this.closeLive(stopping ? "interrupted" : isError ? "error" : "complete");
    if (stopping) this.resolveRetry("cancelled");
    else this.resolveRetry(isError ? "failed" : "recovered");
    this.endForegroundSubagents(stopping ? "stopped" : "failed", stopping ? "Stopped by the user." : "The turn ended before this agent finished.");
    this.closeOpenTools(stopping ? "Stopped by the user." : "The turn ended before this tool finished.");
    const u = obj(rec.usage);
    this.h.emit({
      type: "usage",
      inputTokens: u.input_tokens ?? 0,
      outputTokens: u.output_tokens ?? 0,
      cacheReadTokens: u.cache_read_input_tokens ?? 0,
      cacheWriteTokens: u.cache_creation_input_tokens ?? 0,
      costUsd: rec.total_cost_usd ?? null,
      durationMs: rec.duration_ms ?? null,
      numTurns: rec.num_turns ?? null,
    });
    // A host-requested abort that the harness reports as an error is a cancel.
    if (stopping) return "stopped";
    return isError ? "error" : "complete";
  }

  closeOpenTools(reason: string): void {
    for (const id of [...this.openTools]) {
      this.openTools.delete(id);
      this.h.emit({ type: "tool_execution_end", toolCallId: id, toolName: this.toolNames.get(id) ?? "", result: { text: reason }, isError: true });
    }
  }

  /** Worker lost the harness or gave up on a stop: end whatever is still open. */
  abandon(reason: string): void {
    this.closeLive("interrupted", reason);
    this.resolveRetry("cancelled");
    // Without the CLI no agent runs on, background ones included.
    for (const sa of [...this.subagents.values()]) this.endSubagent(sa, "failed", { error: reason, report: sa.lastText?.text });
    this.closeOpenTools(reason);
  }
}
