// The in-process MCP server named "helm".
//
// Claude Code reaches it over its control channel: each JSON-RPC message arrives as an
// `mcp_message` control request and the answer goes back in the control response. The server
// lists exactly the host tools from launch.json `hostTools[]`, plus the worker's own
// compatibility tools (`app_list_tools` and `app_mcp_call` always; `app_mcp_tools` whenever there
// is an MCP client, see names.ts). Host tools go to the host through the request channel; MCP tools go to the
// MCP bridge (worker/src/mcp).
//
// A root agent's call that runs past `backgroundAfterMs`, or that `background()` is asked to move
// (a message is waiting to be read), is answered early with a call id, so Claude Code reaches its
// next tool boundary. The request itself carries on into the background store, and the server
// answers `app_background_result` from there itself; the host's handler for that tool is only a
// fallback.

import type { Json } from "../protocol/types.ts";
import { DEFAULT_TIMEOUT_MS, NOTHING_CHANGED, type CallResult, type RequestChannel } from "./channel.ts";
import { capToolResult, type OutputCap } from "./cap.ts";
import { COMPAT_ALWAYS, COMPAT_WITH_MCP } from "./names.ts";
import { BackgroundStore, type BackgroundCall } from "./background.ts";
import { MAX_TOOL_NAME_BYTES } from "../mcp/naming.ts";

export const SERVER_NAME = "helm";
/** The CLI's name for a tool on this server. */
export const toolPrefix = `mcp__${SERVER_NAME}__`;
export const SEARCH_PAGE = 10;
const SMALL = 8 * 1024;
/** A root call still running after this is answered early and moved to the background. */
export const BACKGROUND_AFTER_MS = 30_000;
/** How long `app_background_result` waits for a call that is still running. */
export const RESULT_WAIT_MS = 20_000;
export const BACKGROUND_RESULT = "app_background_result";

/** One launch.json `hostTools[]` entry (helm-hosttools CatalogueEntry). */
export interface HostTool {
  name: string;
  description: string;
  inputSchema: Json;
  guidelines?: string;
  requestLimitBytes: number;
  timeoutMs: number;
  children: boolean;
  mutating: boolean;
}

/** What the server needs from the MCP bridge. */
export interface McpBridgeLike {
  /** At least one MCP server is enabled (otherwise `app_mcp_tools` says none is). */
  enabled(): boolean;
  /** `app_mcp_tools`: search discovered tools. */
  tools(args: Json): Promise<unknown>;
  /** A discovered, still-registered MCP tool. */
  has(name: string): boolean;
  call(name: string, args: Json, signal?: AbortSignal): Promise<CallResult>;
}

export interface CallerInfo {
  child: boolean;
  toolCallId?: string;
  /** No hook record matched in time: no host-tool call is sent for it. */
  unattributed?: boolean;
}

export interface ServerOptions {
  hostTools: HostTool[];
  channel: RequestChannel;
  bridge?: McpBridgeLike;
  /** A successful self-archive with `archivePending: true` ends the turn. */
  onEndTurn?: () => void;
  /** Cut very long tool results to a head and a tail. Absent: results pass whole. */
  outputCap?: OutputCap;
  /** Tells the user something in the transcript (a moved call finished). */
  notify?: (text: string) => void;
  backgroundAfterMs?: number;
  resultWaitMs?: number;
  store?: BackgroundStore;
}

type RpcId = string | number | null;
interface Rpc {
  jsonrpc?: string;
  id?: RpcId;
  method?: string;
  params?: Json;
}

const LIST_TOOLS: HostTool = {
  name: "app_list_tools",
  description:
    "Search Helm's current host tools, with their input schemas and usage guidelines, 10 per page. Use it when a tool you expect is missing from your list or its schema looks out of date, then call the tool through app_mcp_call.",
  inputSchema: {
    type: "object",
    properties: {
      query: { type: "string", maxLength: 200, description: "Words to match in tool names and descriptions." },
      offset: { type: "integer", minimum: 0, description: "From a previous page's nextOffset." },
    },
    additionalProperties: false,
  },
  requestLimitBytes: SMALL,
  timeoutMs: DEFAULT_TIMEOUT_MS,
  children: false,
  mutating: false,
};

const MCP_CALL: HostTool = {
  name: "app_mcp_call",
  description:
    "Call any current Helm host tool, or any MCP tool discovered with app_mcp_tools, by its exact name. Use it for tools that are not in your tool list. Arguments follow that tool's own input schema.",
  inputSchema: {
    type: "object",
    properties: {
      name: { type: "string", minLength: 1, maxLength: 128 },
      arguments: { type: "object" },
    },
    required: ["name"],
    additionalProperties: false,
  },
  requestLimitBytes: 32 * 1024,
  timeoutMs: DEFAULT_TIMEOUT_MS,
  children: true,
  mutating: true,
};

const MCP_TOOLS: HostTool = {
  name: "app_mcp_tools",
  description:
    "Search the tools of the MCP servers configured in Helm (Settings > MCP), with their schemas. Call one with app_mcp_call.",
  inputSchema: {
    type: "object",
    properties: {
      query: { type: "string", maxLength: 200 },
      offset: { type: "integer", minimum: 0 },
      server: { type: "string", maxLength: 128 },
    },
    additionalProperties: false,
  },
  requestLimitBytes: SMALL,
  timeoutMs: 45_000,
  children: true,
  mutating: false,
};

function text(v: unknown): string {
  return typeof v === "string" ? v : JSON.stringify(v);
}

/** An MCP CallToolResult for a channel or bridge outcome. */
export function toolResult(r: CallResult): Json {
  if (!r.ok) return { content: [{ type: "text", text: r.error }], isError: true };
  const out: Json = { content: [{ type: "text", text: text(r.result) }] };
  if (r.result && typeof r.result === "object" && !Array.isArray(r.result)) out.structuredContent = r.result;
  return out;
}

function refuse(msg: string): CallResult {
  return { ok: false, error: `${msg}${NOTHING_CHANGED}`, changed: "no" };
}

/** Never moved to the background: tool discovery, and the fetch itself. */
const STAYS_IN_FOREGROUND = new Set([LIST_TOOLS.name, MCP_TOOLS.name, BACKGROUND_RESULT]);

function seconds(ms: number): number {
  return Math.max(0, Math.round(ms / 1000));
}

/** What the agent reads when its call is moved. */
function movedAnswer(c: BackgroundCall): Json {
  const careful = c.mutating ? " It may still change things after this answer: check its result before acting on what it does, and do not repeat it." : "";
  return {
    state: "background",
    callId: c.callId,
    tool: c.tool,
    message: `${c.tool} is still running, so Helm moved it to the background and the chat carries on. Do not call it again: read its result later with ${BACKGROUND_RESULT} {"callId": "${c.callId}"}.${careful}`,
  };
}

function describeCall(c: BackgroundCall, now: number): Json {
  const out: Json = { callId: c.callId, tool: c.tool, state: c.result ? "done" : "running", runningSeconds: seconds((c.finishedAt ?? now) - c.startedAt) };
  if (c.result) out.read = c.readAt !== undefined;
  return out;
}

export class HostToolServer {
  private readonly o: ServerOptions;
  private readonly host = new Map<string, HostTool>();
  private readonly store: BackgroundStore;
  /** Root calls running in the foreground, by tool-call id: each entry moves its call now. */
  private readonly movable = new Map<string, () => boolean>();

  constructor(o: ServerOptions) {
    this.o = o;
    for (const t of o.hostTools) this.host.set(t.name, t);
    this.store = o.store ?? new BackgroundStore();
  }

  /** Move a running root call to the background now. False when there is no such call or no room. */
  background(toolCallId: string): boolean {
    return this.movable.get(toolCallId)?.() ?? false;
  }

  /** The tools/list catalogue, host tools first. */
  catalogue(): HostTool[] {
    const compat = [LIST_TOOLS, MCP_CALL].filter((t) => (COMPAT_ALWAYS as readonly string[]).includes(t.name));
    const out = [...this.o.hostTools, ...compat];
    if (this.o.bridge && (COMPAT_WITH_MCP as readonly string[]).includes(MCP_TOOLS.name)) out.push(MCP_TOOLS);
    return out;
  }

  /** Answer one JSON-RPC message. Notifications get `null` (nothing to send back). */
  async handle(msg: Rpc, caller: CallerInfo, signal?: AbortSignal): Promise<Json | null> {
    const id = msg.id ?? null;
    const ok = (result: unknown): Json => ({ jsonrpc: "2.0", id, result });
    const err = (code: number, message: string): Json => ({ jsonrpc: "2.0", id, error: { code, message } });
    if (msg.id === undefined) return null;
    switch (msg.method) {
      case "initialize":
        return ok({
          protocolVersion: typeof msg.params?.protocolVersion === "string" ? msg.params.protocolVersion : "2025-06-18",
          capabilities: { tools: {} },
          serverInfo: { name: SERVER_NAME, version: "1" },
        });
      case "ping":
        return ok({});
      case "tools/list":
        return ok({ tools: this.catalogue().map((t) => ({ name: t.name, description: t.description, inputSchema: t.inputSchema })) });
      case "tools/call": {
        const name = String(msg.params?.name ?? "");
        const args = (msg.params?.arguments ?? {}) as Json;
        const result = toolResult(await this.callOrMove(name, args, caller, signal));
        return ok(this.o.outputCap ? capToolResult(result, name, this.o.outputCap) : result);
      }
      default:
        return err(-32601, `Method not found: ${String(msg.method)}`);
    }
  }

  /**
   * Run a call, answering early when it is moved to the background. Only the root agent's calls
   * move: a sub-agent waits for its own calls, and an unattributed call has no id to fetch by.
   */
  private async callOrMove(name: string, args: Json, caller: CallerInfo, signal?: AbortSignal): Promise<CallResult> {
    const target = name === MCP_CALL.name ? String(args.name ?? "") : name;
    const id = caller.toolCallId;
    if (caller.child || caller.unattributed || !id || STAYS_IN_FOREGROUND.has(target) || this.movable.has(id)) {
      return this.call(name, args, caller, signal);
    }
    // The call gets its own cancel: the CLI's reaches it only until the call is moved.
    const own = new AbortController();
    const forward = () => own.abort();
    if (signal?.aborted) own.abort();
    else signal?.addEventListener("abort", forward, { once: true });
    const startedAt = Date.now();
    // A bridged MCP tool may change anything; only a host tool says it does not.
    const mutating = this.host.get(target)?.mutating ?? true;
    let finished = false;
    const work = this.call(name, args, caller, own.signal)
      .catch((e: Error): CallResult => ({ ok: false, error: `${target} failed: ${e.message}`, changed: mutating ? "maybe" : "no" }))
      .finally(() => {
        finished = true;
      });
    let moved: (c: BackgroundCall) => void = () => {};
    const movedP = new Promise<BackgroundCall>((r) => (moved = r));
    const move = (): boolean => {
      if (finished || this.movable.get(id) !== move) return false;
      const c = this.store.add(id, target, mutating, startedAt, work, (done) => this.finishedInBackground(done));
      if (!c) return false;
      this.movable.delete(id);
      signal?.removeEventListener("abort", forward);
      moved(c);
      return true;
    };
    this.movable.set(id, move);
    const timer = setTimeout(move, this.o.backgroundAfterMs ?? BACKGROUND_AFTER_MS);
    try {
      // Exactly one answer goes back: the result, or the moved answer with the result in the store.
      const first = await Promise.race([work, movedP]);
      return "callId" in first ? { ok: true, result: movedAnswer(first) } : first;
    } finally {
      clearTimeout(timer);
      if (this.movable.get(id) === move) this.movable.delete(id);
      if (finished) signal?.removeEventListener("abort", forward);
    }
  }

  private finishedInBackground(c: BackgroundCall): void {
    const how = c.result?.ok ? "finished" : "failed";
    const secs = seconds((c.finishedAt ?? c.startedAt) - c.startedAt);
    this.o.notify?.(`${c.tool} ${how} in the background after ${secs} s. The agent can read its result with ${BACKGROUND_RESULT}.`);
  }

  /** `app_background_result`: one call's result (waiting a little for a running one), or the list. */
  private async backgroundResult(args: Json, caller: CallerInfo): Promise<CallResult> {
    if (caller.child) return refuse(`${BACKGROUND_RESULT} is only available to the parent chat.`);
    const id = typeof args.callId === "string" ? args.callId.slice(0, 128) : "";
    if (!id) {
      const now = Date.now();
      return { ok: true, result: { calls: this.store.list().map((c) => describeCall(c, now)) } };
    }
    const c = this.store.get(id);
    if (!c) {
      return refuse(
        `There is no background call ${id}. A result is kept 30 minutes after its call finishes and 10 minutes after it is read; call ${BACKGROUND_RESULT} with no callId to list what Helm still holds.`,
      );
    }
    if (!c.result) {
      let timer: NodeJS.Timeout | undefined;
      await Promise.race([c.done, new Promise<void>((r) => (timer = setTimeout(r, this.o.resultWaitMs ?? RESULT_WAIT_MS)))]);
      clearTimeout(timer);
    }
    const r = c.result;
    if (!r) {
      return { ok: true, result: { ...describeCall(c, Date.now()), message: "Still running. Carry on with other work and ask again later; do not call the tool again." } };
    }
    this.store.markRead(c);
    if (!r.ok) return { ok: false, error: `${c.tool} failed in the background: ${r.error}`, changed: r.changed };
    return { ok: true, result: { state: "done", callId: c.callId, tool: c.tool, result: r.result as Json } };
  }

  /** Route one tool call. */
  async call(name: string, args: Json, caller: CallerInfo, signal?: AbortSignal): Promise<CallResult> {
    // One length cap on a tool name wherever it enters, and a long name is never echoed back.
    if (Buffer.byteLength(name, "utf8") > MAX_TOOL_NAME_BYTES) return refuse(`A tool name is at most ${MAX_TOOL_NAME_BYTES} bytes.`);
    if (name === LIST_TOOLS.name) {
      if (caller.child) return refuse(`${name} is only available to the parent chat.`);
      return { ok: true, result: this.search(args) };
    }
    if (name === MCP_TOOLS.name) {
      if (!this.o.bridge?.enabled()) return refuse("No MCP server is enabled in Helm (Settings > MCP).");
      return { ok: true, result: await this.o.bridge.tools(args) };
    }
    if (name === MCP_CALL.name) {
      const target = String(args.name ?? "");
      if (Buffer.byteLength(target, "utf8") > MAX_TOOL_NAME_BYTES) return refuse(`A tool name is at most ${MAX_TOOL_NAME_BYTES} bytes.`);
      const inner = (args.arguments ?? {}) as Json;
      if (target === MCP_CALL.name || target === LIST_TOOLS.name || target === MCP_TOOLS.name) {
        return refuse(`${target} cannot be called through app_mcp_call.`);
      }
      if (target === BACKGROUND_RESULT && this.host.has(target)) return this.backgroundResult(inner, caller);
      if (this.o.bridge?.has(target)) return this.o.bridge.call(target, inner, signal);
      // A child reaches only discovered MCP tools through app_mcp_call, never a host tool.
      if (caller.child) return refuse(`${target} is only available to the parent chat.`);
      return this.host.has(target) ? this.hostCall(target, inner, caller, signal) : refuse(`There is no tool ${target}.`);
    }
    if (name === BACKGROUND_RESULT && this.host.has(name)) return this.backgroundResult(args, caller);
    if (this.host.has(name)) return this.hostCall(name, args, caller, signal);
    if (this.o.bridge?.has(name)) return this.o.bridge.call(name, args, signal);
    return refuse(`There is no tool ${name}.`);
  }

  private async hostCall(name: string, args: Json, caller: CallerInfo, signal?: AbortSignal): Promise<CallResult> {
    if (caller.unattributed) return refuse(`Helm could not tell which agent made this ${name} call, so it was not sent. Try again.`);
    const t = this.host.get(name) as HostTool;
    const r = await this.o.channel.call(name, args, {
      limitBytes: t.requestLimitBytes,
      timeoutMs: t.timeoutMs,
      mutating: t.mutating,
      caller: { child: caller.child, ...(caller.toolCallId ? { toolCallId: caller.toolCallId } : {}) },
      signal,
    });
    if (r.ok && name === "app_set_chat_archived" && (r.result as Json | null)?.archivePending === true) this.o.onEndTurn?.();
    return r;
  }

  private search(args: Json): Json {
    const q = typeof args.query === "string" ? args.query.toLowerCase().split(/\s+/).filter(Boolean) : [];
    const offset = Number.isInteger(args.offset) && (args.offset as number) > 0 ? (args.offset as number) : 0;
    const hits = this.catalogue().filter((t) => q.every((w) => t.name.includes(w) || t.description.toLowerCase().includes(w)));
    const page = hits.slice(offset, offset + SEARCH_PAGE).map((t) => ({ name: t.name, description: t.description, inputSchema: t.inputSchema, ...(t.guidelines ? { guidelines: t.guidelines } : {}) }));
    const out: Json = { tools: page, total: hits.length };
    if (offset + SEARCH_PAGE < hits.length) out.nextOffset = offset + SEARCH_PAGE;
    return out;
  }
}

/** The `--mcp-config` value that registers this server with Claude Code (an SDK-type server). */
export function sdkMcpConfig(): string {
  return JSON.stringify({ mcpServers: { [SERVER_NAME]: { type: "sdk", name: SERVER_NAME } } });
}
