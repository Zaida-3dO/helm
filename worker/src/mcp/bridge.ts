// The worker's MCP bridge: holds the resolved server list from the host's
// `mcp_config` line, connects lazily per server, discovers and loads tools for the model, runs
// calls, and reports per-server status as `mcp_status` lines.
//
// - Limits and naming: at most 8 MCP requests run at once in a worker.
// - Redaction: every server's resolved secrets are scrubbed from names, descriptions, schemas,
//   results and errors before they leave the bridge.
// - A server Claude Code already loads natively (same normalised name or URL) is hidden
//   from Helm's client and reported as `native`, so nothing loads twice.
// - A config change replaces only the servers whose definition changed; their loaded tools are
//   dropped, so a child can call only tools that are discovered and still registered.

import { HttpTransport, type Resolver } from "./http.ts";
import { generatedName, label, matches, terms } from "./naming.ts";
import { formatResult } from "./output.ts";
import { Redactor } from "./redact.ts";
import { McpSession, type Transport } from "./session.ts";
import { StdioTransport } from "./stdio.ts";
import {
  LIMITS,
  McpError,
  type CallResult,
  type DiscoveryResult,
  type Json,
  type LoadedTool,
  type McpConfig,
  type McpLimits,
  type McpTool,
  type NativeServer,
  type ResolvedServer,
  type ServerState,
  type ServerStatus,
} from "./types.ts";

export interface BridgeOptions {
  /** The chat's folder: stdio servers start there. */
  cwd?: string;
  env?: NodeJS.ProcessEnv;
  limits?: Partial<McpLimits>;
  resolver?: Resolver;
  now?: () => number;
}

export interface McpBridge {
  configure(cfg: McpConfig): void;
  /** The servers the CLI reported loading natively at start-up. */
  noteNative(servers: NativeServer[]): void;
  status(): ServerStatus[];
  /** Ids `app_mcp_tools` may name: enabled and not provided natively. */
  serverIds(): string[];
  tools(args: { server: string; query?: string }): Promise<DiscoveryResult>;
  /** True when `name` is a discovered tool whose server is still configured unchanged. */
  isLoaded(name: string): boolean;
  call(name: string, args: Json, signal?: AbortSignal): Promise<CallResult>;
  close(): void;
}

/** Normalised server name: case, separators and punctuation folded. */
export function normName(n: string): string {
  return n.toLowerCase().replace(/[^a-z0-9]/g, "");
}

/** Normalised URL: lowercase origin plus path without a trailing slash; undefined if invalid. */
export function normUrl(u: string | undefined): string | undefined {
  if (!u) return undefined;
  try {
    const x = new URL(u);
    return `${x.protocol}//${x.host.toLowerCase()}${x.pathname.replace(/\/+$/, "")}`;
  } catch {
    return undefined;
  }
}

/** Native servers the CLI must not load: any named like Helm's own server. */
export function deniedNative(names: string[]): string[] {
  return names.filter((n) => normName(n) === "helm");
}

interface Entry {
  server: ResolvedServer;
  key: string;
  red: Redactor;
  session?: McpSession;
  connecting?: Promise<McpSession>;
  status: ServerStatus;
  hidden: boolean;
}

interface Loaded {
  serverId: string;
  key: string;
  tool: string;
  view: LoadedTool;
}

class Slots {
  private used = 0;
  private readonly queue: (() => void)[] = [];
  private readonly max: number;
  constructor(max: number) {
    this.max = max;
  }
  async run<T>(f: () => Promise<T>): Promise<T> {
    if (this.used >= this.max) await new Promise<void>((r) => this.queue.push(r));
    this.used++;
    try {
      return await f();
    } finally {
      this.used--;
      this.queue.shift()?.();
    }
  }
}

export function createMcpBridge(sink: (line: Json) => void, opts: BridgeOptions = {}): McpBridge {
  const limits: McpLimits = { ...LIMITS, ...(opts.limits ?? {}) };
  const now = opts.now ?? Date.now;
  const env = opts.env ?? process.env;
  const entries = new Map<string, Entry>();
  const loaded = new Map<string, Loaded>();
  const slots = new Slots(limits.maxConcurrent);
  let native: NativeServer[] = [];
  let closed = false;

  const emit = (e: Entry, state: ServerState, extra: { error?: string; toolCount?: number } = {}) => {
    const st: ServerStatus = { serverId: e.server.id, state, at: now() };
    if (extra.error !== undefined) st.error = e.red.text(extra.error).slice(0, 2000);
    if (extra.toolCount !== undefined) st.toolCount = extra.toolCount;
    e.status = st;
    sink({ type: "mcp_status", ...st });
  };

  const isNative = (s: ResolvedServer): boolean => {
    const names = new Set(native.map((n) => normName(n.name)));
    const urls = new Set(native.map((n) => normUrl(n.url)).filter(Boolean));
    const u = normUrl(s.url);
    return names.has(normName(s.id)) || names.has(normName(s.name)) || (u !== undefined && urls.has(u));
  };

  const dropLoaded = (serverId: string) => {
    for (const [k, v] of loaded) if (v.serverId === serverId) loaded.delete(k);
  };

  const retire = (e: Entry) => {
    e.session?.close("configuration changed");
    e.session = undefined;
    e.connecting = undefined;
    dropLoaded(e.server.id);
  };

  const refreshHidden = () => {
    for (const e of entries.values()) {
      const h = isNative(e.server);
      if (h === e.hidden) continue;
      e.hidden = h;
      if (h) {
        retire(e);
        emit(e, "native");
      } else {
        emit(e, e.server.error ? "failed" : "idle", e.server.error ? { error: e.server.error } : {});
      }
    }
  };

  const transportFor = (s: ResolvedServer): Transport & { start?: () => void } => {
    if (s.transport === "stdio") {
      const red = entries.get(s.id)?.red;
      return new StdioTransport(s, { cwd: opts.cwd, env, maxRecordBytes: limits.maxRecordBytes, redact: red ? (x) => red.text(x) : undefined });
    }
    return new HttpTransport(s, { limits, resolver: opts.resolver });
  };

  const connect = (e: Entry): Promise<McpSession> => {
    if (e.session && !e.session.closed) return Promise.resolve(e.session);
    if (e.connecting) return e.connecting;
    if (e.server.error) return Promise.reject(new McpError(e.server.error));
    emit(e, "connecting");
    const attempt = (async () => {
      const t = transportFor(e.server);
      const session = new McpSession(t, limits, e.server.name);
      t.start?.();
      await session.initialize();
      return session;
    })();
    e.connecting = attempt;
    attempt.then(
      (session) => {
        if (e.connecting !== attempt) {
          session.close("configuration changed");
          return;
        }
        e.connecting = undefined;
        e.session = session;
        session.onEnd = (why) => {
          if (e.session !== session) return;
          e.session = undefined;
          if (why !== "closed" && why !== "configuration changed") emit(e, "failed", { error: why });
        };
        emit(e, "connected");
      },
      (err: unknown) => {
        if (e.connecting !== attempt) return;
        e.connecting = undefined;
        emit(e, "failed", { error: (err as Error).message });
      },
    );
    return attempt;
  };

  const publicError = (e: Entry, err: unknown): McpError => {
    const m = err instanceof McpError ? err : new McpError(String((err as Error)?.message ?? err), { changed: "maybe" });
    return new McpError(e.red.text(m.message), { changed: m.changed, signin: m.signin });
  };

  const entryFor = (id: string): Entry => {
    const e = entries.get(id);
    if (!e) throw new McpError(`No enabled MCP server has the id ${id}.`);
    if (e.hidden) throw new McpError(`${e.server.name} is loaded natively by Claude Code; call its tools directly.`);
    return e;
  };

  return {
    configure(cfg: McpConfig) {
      if (closed) return;
      const next = new Map<string, ResolvedServer>();
      for (const s of cfg.servers ?? []) if (s && typeof s.id === "string") next.set(s.id, s);
      for (const [id, e] of entries) {
        if (!next.has(id)) {
          retire(e);
          entries.delete(id);
        }
      }
      if (cfg.native) native = cfg.native;
      for (const [id, s] of next) {
        const key = JSON.stringify(s);
        const old = entries.get(id);
        if (old && old.key === key) continue;
        if (old) retire(old);
        const e: Entry = {
          server: s,
          key,
          red: new Redactor(s.secrets ?? []),
          status: { serverId: id, state: "idle", at: now() },
          hidden: isNative(s),
        };
        entries.set(id, e);
        if (e.hidden) emit(e, "native");
        else if (s.error) emit(e, "failed", { error: s.error });
        else emit(e, "idle");
      }
      refreshHidden();
    },

    noteNative(servers: NativeServer[]) {
      native = servers;
      refreshHidden();
    },

    status() {
      return [...entries.values()].map((e) => e.status);
    },

    serverIds() {
      return [...entries.values()].filter((e) => !e.hidden).map((e) => e.server.id);
    },

    async tools({ server, query }) {
      const e = entryFor(server);
      if (query !== undefined && query.length > 200) throw new McpError("The query is at most 200 characters.");
      return slots.run(async () => {
        let list: McpTool[];
        try {
          const session = await connect(e);
          list = await session.listTools();
        } catch (err) {
          const pe = publicError(e, err);
          throw new McpError(pe.message);
        }
        emit(e, "connected", { toolCount: list.length });
        const t = terms(query, e.server.id);
        // A tool whose name reveals a secret cannot be renamed safely, so it is not offered.
        const safe = list.filter((tool) => !e.red.reveals(tool.name));
        const hits = safe.filter((tool) => matches(t, [tool.name, tool.description, tool.title, label(e.server.name, tool.name)]));
        const out: LoadedTool[] = [];
        for (const tool of hits) {
          if (out.length >= limits.maxPerDiscovery) break;
          const name = generatedName(e.server.id, tool.name);
          const have = loaded.get(name);
          if (!have && loaded.size >= limits.maxLoaded) break;
          const view: LoadedTool = {
            name,
            label: e.red.text(label(e.server.name, tool.name)),
            description: e.red.text(
              `${tool.description ?? tool.title ?? ""}\n\nA cancelled or failed call does not prove a write did not happen.`.trim(),
            ),
            inputSchema: e.red.value(tool.inputSchema),
          };
          loaded.set(name, { serverId: e.server.id, key: e.key, tool: tool.name, view });
          out.push(view);
        }
        const result: DiscoveryResult = { server: e.server.id, total: safe.length, matches: hits.length, loaded: out, more: hits.length > out.length };
        if (e.server.guidance) result.guidance = e.server.guidance;
        return result;
      });
    },

    isLoaded(name: string) {
      const l = loaded.get(name);
      const e = l && entries.get(l.serverId);
      return !!(l && e && !e.hidden && e.key === l.key);
    },

    async call(name: string, args: Json, signal?: AbortSignal) {
      const l = loaded.get(name);
      if (!l) throw new McpError(`${name} has not been discovered: load it with app_mcp_tools first.`);
      const e = entries.get(l.serverId);
      if (!e || e.hidden || e.key !== l.key) {
        loaded.delete(name);
        throw new McpError(`${name} is no longer available because its server changed. Discover it again with app_mcp_tools.`);
      }
      return slots.run(async () => {
        let raw: Json;
        try {
          const session = await connect(e);
          raw = await session.callTool(l.tool, args ?? {}, signal);
        } catch (err) {
          throw publicError(e, err);
        }
        const r = formatResult(raw, e.red, limits);
        return { content: r.content, isError: r.isError || undefined, source: { kind: "mcp", server: e.red.text(e.server.name), tool: e.red.text(l.tool) } };
      });
    },

    close() {
      closed = true;
      for (const e of entries.values()) retire(e);
      entries.clear();
      loaded.clear();
    },
  };
}
