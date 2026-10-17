// Normalises Claude Code stream-json output into the common event stream. Stateful per worker; one
// native turn at a time.

import type { ContentBlock, Json, RetryEndState, SettleOutcome } from "../../protocol/types.ts";

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

  constructor(hooks: TranslatorHooks) {
    this.h = hooks;
  }

  get hasOpenRetry(): boolean {
    return this.retry !== undefined;
  }

  /** Returns true if the record was something the translator understands. */
  handle(rec: Json): boolean {
    // Subagent traffic (parent_tool_use_id set) is not part of the parent transcript.
    // Subagent UI comes later.
    if (rec.parent_tool_use_id) return true;
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
      this.h.emit({ type: "tool_execution_start", toolCallId: b.id, toolName: b.name, args: b.arguments });
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
      this.h.emit({
        type: "tool_execution_end",
        toolCallId,
        toolName: this.toolNames.get(toolCallId) ?? "",
        result: { text: resultText(b.content), content: Array.isArray(b.content) ? convertContent(b.content) : undefined },
        isError: b.is_error === true,
      });
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
    this.closeOpenTools(reason);
  }
}
