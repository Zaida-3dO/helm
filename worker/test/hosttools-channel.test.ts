// Worker side: the request channel and the in-process "helm" MCP server.
import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { test } from "node:test";
import { RequestChannel, type CallResult } from "../src/hosttools/channel.ts";
import { HostToolServer, sdkMcpConfig, type HostTool, type McpBridgeLike } from "../src/hosttools/server.ts";
import type { Json } from "../src/protocol/types.ts";

function tmp(): string {
  return fs.mkdtempSync(path.join(os.tmpdir(), "helm-ht-"));
}

/** A fake host: answers each chat_request by writing response.json after `delayMs`. */
function fakeHost(dir: string, answer: (r: Json) => Json | null, delayMs = 5) {
  const lines: Json[] = [];
  const emit = (rec: Json) => {
    lines.push(rec);
    if (rec.type !== "chat_request") return;
    const body = answer(rec);
    if (!body) return;
    setTimeout(() => {
      const folder = path.join(dir, String(rec.id));
      if (fs.existsSync(folder)) fs.writeFileSync(path.join(folder, "response.json"), JSON.stringify({ id: rec.id, ...body }));
    }, delayMs);
  };
  return { lines, emit };
}

const opts = { limitBytes: 8 * 1024, timeoutMs: 20_000, mutating: false };

test("a call writes chat_request, reads response.json, then cleans up with chat_cancel", async () => {
  const dir = tmp();
  const h = fakeHost(dir, (r) => ({ ok: true, result: { echo: r.args } }));
  const ch = new RequestChannel({ dir, emit: h.emit });
  const r = await ch.call("app_list_chats", { q: 1 }, { ...opts, caller: { child: false, toolCallId: "tu1" } });
  assert.deepEqual(r, { ok: true, result: { echo: { q: 1 } } });
  assert.equal(h.lines[0].type, "chat_request");
  assert.equal(h.lines[0].operation, "app_list_chats");
  assert.deepEqual(h.lines[0].caller, { child: false, toolCallId: "tu1" });
  assert.deepEqual(h.lines[1], { type: "chat_cancel", id: h.lines[0].id });
  assert.equal(fs.readdirSync(dir).length, 0, "the request folder is removed");
});

test("oversize requests are refused before anything is written", async () => {
  const dir = tmp();
  const h = fakeHost(dir, () => ({ ok: true, result: 1 }));
  const ch = new RequestChannel({ dir, emit: h.emit });
  const fits = { s: "x".repeat(8 * 1024 - 8) };
  assert.equal(Buffer.byteLength(JSON.stringify(fits)), 8 * 1024);
  assert.equal((await ch.call("app_list_chats", fits, opts)).ok, true);
  const n = h.lines.length;
  const r = await ch.call("app_list_chats", { s: "x".repeat(8 * 1024 - 7) }, opts);
  assert.equal(r.ok, false);
  assert.match((r as { error: string }).error, /at most 8 KiB\. Nothing changed\./);
  assert.equal(h.lines.length, n, "no line written");
});

test("at most 8 calls run at once; the 9th is refused with nothing changed", async () => {
  const dir = tmp();
  const h = fakeHost(dir, () => ({ ok: true, result: "ok" }), 150);
  const ch = new RequestChannel({ dir, emit: h.emit });
  const calls = Array.from({ length: 9 }, () => ch.call("app_list_chats", {}, opts));
  const results = await Promise.all(calls);
  assert.equal(results.filter((r) => r.ok).length, 8);
  const refused = results.find((r) => !r.ok) as { error: string; changed: string };
  assert.match(refused.error, /8 host-tool calls running\. Nothing changed\./);
  assert.equal(refused.changed, "no");
  assert.equal(ch.running, 0);
});

test("a lost reply says the change may have happened for mutating tools only", async () => {
  const dir = tmp();
  const h = fakeHost(dir, () => null);
  const ch = new RequestChannel({ dir, emit: h.emit, graceMs: 0 });
  const w = (await ch.call("app_create_chat", {}, { ...opts, timeoutMs: 60, mutating: true })) as { error: string; changed: string };
  assert.equal(w.changed, "maybe");
  assert.match(w.error, /may have happened/);
  const r = (await ch.call("app_list_chats", {}, { ...opts, timeoutMs: 60 })) as { error: string; changed: string };
  assert.equal(r.changed, "no");
  assert.ok(h.lines.filter((l) => l.type === "chat_cancel").length === 2, "cancel sent on timeout");
  const ac = new AbortController();
  const p = ch.call("app_create_chat", {}, { ...opts, mutating: true, signal: ac.signal });
  setTimeout(() => ac.abort(), 20);
  assert.match(((await p) as { error: string }).error, /cancelled after it was sent.*may have happened/);
});

test("host errors pass through with what changed", async () => {
  const dir = tmp();
  const h = fakeHost(dir, () => ({ ok: false, error: "No such chat. Nothing changed.", changed: "no" }));
  const ch = new RequestChannel({ dir, emit: h.emit });
  assert.deepEqual(await ch.call("app_read_chat", {}, opts), { ok: false, error: "No such chat. Nothing changed.", changed: "no" });
});

const TOOLS: HostTool[] = [
  { name: "app_list_chats", description: "List the chats.", inputSchema: { type: "object" }, requestLimitBytes: 8192, timeoutMs: 20000, children: false, mutating: false },
  { name: "app_set_chat_archived", description: "Archive a chat.", inputSchema: { type: "object" }, requestLimitBytes: 8192, timeoutMs: 20000, children: false, mutating: true },
];

function server(answer: (r: Json) => Json | null, bridge?: McpBridgeLike, onEndTurn?: () => void) {
  const dir = tmp();
  const h = fakeHost(dir, answer);
  const s = new HostToolServer({ hostTools: TOOLS, channel: new RequestChannel({ dir, emit: h.emit }), bridge, onEndTurn });
  return { s, h };
}

const root = { child: false };
const child = { child: true, toolCallId: "tu" };

test("initialize, ping and tools/list; notifications get no answer", async () => {
  const { s } = server(() => null);
  const init = (await s.handle({ jsonrpc: "2.0", id: 1, method: "initialize", params: { protocolVersion: "2025-06-18" } }, root)) as Json;
  assert.equal((init.result as Json).serverInfo && ((init.result as Json).serverInfo as Json).name, "helm");
  assert.deepEqual(((await s.handle({ jsonrpc: "2.0", id: 2, method: "ping" }, root)) as Json).result, {});
  assert.equal(await s.handle({ jsonrpc: "2.0", method: "notifications/initialized" }, root), null);
  const list = (await s.handle({ jsonrpc: "2.0", id: 3, method: "tools/list" }, root)) as Json;
  const names = ((list.result as Json).tools as Json[]).map((t) => t.name);
  assert.deepEqual(names, ["app_list_chats", "app_set_chat_archived", "app_list_tools", "app_mcp_call"], "app_mcp_tools only with an MCP client");
  assert.equal(((await s.handle({ jsonrpc: "2.0", id: 4, method: "bogus" }, root)) as Json).error && true, true);
  assert.deepEqual(JSON.parse(sdkMcpConfig()), { mcpServers: { helm: { type: "sdk", name: "helm" } } });
});

test("tools/call routes host tools through the channel with the caller binding", async () => {
  const { s, h } = server(() => ({ ok: true, result: { chats: [] } }));
  const r = (await s.handle({ jsonrpc: "2.0", id: 1, method: "tools/call", params: { name: "app_list_chats", arguments: {} } }, { child: false, toolCallId: "tu9" })) as Json;
  const res = r.result as Json;
  assert.deepEqual(res.structuredContent, { chats: [] });
  assert.equal(res.isError, undefined);
  assert.deepEqual(h.lines[0].caller, { child: false, toolCallId: "tu9" });
});

test("app_mcp_tools is listed before any server is configured, and says when none is", async () => {
  // The host sends the server list only after Claude Code has read this server's tool list.
  const bridge: McpBridgeLike = { enabled: () => false, tools: async () => ({}), has: () => false, call: async () => ({ ok: true, result: "" }) };
  const { s } = server(() => null, bridge);
  assert.ok(s.catalogue().some((t) => t.name === "app_mcp_tools"));
  const r = await s.call("app_mcp_tools", {}, root);
  assert.match((r as { error: string }).error, /No MCP server is enabled/);
});

test("children reach only MCP tools; app_mcp_call never reaches a host tool for a child", async () => {
  const calls: string[] = [];
  const bridge: McpBridgeLike = {
    enabled: () => true,
    tools: async () => ({ tools: [{ name: "notes__search" }] }),
    has: (n) => n === "notes__search",
    call: async (n): Promise<CallResult> => {
      calls.push(n);
      return { ok: true, result: "found" };
    },
  };
  const { s, h } = server(() => ({ ok: true, result: "host" }), bridge);
  assert.ok(s.catalogue().some((t) => t.name === "app_mcp_tools"));
  const viaCall = await s.call("app_mcp_call", { name: "app_list_chats", arguments: {} }, child);
  assert.match((viaCall as { error: string }).error, /only available to the parent chat/);
  assert.equal((await s.call("app_list_tools", {}, child)).ok, false);
  assert.deepEqual(await s.call("app_mcp_call", { name: "notes__search", arguments: {} }, child), { ok: true, result: "found" });
  assert.equal((await s.call("app_mcp_tools", {}, child)).ok, true);
  assert.equal((await s.call("app_mcp_call", { name: "app_mcp_call" }, root)).ok, false, "no recursive dispatch");
  assert.deepEqual(await s.call("app_mcp_call", { name: "app_list_chats", arguments: {} }, root), { ok: true, result: "host" });
  // A child calling a host tool directly is sent with child: true; the host gate refuses it.
  await s.call("app_list_chats", {}, child);
  assert.equal(h.lines.filter((l) => l.type === "chat_request").at(-1)?.caller && (h.lines.filter((l) => l.type === "chat_request").at(-1)?.caller as Json).child, true);
  assert.deepEqual(calls, ["notes__search"]);
});

test("app_list_tools searches 10 per page", async () => {
  const many: HostTool[] = Array.from({ length: 12 }, (_, i) => ({ ...TOOLS[0], name: `app_tool_${String(i).padStart(2, "0")}`, description: "A chat tool." }));
  const dir = tmp();
  const s = new HostToolServer({ hostTools: many, channel: new RequestChannel({ dir, emit: () => {} }) });
  const p1 = (await s.call("app_list_tools", { query: "chat" }, root)) as { ok: true; result: Json };
  assert.equal((p1.result.tools as Json[]).length, 10);
  assert.equal(p1.result.nextOffset, 10);
  const p2 = (await s.call("app_list_tools", { query: "chat", offset: 10 }, root)) as { ok: true; result: Json };
  assert.equal((p2.result.tools as Json[]).length, 2);
  assert.equal(p2.result.nextOffset, undefined);
});

test("a self-archive with archivePending ends the turn", async () => {
  let ended = 0;
  const { s } = server(() => ({ ok: true, result: { archivePending: true } }), undefined, () => ended++);
  await s.call("app_set_chat_archived", { chatId: "x", archived: true }, root);
  assert.equal(ended, 1);
});

test("attribution: hook records match calls by tool and input; unmatched calls count as a child's", async () => {
  const { Attribution } = await import("../src/hosttools/attribution.ts");
  const a = new Attribution("mcp__helm__");
  a.noteHook({ tool_name: "mcp__helm__app_list_chats", tool_use_id: "tu1", tool_input: { b: 1, a: 2 } });
  a.noteHook({ tool_name: "mcp__helm__app_list_chats", tool_use_id: "tu2", agent_id: "sub", tool_input: {} });
  a.noteHook({ tool_name: "Bash", tool_use_id: "tu3", tool_input: {} });
  assert.deepEqual(await a.take("app_list_chats", { a: 2, b: 1 }), { child: false, toolCallId: "tu1" }, "key order does not matter");
  assert.deepEqual(await a.take("app_list_chats", {}), { child: true, toolCallId: "tu2" });
  // A call whose hook arrives a little later still matches.
  const later = a.take("app_read_chat", { chatId: "x" }, 500);
  a.noteHook({ tool_name: "mcp__helm__app_read_chat", tool_use_id: "tu4", tool_input: { chatId: "x" } });
  assert.deepEqual(await later, { child: false, toolCallId: "tu4" });
  // No record in time: treated as a child and marked unattributed (fails closed).
  assert.deepEqual(await a.take("app_read_chat", { chatId: "y" }, 30), { child: true, unattributed: true });
});

test("attribution: a parent's leftover hook record cannot be borrowed by a later child call", async () => {
  const { Attribution, RECORD_TTL_MS } = await import("../src/hosttools/attribution.ts");
  let now = 1_000;
  const a = new Attribution("mcp__helm__", () => now);
  // The parent's hook fired but its call never came.
  a.noteHook({ tool_name: "mcp__helm__app_send_message", tool_use_id: "p1", tool_input: { chatId: "x" } });
  // A child's identical call arrives with its own record: the records disagree, so the narrower wins.
  a.noteHook({ tool_name: "mcp__helm__app_send_message", tool_use_id: "c1", agent_id: "sub", tool_input: { chatId: "x" } });
  assert.deepEqual(await a.take("app_send_message", { chatId: "x" }), { child: true, toolCallId: "c1" }, "the child's record, with its own id");
  // An old record expires.
  a.noteHook({ tool_name: "mcp__helm__app_send_message", tool_use_id: "p2", tool_input: { chatId: "y" } });
  now += RECORD_TTL_MS + 1;
  assert.deepEqual(await a.take("app_send_message", { chatId: "y" }, 20), { child: true, unattributed: true });
  // Turn end clears unclaimed records.
  a.noteHook({ tool_name: "mcp__helm__app_send_message", tool_use_id: "p3", tool_input: { chatId: "z" } });
  a.clear();
  assert.equal((await a.take("app_send_message", { chatId: "z" }, 20)).unattributed, true);
});

test("an unattributed call never reaches the host, but may still use MCP tools", async () => {
  const bridge: McpBridgeLike = { enabled: () => true, tools: async () => ({}), has: (n) => n === "m__t", call: async () => ({ ok: true, result: "mcp" }) };
  const { s, h } = server(() => ({ ok: true, result: "host" }), bridge);
  const r = await s.call("app_list_chats", {}, { child: true, unattributed: true });
  assert.match((r as { error: string }).error, /could not tell which agent.*Nothing changed/);
  assert.equal(h.lines.length, 0, "nothing sent");
  assert.deepEqual(await s.call("app_mcp_call", { name: "m__t", arguments: {} }, { child: true, unattributed: true }), { ok: true, result: "mcp" });
});

test("tool identity: Helm's tools, MCP tools through app_mcp_call, native MCP and native tools", async () => {
  const { toolSource } = await import("../src/hosttools/source.ts");
  assert.deepEqual(toolSource("mcp__helm__app_list_chats", {}), { kind: "host", server: "Helm", tool: "app_list_chats" });
  assert.deepEqual(toolSource("mcp__helm__app_mcp_call", { name: "app_read_chat" }), { kind: "host", server: "Helm", tool: "app_read_chat" });
  assert.deepEqual(toolSource("mcp__helm__app_mcp_call", { name: "notes__search" }), { kind: "mcp", server: "notes", tool: "search" });
  assert.deepEqual(toolSource("mcp__agent-standup__get_item", {}), { kind: "mcp", server: "agent-standup", tool: "get_item" });
  assert.deepEqual(toolSource("Bash", {}), { kind: "native", server: "Claude Code", tool: "Bash" });
});
