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
//   leaseKey: "..."             set => browser_claim answers `accepted({outcome:"granted",
//                               claimId:"c1", key: leaseKey})` in both modes (the key survives
//                               redaction checks); unset => the answers stay as they were.
//   page: {...}                 the gate mode below: the broker's acting rules over one page.
//   acts: "out path"            append one JSON line per browser_act call (gate mode).
// }
//
// Gate mode (`page` set) answers browser_act the way an attached browser decides it: the site
// list, the password / one-time-code / card fields, the consequence words, and the owner grant
// (HMAC-SHA256 under BROKER_OWNER_GRANT_SECRET from the environment over seven escaped fields —
// lease, ref, control name, host, issued second, verb and the hex SHA-256 of the call's value
// (empty for a verb that carries none) — as `<issued>.<base64url tag>`, two minutes wide with
// five seconds of early skew, spent once per process). browser_navigate, browser_read (with
// find and inline), browser_capture and browser_claim answer in the broker's accepted JSON shape.
// browser_evaluate is gated the same way over an empty reference and name (the grant signs the
// exact expression), and browser_status answers the tab's address.

import { createHash, createHmac, timingSafeEqual } from "node:crypto";
import fs from "node:fs";
import path from "node:path";

type Json = Record<string, unknown>;

interface Control {
  role: string;
  name: string;
  /** The text the control shows when it differs from its accessible name (an icon button with
   * an aria-label, say). The real broker knows this and gates on it. */
  text?: string;
  inputType?: string;
  autocomplete?: string;
}

interface Page {
  /** The host the tab arrives at, whatever was asked for. */
  host: string;
  /** The path the tab arrives at, after redirects. */
  path?: string;
  /** Where the tab stands now, if the page moved on its own since the navigation. */
  hostNow?: string;
  pathNow?: string;
  actSites: string[];
  controls: Record<string, Control>;
  /** The page holds a password or card field (a submit over it is consequential). */
  secretField?: boolean;
  /** Where captures are written; answered as a path relative to it. */
  artifacts?: string;
}

interface Cfg {
  act?: "needs_owner" | "ok";
  statuses?: string;
  secret?: string;
  leaseKey?: string;
  page?: Page;
  acts?: string;
}

/** The claim answer: a configured lease key is handed back the way the real broker hands back
 * its minted one; without it the answers stay the plain "L1" forms. */
function claimAnswer(): Json {
  if (cfg.leaseKey === undefined) return accepted({ key: "L1" });
  return accepted({ outcome: "granted", claimId: "c1", key: cfg.leaseKey });
}

const cfg: Cfg = JSON.parse(process.argv[3] ?? "{}");

const CONSEQUENCE = ["pay", "buy", "order", "checkout", "transfer", "send", "submit", "delete", "remove", "cancel"];
const spent = new Set<string>();

const words = (s: string | undefined) => (s ?? "").toLowerCase().match(/[\p{L}\p{N}]+/gu) ?? [];
const accepted = (value: Json) => ({ content: [{ type: "text", text: JSON.stringify({ outcome: "accepted", value }, null, 2) }] });
const refused = (rule: string, message: string) => ({ content: [{ type: "text", text: `refused (${rule}): ${message}` }], isError: true });

/** The digest of a call's value as the real broker binds it: hex SHA-256, empty for no value. */
function valueDigest(value: string | undefined): string {
  if (value === undefined || value === "") return "";
  return createHash("sha256").update(value, "utf8").digest("hex");
}

function verdict(grant: string, t: { leaseKey: string; ref: string; accessibleName: string; urlHost: string; verb: string; value?: string }): string {
  const secret = process.env.BROKER_OWNER_GRANT_SECRET;
  if (!secret) return "no_secret";
  const at = grant.lastIndexOf(".");
  const issued = at > 0 ? Number(grant.slice(0, at)) : 0;
  const tag = Buffer.from(at > 0 ? grant.slice(at + 1) : grant, "base64url");
  const esc = (s: string) => s.split(String.fromCharCode(92)).join(String.fromCharCode(92, 92)).split("|").join(String.fromCharCode(92) + "|");
  const want = createHmac("sha256", secret)
    .update([esc(t.leaseKey), esc(t.ref), esc(t.accessibleName), esc(t.urlHost), String(issued), esc(t.verb), esc(valueDigest(t.value))].join("|"))
    .digest();
  if (tag.length !== 32 || !timingSafeEqual(tag, want)) return "forged";
  const age = Math.floor(Date.now() / 1000) - issued;
  if (age > 120 || age < -5) return "expired";
  if (spent.has(grant)) return "replayed";
  spent.add(grant);
  return "valid";
}

function gateAct(page: Page, args: Json): Json {
  const ref = typeof args.target === "string" ? args.target : "";
  // The real broker reads both spellings of the grant argument.
  const grant = typeof args.owner_grant === "string" ? args.owner_grant : typeof args.ownerGrant === "string" ? args.ownerGrant : undefined;
  const c = page.controls[ref];
  const log = (outcome: string) => cfg.acts && fs.appendFileSync(cfg.acts, `${JSON.stringify({ ref, action: args.action, grant: grant !== undefined, outcome })}\n`);
  const no = (rule: string, message: string) => {
    log(rule);
    return refused(rule, message);
  };
  if (!page.actSites.includes(page.host)) return no("attached.act_site_off_list", "This site is not in BROKER_ATTACHED_ACT_SITES.");
  if (!c) return no("act.ref_resolves", `No element on this page matches the reference "${ref}".`);
  const ac = (c.autocomplete ?? "").split(/\s+/);
  if (c.inputType === "password" || ac.some((t) => ["password", "current-password", "new-password", "one-time-code", "cc-number", "cc-csc", "cc-exp"].includes(t)))
    return no("attached.field_forbidden", "This control is a secret field, and an agent never types into one.");
  if (grant !== undefined) {
    const v = verdict(grant, {
      leaseKey: String(args.lease_key ?? ""),
      ref,
      accessibleName: c.name,
      urlHost: page.hostNow ?? page.host,
      verb: String(args.action ?? ""),
      value: typeof args.value === "string" ? args.value : undefined,
    });
    if (v !== "valid" && v !== "no_secret") return no("attached.grant_invalid", `The owner grant did not verify (${v}); the step is refused.`);
  }
  const seen = new Set([...words(c.name), ...words(c.role), ...words(c.text)]);
  const consequential = CONSEQUENCE.some((w) => seen.has(w)) || (!!page.secretField && c.inputType === "submit");
  if (consequential && grant === undefined) return no("attached.needs_owner", "This step is consequential, so the owner does it: call browser_sign_in with what to press.");
  log("acted");
  return accepted({ action: args.action, snapshot: "after.txt" });
}

/** An expression in the page, the way an attached browser decides it: only on a listed site, and
 * only with a grant over the lease, an empty reference and name, the host, the verb `evaluate` and
 * the digest of the exact expression. */
function gateEvaluate(page: Page, args: Json): Json {
  const expression = typeof args.expression === "string" ? args.expression : "";
  const grant = typeof args.owner_grant === "string" ? args.owner_grant : undefined;
  const log = (outcome: string) => cfg.acts && fs.appendFileSync(cfg.acts, `${JSON.stringify({ ref: "", action: "evaluate", expression, grant: grant !== undefined, outcome })}\n`);
  const no = (rule: string, message: string) => {
    log(rule);
    return refused(rule, message);
  };
  if (!page.actSites.includes(page.hostNow ?? page.host)) return no("attached.no_evaluate", "Expressions run only on a listed site.");
  if (grant === undefined) return no("attached.needs_owner", "The owner approves this exact expression first.");
  const v = verdict(grant, { leaseKey: String(args.lease_key ?? ""), ref: "", accessibleName: "", urlHost: page.hostNow ?? page.host, verb: "evaluate", value: expression });
  if (v !== "valid" && v !== "no_secret") return no("attached.grant_invalid", `The owner grant did not verify (${v}); the expression is refused.`);
  log("acted");
  return accepted({ value: "ran" });
}

function gateTool(page: Page, name: string, args: Json): Json | undefined {
  const at = `https://${page.host}${page.path ?? "/aisles"}`;
  const atNow = `https://${page.hostNow ?? page.host}${page.pathNow ?? page.path ?? "/aisles"}`;
  switch (name) {
    case "browser_claim":
      return claimAnswer();
    case "browser_navigate":
      return accepted({ url: at });
    case "browser_act":
      return gateAct(page, args);
    case "browser_evaluate":
      return gateEvaluate(page, args);
    case "browser_status":
      return accepted({ url: atNow });
    case "browser_read": {
      const find = typeof args.find === "string" ? args.find : "";
      const re = find.startsWith("/") && find.endsWith("/") ? new RegExp(find.slice(1, -1)) : null;
      const lines = Object.entries(page.controls).map(
        ([ref, c]) => `- ${c.role} ${JSON.stringify(c.name)}${c.text === undefined ? "" : ` [text=${JSON.stringify(c.text)}]`} [ref=${ref}]`,
      );
      const text = lines.filter((l) => !re || re.test(l)).join("\n");
      return accepted({ url: atNow, artifacts: ["snapshot"], inlined: [{ artifact: "snapshot", text }] });
    }
    case "browser_capture": {
      if (!page.artifacts) return refused("capture.unavailable", "no capture here");
      const rel = `cap-${String(args.selector ?? "").replace(/[^a-z0-9]/gi, "")}.png`;
      fs.writeFileSync(path.join(page.artifacts, rel), Buffer.from("89504e470d0a1a0a", "hex"));
      return accepted({ capture: { path: rel } });
    }
    default:
      return undefined;
  }
}
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
    expression: { type: "string" },
    what: { type: "string" },
    session_id: { type: "string" },
    purpose: { type: "string" },
  },
};

function callTool(name: string, args: Json): { result?: Json; error?: Json } {
  const text = (s: string) => ({ content: [{ type: "text", text: s }] });
  const gated = cfg.page ? gateTool(cfg.page, name, args) : undefined;
  if (gated) return { result: gated };
  switch (name) {
    case "browser_claim":
      if (cfg.leaseKey !== undefined) return { result: claimAnswer() };
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
