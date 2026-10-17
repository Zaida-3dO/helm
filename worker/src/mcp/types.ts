// Shapes of Helm's MCP client in the worker. Dependency-free: JSON-RPC 2.0 over stdio or
// streamable HTTP, only `node:` built-ins.

export type Json = Record<string, unknown>;

/** One enabled server, references already resolved by the host. */
export interface ResolvedServer {
  id: string;
  name: string;
  transport: "stdio" | "http" | "sse";
  command?: string;
  args?: string[];
  env?: Record<string, string>;
  url?: string;
  headers?: Record<string, string>;
  /** The user's opt-in for plain http on the private network. */
  allowPrivateHttp: boolean;
  /** Every resolved secret, for redaction. */
  secrets?: string[];
  /** The preset's guidance for the agent, returned with discovery results. */
  guidance?: string;
  /** The host could not resolve a reference: report failed, never connect. */
  error?: string;
}

/** A server Claude Code already loads natively: from the CLI's start-up report or a
 * native config the host can read. */
export interface NativeServer {
  name: string;
  url?: string;
}

/** The `mcp_config` stdin line's body. */
export interface McpConfig {
  revision: string;
  servers: ResolvedServer[];
  native?: NativeServer[];
}

export type ServerState = "idle" | "connecting" | "connected" | "failed" | "signin_required" | "native";

export interface ServerStatus {
  serverId: string;
  state: ServerState;
  error?: string;
  toolCount?: number;
  at: number;
}

/** A tool as a server lists it. */
export interface McpTool {
  name: string;
  title?: string;
  description?: string;
  inputSchema: Json;
}

/** One tool loaded for the model by `app_mcp_tools`. */
export interface LoadedTool {
  /** The generated name the model calls it by. */
  name: string;
  /** "<server name> · <tool name>". */
  label: string;
  description: string;
  inputSchema: Json;
}

export interface DiscoveryResult {
  server: string;
  total: number;
  matches: number;
  loaded: LoadedTool[];
  more: boolean;
  /** The server's preset guidance, if any. */
  guidance?: string;
}

/** An MCP tool result, after the output limits and redaction. */
export interface CallResult {
  content: Json[];
  isError?: boolean;
  /** Where the call went, for the transcript row. */
  source: { kind: "mcp"; server: string; tool: string };
}

/** Request and output limits. Tests override them. */
export interface McpLimits {
  startupMs: number;
  pageMs: number;
  discoveryMs: number;
  callMs: number;
  maxListed: number;
  maxPerDiscovery: number;
  maxLoaded: number;
  maxConcurrent: number;
  maxRecordBytes: number;
  maxTextBytes: number;
  maxLines: number;
  maxImages: number;
  maxImageBytes: number;
  maxImageTotal: number;
  maxRedirects: number;
}

export const LIMITS: McpLimits = {
  startupMs: 30_000,
  pageMs: 10_000,
  discoveryMs: 45_000,
  callMs: 60_000,
  maxListed: 512,
  maxPerDiscovery: 20,
  maxLoaded: 128,
  maxConcurrent: 8,
  maxRecordBytes: 4 * 1024 * 1024,
  maxTextBytes: 50_000,
  maxLines: 2_000,
  maxImages: 4,
  maxImageBytes: 2 * 1024 * 1024,
  maxImageTotal: 4 * 1024 * 1024,
  maxRedirects: 3,
};

/** A failure with public text. `changed` says whether a write may have happened anyway. */
export class McpError extends Error {
  readonly changed: "no" | "maybe";
  readonly signin: boolean;
  constructor(message: string, opts: { changed?: "no" | "maybe"; signin?: boolean } = {}) {
    super(message);
    this.changed = opts.changed ?? "no";
    this.signin = opts.signin ?? false;
  }
}
