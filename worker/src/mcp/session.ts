// One MCP session over a transport: JSON-RPC 2.0 request matching, initialize, paged tools/list
// and tools/call, with per-request timeouts. Server-to-client requests other than ping are
// answered "method not found": Helm offers no sampling, roots or elicitation.

import { McpError, type Json, type McpLimits, type McpTool } from "./types.ts";

export const PROTOCOL_VERSION = "2025-06-18";

export interface Transport {
  /** Sends one message. Replies arrive through `onMessage`, possibly before this resolves. */
  send(msg: Json, signal?: AbortSignal): Promise<void>;
  onMessage: (msg: Json) => void;
  onClose: (why: string) => void;
  /** After initialize: the negotiated protocol version (HTTP sends it as a header). */
  setProtocolVersion?(v: string): void;
  close(): void;
}

interface Pending {
  resolve: (v: unknown) => void;
  reject: (e: Error) => void;
}

function timeoutSignal(ms: number, outer?: AbortSignal): { signal: AbortSignal; cancel: () => void; timedOut: () => boolean } {
  const ctl = new AbortController();
  let fired = false;
  const t = setTimeout(() => {
    fired = true;
    ctl.abort(new Error("timeout"));
  }, ms);
  const onOuter = () => ctl.abort(outer?.reason ?? new Error("aborted"));
  if (outer) {
    if (outer.aborted) onOuter();
    else outer.addEventListener("abort", onOuter, { once: true });
  }
  return {
    signal: ctl.signal,
    cancel: () => {
      clearTimeout(t);
      outer?.removeEventListener("abort", onOuter);
    },
    timedOut: () => fired,
  };
}

export class McpSession {
  private nextId = 1;
  private readonly pending = new Map<number, Pending>();
  private closedWhy: string | undefined;
  serverInfo: Json | undefined;
  /** Called once when the session ends for any reason. */
  onEnd: (why: string) => void = () => {};

  private readonly transport: Transport;
  private readonly limits: McpLimits;
  private readonly label: string;

  constructor(transport: Transport, limits: McpLimits, label: string) {
    this.transport = transport;
    this.limits = limits;
    this.label = label;
    transport.onMessage = (m) => this.onMessage(m);
    transport.onClose = (why) => this.onClosed(why);
  }

  get closed(): boolean {
    return this.closedWhy !== undefined;
  }

  private onMessage(m: Json): void {
    if (m.jsonrpc !== "2.0") return;
    const hasMethod = typeof m.method === "string";
    if (hasMethod && m.id !== undefined && m.id !== null) {
      // A request from the server.
      const reply =
        m.method === "ping"
          ? { jsonrpc: "2.0", id: m.id, result: {} }
          : { jsonrpc: "2.0", id: m.id, error: { code: -32601, message: "Method not found" } };
      this.transport.send(reply).catch(() => {});
      return;
    }
    if (hasMethod) return; // a notification: nothing to do
    if (typeof m.id !== "number") return;
    const p = this.pending.get(m.id);
    if (!p) return;
    this.pending.delete(m.id);
    if (m.error && typeof m.error === "object") {
      const e = m.error as Json;
      p.reject(new McpError(`${this.label} returned an error: ${String(e.message ?? "unknown error")}`));
    } else {
      p.resolve(m.result);
    }
  }

  private onClosed(why: string): void {
    if (this.closedWhy !== undefined) return;
    this.closedWhy = why;
    this.onEnd(why);
    for (const p of this.pending.values()) p.reject(new McpError(why, { changed: "maybe" }));
    this.pending.clear();
  }

  /** One request with its own timeout. A timed-out or aborted request closes the session. */
  async request(method: string, params: Json | undefined, ms: number, outer?: AbortSignal, timeoutText?: string): Promise<unknown> {
    if (this.closedWhy !== undefined) throw new McpError(this.closedWhy);
    const id = this.nextId++;
    const t = timeoutSignal(ms, outer);
    const result = new Promise<unknown>((resolve, reject) => {
      this.pending.set(id, { resolve, reject });
      t.signal.addEventListener(
        "abort",
        () => {
          if (!this.pending.delete(id)) return;
          const msg = t.timedOut()
            ? (timeoutText ?? `${this.label} did not answer ${method} within ${Math.round(ms / 1000)} s.`)
            : `The ${method} request to ${this.label} was cancelled.`;
          reject(new McpError(msg, { changed: "maybe" }));
          this.close(msg);
        },
        { once: true },
      );
    });
    const msg: Json = { jsonrpc: "2.0", id, method };
    if (params !== undefined) msg.params = params;
    try {
      await this.transport.send(msg, t.signal);
    } catch (e) {
      if (this.pending.delete(id)) {
        t.cancel();
        const err = e instanceof McpError ? e : new McpError(`${this.label}: ${(e as Error).message}`, { changed: "maybe" });
        throw err;
      }
    }
    try {
      return await result;
    } finally {
      t.cancel();
    }
  }

  notify(method: string, params?: Json): void {
    const msg: Json = { jsonrpc: "2.0", method };
    if (params !== undefined) msg.params = params;
    this.transport.send(msg).catch(() => {});
  }

  async initialize(signal?: AbortSignal): Promise<void> {
    const r = (await this.request(
      "initialize",
      {
        protocolVersion: PROTOCOL_VERSION,
        capabilities: {},
        clientInfo: { name: "helm", version: "0.1.0" },
      },
      this.limits.startupMs,
      signal,
      `${this.label} did not start within ${Math.round(this.limits.startupMs / 1000)} s.`,
    )) as Json | undefined;
    if (!r || typeof r !== "object") throw new McpError(`${this.label} sent an invalid initialize reply.`);
    this.serverInfo = r;
    const v = typeof r.protocolVersion === "string" ? r.protocolVersion : PROTOCOL_VERSION;
    this.transport.setProtocolVersion?.(v);
    this.notify("notifications/initialized");
  }

  /** Every tool, paged (10 s a page, 45 s in all, at most 512). Non-object schemas are dropped. */
  async listTools(): Promise<McpTool[]> {
    const deadline = Date.now() + this.limits.discoveryMs;
    const out: McpTool[] = [];
    let cursor: string | undefined;
    for (let page = 0; page < 100; page++) {
      const left = deadline - Date.now();
      if (left <= 0) throw new McpError(`${this.label} did not list its tools within ${Math.round(this.limits.discoveryMs / 1000)} s.`);
      const r = (await this.request("tools/list", cursor ? { cursor } : {}, Math.min(this.limits.pageMs, left))) as Json | undefined;
      const tools = Array.isArray(r?.tools) ? (r.tools as Json[]) : [];
      for (const t of tools) {
        if (out.length >= this.limits.maxListed) return out;
        if (typeof t?.name !== "string" || t.name === "") continue;
        const schema = t.inputSchema as Json | undefined;
        if (!schema || typeof schema !== "object" || Array.isArray(schema) || schema.type !== "object") continue;
        out.push({
          name: t.name,
          title: typeof t.title === "string" ? t.title : undefined,
          description: typeof t.description === "string" ? t.description : undefined,
          inputSchema: schema,
        });
      }
      cursor = typeof r?.nextCursor === "string" && r.nextCursor !== "" ? r.nextCursor : undefined;
      if (!cursor) return out;
    }
    return out;
  }

  async callTool(name: string, args: Json, signal?: AbortSignal): Promise<Json> {
    const r = await this.request(
      "tools/call",
      { name, arguments: args },
      this.limits.callMs,
      signal,
      `The call to ${this.label} timed out after ${Math.round(this.limits.callMs / 1000)} s. It may have happened on the server anyway: check before retrying a write.`,
    );
    if (!r || typeof r !== "object") throw new McpError(`${this.label} sent an invalid tool result.`, { changed: "maybe" });
    return r as Json;
  }

  close(why = "closed"): void {
    this.onClosed(why);
    this.transport.close();
  }
}
