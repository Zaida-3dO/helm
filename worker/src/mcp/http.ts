// Streamable HTTP transport (the public MCP specification): each message is a POST; the reply is
// JSON or an SSE stream; the server's session id is echoed back. Built on `node:http`/`node:https`
// rather than `fetch`, for two reasons:
//
// - Plain http to a non-loopback host is allowed only with the server's opt-in and only to a
//   private address. A `lookup` hook checks the address the socket actually connects to, so a
//   name cannot pass the check on one DNS answer and connect on another (DNS rebinding).
// - The redirect rule (≤ 3, 307/308 only, same origin, no credentials or fragment) needs
//   manual redirects.

import dns from "node:dns";
import http from "node:http";
import https from "node:https";
import net from "node:net";
import { McpError, type Json, type McpLimits, type ResolvedServer } from "./types.ts";
import type { Transport } from "./session.ts";

export type AddressClass = "loopback" | "private" | "public";

function classV4(a: string): AddressClass {
  const o = a.split(".").map((x) => Number(x));
  if (o.length !== 4 || o.some((n) => !Number.isInteger(n) || n < 0 || n > 255)) return "public";
  if (o[0] === 127) return "loopback";
  if (o[0] === 10) return "private";
  if (o[0] === 172 && o[1]! >= 16 && o[1]! <= 31) return "private";
  if (o[0] === 192 && o[1] === 168) return "private";
  if (o[0] === 100 && o[1]! >= 64 && o[1]! <= 127) return "private";
  return "public";
}

/** Eight 16-bit groups of an IPv6 address (with an embedded dotted IPv4 tail), or undefined. */
function v6Groups(a: string): number[] | undefined {
  let s = a.toLowerCase();
  const pct = s.indexOf("%");
  if (pct >= 0) s = s.slice(0, pct);
  let tail: number[] = [];
  const dotted = /(\d+\.\d+\.\d+\.\d+)$/.exec(s);
  if (dotted) {
    const o = dotted[1]!.split(".").map(Number);
    tail = [(o[0]! << 8) | o[1]!, (o[2]! << 8) | o[3]!];
    s = s.slice(0, s.length - dotted[1]!.length);
    if (s.endsWith(":") && !s.endsWith("::")) s = s.slice(0, -1);
  }
  const halves = s.split("::");
  if (halves.length > 2) return undefined;
  const part = (x: string) => (x === "" ? [] : x.split(":").map((g) => parseInt(g, 16)));
  const head = part(halves[0]!);
  const rest = halves.length === 2 ? part(halves[1]!) : [];
  const fill = 8 - head.length - rest.length - tail.length;
  if (fill < 0 || (halves.length === 1 && fill !== 0)) return undefined;
  const g = [...head, ...new Array(halves.length === 2 ? fill : 0).fill(0), ...rest, ...tail];
  return g.length === 8 && g.every((n) => Number.isInteger(n) && n >= 0 && n <= 0xffff) ? g : undefined;
}

export function classifyAddress(addr: string): AddressClass {
  const fam = net.isIP(addr);
  if (fam === 4) return classV4(addr);
  if (fam !== 6) return "public";
  const g = v6Groups(addr);
  if (!g) return "public";
  if (g.slice(0, 7).every((x) => x === 0) && g[7] === 1) return "loopback";
  if (g.slice(0, 5).every((x) => x === 0) && g[5] === 0xffff) {
    return classV4(`${g[6]! >> 8}.${g[6]! & 255}.${g[7]! >> 8}.${g[7]! & 255}`);
  }
  if ((g[0]! & 0xfe00) === 0xfc00) return "private";
  return "public";
}

export type Resolver = (host: string) => Promise<{ address: string; family: number }[]>;

const systemResolver: Resolver = (host) => dns.promises.lookup(host, { all: true });

/**
 * The connection rule for one URL. Returns a `lookup` to give the request when the
 * host is a name that must resolve to an allowed address, or throws a public refusal.
 */
export function connectRule(u: URL, allowPrivate: boolean, name: string, resolver: Resolver = systemResolver): net.LookupFunction | undefined {
  if (u.protocol === "https:") return undefined;
  if (u.protocol !== "http:") throw new McpError(`${name}: only https and http URLs are supported.`);
  const host = u.hostname.replace(/^\[|\]$/g, "");
  const allowed = (c: AddressClass) => c === "loopback" || (allowPrivate && c === "private");
  const refusal = () =>
    new McpError(
      allowPrivate
        ? `${name}: plain http is only allowed to this computer or a private-network address.`
        : `${name}: plain http to another machine needs "Allow unencrypted on my network" for this server in Settings → MCP.`,
    );
  if (net.isIP(host)) {
    if (!allowed(classifyAddress(host))) throw refusal();
    return undefined;
  }
  const loopbackName = host.toLowerCase() === "localhost";
  if (!loopbackName && !allowPrivate) throw refusal();
  const ok = (c: AddressClass) => (loopbackName ? c === "loopback" : allowed(c));
  return ((hostname: string, options: dns.LookupOptions, cb: (...a: unknown[]) => void) => {
    resolver(hostname).then(
      (list) => {
        if (list.length === 0) return cb(Object.assign(new Error(`${hostname} did not resolve`), { code: "ENOTFOUND" }));
        if (!list.every((r) => ok(classifyAddress(r.address)))) {
          return cb(
            Object.assign(new Error(`${name}: ${hostname} resolves to an address outside this computer or the private network, so plain http is refused.`), {
              code: "EHELMPUBLIC",
            }),
          );
        }
        if (options && (options as { all?: boolean }).all) cb(null, list);
        else cb(null, list[0]!.address, list[0]!.family);
      },
      (e: Error) => cb(e),
    );
  }) as unknown as net.LookupFunction;
}

interface RawResponse {
  status: number;
  headers: http.IncomingHttpHeaders;
  body: http.IncomingMessage;
}

export interface HttpOptions {
  limits: McpLimits;
  resolver?: Resolver;
}

export class HttpTransport implements Transport {
  onMessage: (msg: Json) => void = () => {};
  onClose: (why: string) => void = () => {};
  private sessionId: string | undefined;
  private protocolVersion: string | undefined;
  private ended = false;
  private readonly origin: string;
  private readonly live = new Set<http.ClientRequest>();

  private readonly server: ResolvedServer;
  private readonly opts: HttpOptions;

  constructor(server: ResolvedServer, opts: HttpOptions) {
    this.server = server;
    this.opts = opts;
    if (server.transport !== "http") {
      throw new McpError(`${server.name}: the older HTTP+SSE transport is not supported yet; use the server's streamable http endpoint.`);
    }
    const u = new URL(server.url ?? "");
    this.origin = u.origin;
    // Fail early on a refused address.
    connectRule(u, server.allowPrivateHttp, server.name, opts.resolver);
  }

  setProtocolVersion(v: string): void {
    this.protocolVersion = v;
  }

  private post(url: URL, body: string, signal?: AbortSignal): Promise<RawResponse> {
    const lookup = connectRule(url, this.server.allowPrivateHttp, this.server.name, this.opts.resolver);
    const headers: Record<string, string> = { ...(this.server.headers ?? {}) };
    headers["content-type"] = "application/json";
    headers["accept"] = "application/json, text/event-stream";
    headers["content-length"] = String(Buffer.byteLength(body));
    if (this.sessionId) headers["mcp-session-id"] = this.sessionId;
    if (this.protocolVersion) headers["mcp-protocol-version"] = this.protocolVersion;
    const mod = url.protocol === "https:" ? https : http;
    return new Promise((resolve, reject) => {
      const req = mod.request(url, { method: "POST", headers, lookup, signal }, (res) => {
        resolve({ status: res.statusCode ?? 0, headers: res.headers, body: res });
      });
      this.live.add(req);
      req.on("close", () => this.live.delete(req));
      req.on("error", (e: Error & { code?: string }) => {
        if (e.code === "EHELMPUBLIC") reject(new McpError(e.message));
        else if (signal?.aborted) reject(new McpError(`The request to ${this.server.name} was cancelled.`, { changed: "maybe" }));
        else reject(new McpError(`${this.server.name} could not be reached: ${e.message}`, { changed: "maybe" }));
      });
      req.end(body);
    });
  }

  /** POST, following redirects under the redirect rule above. */
  private async postFollowing(body: string, signal?: AbortSignal): Promise<RawResponse> {
    let url = new URL(this.server.url ?? "");
    const seen = new Set<string>([url.href]);
    for (let hop = 0; ; hop++) {
      const res = await this.post(url, body, signal);
      if (res.status < 300 || res.status >= 400) return res;
      res.body.resume();
      if (res.status !== 307 && res.status !== 308) {
        throw new McpError(`${this.server.name} answered with a redirect (HTTP ${res.status}) Helm does not follow; only 307 and 308 are allowed.`);
      }
      if (hop >= this.opts.limits.maxRedirects) throw new McpError(`${this.server.name} redirected more than ${this.opts.limits.maxRedirects} times.`);
      const loc = res.headers.location;
      if (!loc) throw new McpError(`${this.server.name} sent a redirect without a location.`);
      const next = new URL(loc, url);
      if (next.origin !== this.origin) throw new McpError(`${this.server.name} redirected to another site, which Helm refuses.`);
      if (next.username || next.password || next.hash) throw new McpError(`${this.server.name} redirected to a URL with credentials or a fragment, which Helm refuses.`);
      if (seen.has(next.href)) throw new McpError(`${this.server.name} sent a redirect loop.`);
      seen.add(next.href);
      url = next;
    }
  }

  async send(msg: Json, signal?: AbortSignal): Promise<void> {
    if (this.ended) throw new McpError(`${this.server.name} is not connected.`);
    const res = await this.postFollowing(JSON.stringify(msg), signal);
    const sid = res.headers["mcp-session-id"];
    if (typeof sid === "string" && sid !== "") this.sessionId = sid;
    if (res.status === 202 || res.status === 204) {
      res.body.resume();
      return;
    }
    if (res.status === 401 || res.status === 403) {
      res.body.resume();
      throw new McpError(`${this.server.name} refused the credentials (HTTP ${res.status}). Check its token in Settings → MCP.`, { signin: true });
    }
    if (res.status === 404 && this.sessionId) {
      res.body.resume();
      this.end(`${this.server.name} ended the session.`);
      throw new McpError(`${this.server.name} ended the session; it will reconnect on the next call.`, { changed: "maybe" });
    }
    if (res.status < 200 || res.status >= 300) {
      res.body.resume();
      throw new McpError(`${this.server.name} answered HTTP ${res.status}.`, { changed: "maybe" });
    }
    const type = String(res.headers["content-type"] ?? "").toLowerCase();
    if (type.startsWith("text/event-stream")) {
      await this.readSse(res.body, msg.id);
      return;
    }
    const text = await this.readCapped(res.body);
    if (text.trim() === "") return;
    let v: unknown;
    try {
      v = JSON.parse(text);
    } catch {
      throw new McpError(`${this.server.name} sent a reply that is not JSON.`, { changed: "maybe" });
    }
    for (const m of Array.isArray(v) ? v : [v]) if (m && typeof m === "object") this.onMessage(m as Json);
  }

  private readCapped(body: http.IncomingMessage): Promise<string> {
    const cap = this.opts.limits.maxRecordBytes;
    return new Promise((resolve, reject) => {
      const chunks: Buffer[] = [];
      let n = 0;
      body.on("data", (c: Buffer) => {
        n += c.length;
        if (n > cap) {
          body.destroy();
          reject(new McpError(`${this.server.name} sent a reply larger than 4 MiB.`, { changed: "maybe" }));
          return;
        }
        chunks.push(c);
      });
      body.on("end", () => resolve(Buffer.concat(chunks).toString("utf8")));
      body.on("error", (e) => reject(new McpError(`${this.server.name}: ${e.message}`, { changed: "maybe" })));
    });
  }

  /** Reads SSE events until the reply to `id` arrives (or the stream ends). The record limit is
   * per line, so a long stream is never buffered whole. */
  private readSse(body: http.IncomingMessage, id: unknown): Promise<void> {
    const cap = this.opts.limits.maxRecordBytes;
    return new Promise((resolve, reject) => {
      let buf = "";
      let data: string[] = [];
      let event = "";
      let done = false;
      const finish = (e?: McpError) => {
        if (done) return;
        done = true;
        body.destroy();
        if (e) reject(e);
        else resolve();
      };
      const dispatch = () => {
        if (data.length > 0 && (event === "" || event === "message")) {
          try {
            const m = JSON.parse(data.join("\n")) as Json;
            if (m && typeof m === "object") {
              this.onMessage(m);
              if (id !== undefined && m.id === id && m.method === undefined) finish();
            }
          } catch {
            /* skip a malformed event */
          }
        }
        data = [];
        event = "";
      };
      body.setEncoding("utf8");
      body.on("data", (c: string) => {
        buf += c;
        let i: number;
        while ((i = buf.indexOf("\n")) >= 0) {
          let line = buf.slice(0, i);
          buf = buf.slice(i + 1);
          if (line.endsWith("\r")) line = line.slice(0, -1);
          if (line === "") dispatch();
          else if (line.startsWith("data:")) data.push(line.slice(5).replace(/^ /, ""));
          else if (line.startsWith("event:")) event = line.slice(6).trim();
          if (done) return;
        }
        if (buf.length > cap) finish(new McpError(`${this.server.name} sent an event larger than 4 MiB.`, { changed: "maybe" }));
      });
      body.on("end", () => {
        dispatch();
        finish();
      });
      body.on("error", () => finish());
    });
  }

  private end(why: string): void {
    if (this.ended) return;
    this.ended = true;
    this.onClose(why);
  }

  close(): void {
    this.end("closed");
    for (const r of this.live) r.destroy();
    this.live.clear();
  }
}
