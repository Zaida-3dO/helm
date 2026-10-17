// Fake MCP server: stdio and streamable HTTP (JSON or SSE replies), scripted by a
// small config so tests make no service calls. Not a full MCP implementation: it answers
// initialize, ping, tools/list (paged) and tools/call for a fixed tool set.
//
// stdio:  node server.ts --stdio '<config json>'
// http:   import { startHttpFake } from "./server.ts"; const f = await startHttpFake(config);

import fs from "node:fs";
import http from "node:http";
import type { AddressInfo } from "node:net";

export interface FakeConfig {
  /** Filler tools `tool_0` … beyond the fixed set. */
  tools?: number;
  /** Extra tools with exactly these names. */
  toolNames?: string[];
  /** Echoed in a description, a schema key and value, every result and every error. */
  secret?: string;
  /** tools/list page size (default: everything on one page). */
  pageSize?: number;
  /** stdio: exit at once, or never answer initialize. */
  failStart?: "exit" | "hang";
  /** How long the `slow` tool takes. */
  slowMs?: number;
  /** http: answer requests with an SSE stream instead of JSON. */
  sse?: boolean;
  /** http: refuse with 401 unless this header has this value. */
  requireHeader?: { name: string; value: string };
  /** http: answer the first POST with this redirect. */
  redirect?: { status: number; location: string };
  /** Append every received message (one JSON line each) to this file. */
  log?: string;
}

type Json = Record<string, unknown>;

function toolList(cfg: FakeConfig): Json[] {
  const s = cfg.secret;
  const obj = (props: Json = {}) => ({ type: "object", properties: props });
  const tools: Json[] = [
    { name: "echo", description: "Echo the arguments back as text.", inputSchema: obj({ text: { type: "string" } }) },
    {
      name: "secret_echo",
      description: s ? `Returns the key ${s} in its result.` : "Returns a fixed result.",
      inputSchema: obj(s ? { [s]: { type: "string", description: `token ${s}` }, ok: { type: "string" } } : {}),
    },
    { name: "fail", description: "Always fails with a JSON-RPC error.", inputSchema: obj() },
    { name: "tool_error", description: "Returns isError true.", inputSchema: obj() },
    { name: "big", description: "Returns a long text.", inputSchema: obj({ lines: { type: "number" }, bytes: { type: "number" } }) },
    { name: "image", description: "Returns images.", inputSchema: obj({ count: { type: "number" } }) },
    { name: "structured", description: "Returns structured content.", inputSchema: obj() },
    { name: "slow", description: "Waits before answering.", inputSchema: obj() },
    { name: "env", description: "Says whether an environment variable is set.", inputSchema: obj({ name: { type: "string" } }) },
    { name: "bad_schema", description: "Has a non-object schema and must be skipped.", inputSchema: { type: "string" } },
  ];
  for (const n of cfg.toolNames ?? []) tools.push({ name: n, description: "A named extra tool.", inputSchema: obj() });
  for (let i = 0; i < (cfg.tools ?? 0); i++) {
    tools.push({ name: `tool_${i}`, description: `Filler tool number ${i}.`, inputSchema: obj() });
  }
  return tools;
}

const PNG_1PX = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNk+M9QDwADhgGAWjR9awAAAABJRU5ErkJggg==";

async function callTool(cfg: FakeConfig, name: string, args: Json): Promise<{ result?: Json; error?: Json }> {
  const s = cfg.secret ?? "";
  switch (name) {
    case "echo":
      return { result: { content: [{ type: "text", text: JSON.stringify(args) }] } };
    case "secret_echo":
      return { result: { content: [{ type: "text", text: `the secret is ${s}` }], structuredContent: { token: s } } };
    case "fail":
      return { error: { code: -32000, message: `failed while holding ${s || "nothing"}` } };
    case "tool_error":
      return { result: { content: [{ type: "text", text: "tool says no" }], isError: true } };
    case "big": {
      const lines = Number(args.lines ?? 0);
      const bytes = Number(args.bytes ?? 0);
      const text = lines > 0 ? Array.from({ length: lines }, (_, i) => `line ${i}`).join("\n") : "x".repeat(bytes);
      return { result: { content: [{ type: "text", text }] } };
    }
    case "image": {
      const n = Number(args.count ?? 1);
      const content: Json[] = Array.from({ length: n }, () => ({ type: "image", data: PNG_1PX, mimeType: "image/png" }));
      content.push({ type: "image", data: PNG_1PX, mimeType: "image/gif" });
      return { result: { content } };
    }
    case "env":
      return { result: { content: [{ type: "text", text: process.env[String(args.name)] === undefined ? "unset" : `set:${process.env[String(args.name)]}` }] } };
    case "structured":
      return { result: { content: [{ type: "text", text: '{"a":1}' }], structuredContent: { a: 1 } } };
    case "slow":
      await new Promise((r) => setTimeout(r, cfg.slowMs ?? 1000));
      return { result: { content: [{ type: "text", text: "done" }] } };
    default:
      if (/^tool_\d+$/.test(name)) return { result: { content: [{ type: "text", text: `${name} ok` }] } };
      return { error: { code: -32602, message: `unknown tool ${name}` } };
  }
}

/** Answers one JSON-RPC message, or undefined for a notification. */
export async function handle(cfg: FakeConfig, m: Json): Promise<Json | undefined> {
  if (cfg.log) fs.appendFileSync(cfg.log, `${JSON.stringify(m)}\n`);
  if (m.id === undefined || m.id === null) return undefined;
  const reply = (body: Json) => ({ jsonrpc: "2.0", id: m.id, ...body });
  const params = (m.params ?? {}) as Json;
  switch (m.method) {
    case "initialize":
      return reply({ result: { protocolVersion: params.protocolVersion ?? "2025-06-18", capabilities: { tools: {} }, serverInfo: { name: "fake-mcp", version: "1" } } });
    case "ping":
      return reply({ result: {} });
    case "tools/list": {
      const all = toolList(cfg);
      const size = cfg.pageSize ?? all.length;
      const start = typeof params.cursor === "string" ? Number(params.cursor) : 0;
      const page = all.slice(start, start + size);
      const next = start + size < all.length ? String(start + size) : undefined;
      return reply({ result: next ? { tools: page, nextCursor: next } : { tools: page } });
    }
    case "tools/call": {
      const r = await callTool(cfg, String(params.name ?? ""), (params.arguments ?? {}) as Json);
      return reply(r.error ? { error: r.error } : { result: r.result });
    }
    default:
      return reply({ error: { code: -32601, message: "Method not found" } });
  }
}

export interface HttpFake {
  url: string;
  /** Requests seen: method, path and headers (lowercased names). */
  requests: { path: string; headers: http.IncomingHttpHeaders; body: string }[];
  close(): Promise<void>;
}

export async function startHttpFake(cfg: FakeConfig, path = "/mcp"): Promise<HttpFake> {
  const requests: HttpFake["requests"] = [];
  let redirected = false;
  const server = http.createServer((req, res) => {
    let body = "";
    req.setEncoding("utf8");
    req.on("data", (c: string) => (body += c));
    req.on("end", () => {
      requests.push({ path: req.url ?? "", headers: req.headers, body });
      if (cfg.redirect && !redirected) {
        redirected = true;
        res.writeHead(cfg.redirect.status, { location: cfg.redirect.location });
        res.end();
        return;
      }
      if (req.method !== "POST" || !(req.url ?? "").startsWith(path.split("?")[0]!)) {
        res.writeHead(404);
        res.end();
        return;
      }
      if (cfg.requireHeader && req.headers[cfg.requireHeader.name.toLowerCase()] !== cfg.requireHeader.value) {
        res.writeHead(401);
        res.end();
        return;
      }
      let msg: Json;
      try {
        msg = JSON.parse(body) as Json;
      } catch {
        res.writeHead(400);
        res.end();
        return;
      }
      void handle(cfg, msg).then((reply) => {
        const headers: Record<string, string> = { "mcp-session-id": "fake-session-1" };
        if (!reply) {
          res.writeHead(202, headers);
          res.end();
          return;
        }
        if (cfg.sse) {
          res.writeHead(200, { ...headers, "content-type": "text/event-stream" });
          // A notification first, then the reply, then keep the stream open briefly.
          res.write(`event: message\ndata: ${JSON.stringify({ jsonrpc: "2.0", method: "notifications/message", params: { level: "info", data: "hi" } })}\n\n`);
          res.write(`event: message\ndata: ${JSON.stringify(reply)}\n\n`);
          setTimeout(() => res.end(), 50);
        } else {
          res.writeHead(200, { ...headers, "content-type": "application/json" });
          res.end(JSON.stringify(reply));
        }
      });
    });
  });
  await new Promise<void>((r) => server.listen(0, "127.0.0.1", r));
  const port = (server.address() as AddressInfo).port;
  return {
    url: `http://127.0.0.1:${port}${path}`,
    requests,
    close: () =>
      new Promise<void>((r) => {
        server.closeAllConnections();
        server.close(() => r());
      }),
  };
}

function runStdio(cfg: FakeConfig): void {
  if (cfg.failStart === "exit") {
    process.stderr.write(`fake mcp refusing to start ${cfg.secret ?? ""}\n`);
    process.exit(3);
  }
  let buf = "";
  process.stdin.setEncoding("utf8");
  process.stdin.on("data", (c: string) => {
    buf += c;
    let i: number;
    while ((i = buf.indexOf("\n")) >= 0) {
      const line = buf.slice(0, i).trim();
      buf = buf.slice(i + 1);
      if (!line) continue;
      const m = JSON.parse(line) as Json;
      if (cfg.failStart === "hang" && m.method === "initialize") continue;
      void handle(cfg, m).then((r) => {
        if (r) process.stdout.write(`${JSON.stringify(r)}\n`);
      });
    }
  });
  process.stdin.on("end", () => process.exit(0));
  // Some real servers log to stdout; the client must ignore non-JSON lines.
  process.stdout.write("fake mcp server ready (not json)\n");
}

if (process.argv[2] === "--stdio") {
  runStdio(JSON.parse(process.argv[3] ?? "{}") as FakeConfig);
}
