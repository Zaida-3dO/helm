// The in-process MCP server named "helm".
//
// Claude Code reaches it over its control channel: each JSON-RPC message arrives as an
// `mcp_message` control request and the answer goes back in the control response. The server
// lists exactly the host tools from launch.json `hostTools[]`, plus the worker's own
// compatibility tools (`app_list_tools` and `app_mcp_call` always; `app_mcp_tools` whenever there
// is an MCP client, see names.ts). Host tools go to the host through the request channel; MCP tools go to the
// MCP bridge (worker/src/mcp).

import type { Json } from "../protocol/types.ts";
import { DEFAULT_TIMEOUT_MS, NOTHING_CHANGED, type CallResult, type RequestChannel } from "./channel.ts";
import { capToolResult, type OutputCap } from "./cap.ts";
import { COMPAT_ALWAYS, COMPAT_WITH_MCP } from "./names.ts";

export const SERVER_NAME = "helm";
/** The CLI's name for a tool on this server. */
export const toolPrefix = `mcp__${SERVER_NAME}__`;
export const SEARCH_PAGE = 10;
const SMALL = 8 * 1024;

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

export class HostToolServer {
  private readonly o: ServerOptions;
  private readonly host = new Map<string, HostTool>();

  constructor(o: ServerOptions) {
    this.o = o;
    for (const t of o.hostTools) this.host.set(t.name, t);
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
        const result = toolResult(await this.call(name, args, caller, signal));
        return ok(this.o.outputCap ? capToolResult(result, name, this.o.outputCap) : result);
      }
      default:
        return err(-32601, `Method not found: ${String(msg.method)}`);
    }
  }

  /** Route one tool call. */
  async call(name: string, args: Json, caller: CallerInfo, signal?: AbortSignal): Promise<CallResult> {
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
      const inner = (args.arguments ?? {}) as Json;
      if (target === MCP_CALL.name || target === LIST_TOOLS.name || target === MCP_TOOLS.name) {
        return refuse(`${target} cannot be called through app_mcp_call.`);
      }
      if (this.o.bridge?.has(target)) return this.o.bridge.call(target, inner, signal);
      // A child reaches only discovered MCP tools through app_mcp_call, never a host tool.
      if (caller.child) return refuse(`${target} is only available to the parent chat.`);
      return this.host.has(target) ? this.hostCall(target, inner, caller, signal) : refuse(`There is no tool ${target}.`);
    }
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
