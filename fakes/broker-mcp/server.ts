// Fake browser broker over MCP stdio: the eleven browser tools, scripted by a config object.
// It answers just enough for the worker's browser-hold checks:
//
//   node server.ts --stdio '<config json>'
//
// Config: {
//   act: "needs_owner" | "ok"   the browser_act answer (default: needs_owner, like an attached
//                               browser refusing a consequential step),
//   statuses: "out path"        append each browser_status line to this file (keep-alive counts),
//   secret: "..."               echoed into one tool's text, for the redaction checks.
// }

import fs from "node:fs";

type Json = Record<string, unknown>;

interface Cfg {
  act?: "needs_owner" | "ok";
  statuses?: string;
  secret?: string;
}

const cfg: Cfg = JSON.parse(process.argv[3] ?? "{}");
const TOOLS = [
  "browser_claim",
  "browser_status",
  "browser_release",
  "browser_tab_replace",
  "browser_navigate",
  "browser_act",
  "browser_read",
  "browser_evaluate",
  "browser_capture",
  "browser_sign_in",
  "browser_doctor",
];

const schema = {
  type: "object",
  properties: {
    lease_key: { type: "string" },
    url: { type: "string" },
    action: { type: "string" },
    what: { type: "string" },
    session_id: { type: "string" },
    purpose: { type: "string" },
  },
};

function callTool(name: string, args: Json): { result?: Json; error?: Json } {
  const text = (s: string) => ({ content: [{ type: "text", text: s }] });
  switch (name) {
    case "browser_claim":
      return { result: text("lease L1 is active: one tab of your browser") };
    case "browser_status": {
      const n = (callTool.counts[name] = (callTool.counts[name] ?? 0) + 1);
      const line = `statuses ${n}: lease ${String(args.lease_key ?? "")} active`;
      if (cfg.statuses) fs.appendFileSync(cfg.statuses, `${line}\n`);
      return { result: text(line) };
    }
    case "browser_release":
      return { result: text("released") };
    case "browser_navigate":
      return { result: text(`at ${String(args.url ?? "")}${cfg.secret ? ` (${cfg.secret})` : ""}`) };
    case "browser_act":
      if ((cfg.act ?? "needs_owner") === "ok") return { result: text("clicked") };
      return {
        result: {
          content: [{ type: "text", text: "refused (attached.needs_owner): this step is the owner's to do in their own browser — ask them with browser_sign_in, do not work around it" }],
          isError: true,
        },
      };
    case "browser_sign_in":
      if (args.action === "done") return { result: text("the browser is yours again; your tab kept") };
      return {
        result: {
          content: [{ type: "text", text: JSON.stringify({ relay: "the shop wants you to sign in; do it in your browser, then say done", wait_seconds: 600 }) }],
        },
      };
    case "browser_doctor":
      return { result: text(JSON.stringify({ checks: [{ id: "attached.token", title: "The token is set", status: "ok", detail: "set" }], exitCode: 0 })) };
    default:
      return { result: text(`ran ${name}`) };
  }
}
callTool.counts = {} as Record<string, number>;

function handle(m: Json): Json | undefined {
  if (m.id === undefined || m.id === null) return undefined;
  const reply = (body: Json) => ({ jsonrpc: "2.0", id: m.id, ...body });
  const params = (m.params ?? {}) as Json;
  switch (m.method) {
    case "initialize":
      return reply({ result: { protocolVersion: params.protocolVersion ?? "2025-06-18", capabilities: { tools: {} }, serverInfo: { name: "fake-broker", version: "1" } } });
    case "ping":
      return reply({ result: {} });
    case "tools/list":
      return reply({ result: { tools: TOOLS.map((name) => ({ name, description: `fake broker ${name}`, inputSchema: schema })) } });
    case "tools/call": {
      const r = callTool(String(params.name ?? ""), (params.arguments ?? {}) as Json);
      return reply(r.error ? { error: r.error } : { result: r.result });
    }
    default:
      return reply({ error: { code: -32601, message: "Method not found" } });
  }
}

if (process.argv[2] === "--stdio") {
  let buf = "";
  process.stdin.setEncoding("utf8");
  process.stdin.on("data", (c: string) => {
    buf += c;
    let i: number;
    while ((i = buf.indexOf("\n")) >= 0) {
      const line = buf.slice(0, i).trim();
      buf = buf.slice(i + 1);
      if (!line) continue;
      const r = handle(JSON.parse(line) as Json);
      if (r) process.stdout.write(`${JSON.stringify(r)}\n`);
    }
  });
  process.stdin.on("end", () => process.exit(0));
}
