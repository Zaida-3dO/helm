// The bridge as the in-process server sees it, the mcp_config line, and the CLI's
// init report feeding the native-server de-duplication.

import assert from "node:assert/strict";
import { after, test } from "node:test";
import { applyConfigLine, asHostToolBridge, duplicateMcpRefusal, nativeFromInit } from "../src/mcp/adapter.ts";
import { generatedName } from "../src/mcp/index.ts";
import { HostToolServer } from "../src/hosttools/server.ts";
import { harness, stdioServer } from "./mcp-helpers.ts";

const open: { close(): void }[] = [];
after(() => open.forEach((b) => b.close()));

test("the helm server reaches MCP tools through the adapter, child calls included", async () => {
  const h = harness();
  open.push(h.bridge);
  assert.equal(applyConfigLine(h.bridge, { type: "mcp_config", revision: "r1", servers: [stdioServer("fake", {})] }), true);
  const channel = { call: async () => ({ ok: false as const, error: "no host here", changed: "no" as const }) };
  const server = new HostToolServer({ hostTools: [], channel: channel as never, bridge: asHostToolBridge(h.bridge) });
  assert.ok(server.catalogue().some((t) => t.name === "app_mcp_tools"), "app_mcp_tools is listed with a server enabled");
  const found = await server.call("app_mcp_tools", { query: "echo" }, { child: true });
  assert.equal(found.ok, true);
  const name = generatedName("fake", "echo");
  const r = await server.call("app_mcp_call", { name, arguments: { text: "hi" } }, { child: true });
  assert.deepEqual(r, { ok: true, result: '{"text":"hi"}' });
  const err = await server.call("app_mcp_call", { name: generatedName("fake", "fail"), arguments: {} }, { child: false });
  assert.equal(err.ok, false, "an undiscovered tool is not callable");
  await server.call("app_mcp_tools", { server: "fake", query: "tool error" }, { child: false });
  const te = await server.call("app_mcp_call", { name: generatedName("fake", "tool_error"), arguments: {} }, { child: false });
  assert.deepEqual(te, { ok: false, error: "tool says no", changed: "maybe" });
});

test("app_mcp_tools without a server searches all of them", async () => {
  const h = harness();
  open.push(h.bridge);
  applyConfigLine(h.bridge, { type: "mcp_config", revision: "r1", servers: [stdioServer("a", {}), stdioServer("b", { failStart: "exit" })] });
  const r = (await asHostToolBridge(h.bridge).tools({ query: "echo" })) as { servers: { server: string; error?: string }[] };
  assert.deepEqual(r.servers.map((s) => s.server), ["a", "b"]);
  assert.match(String(r.servers[1]!.error), /exited/);
});

test("a malformed mcp_config line changes nothing", () => {
  const h = harness();
  open.push(h.bridge);
  assert.equal(applyConfigLine(h.bridge, { type: "mcp_config", servers: [] }), false);
  assert.equal(applyConfigLine(h.bridge, { type: "other", revision: "r", servers: [] }), false);
  assert.equal(h.lines.length, 0);
  assert.equal(asHostToolBridge(h.bridge).enabled(), false);
});

test("the CLI's init report becomes the native list; Helm's own and failed servers are left out", () => {
  const init = {
    type: "system",
    subtype: "init",
    mcp_servers: [
      { name: "helm", status: "connected" },
      { name: "agent-standup", status: "connected" },
      { name: "broken", status: "failed" },
      { name: "claude.ai Gmail", status: "connected" },
    ],
  };
  assert.deepEqual(nativeFromInit(init), [{ name: "agent-standup" }, { name: "claude.ai Gmail" }]);
  assert.deepEqual(nativeFromInit({ type: "system", subtype: "other" }), []);
});

test("a call to Claude Code's copy of a Helm server is refused and pointed at Helm's copy", () => {
  const bridge = { shadowing: (n: string) => (n === "agent-standup" ? { id: "agent_standup", name: "Agent Standup" } : undefined) };
  const why = duplicateMcpRefusal("mcp__agent-standup__list_items", bridge);
  assert.ok(why?.includes(`app_mcp_tools (server "agent_standup")`), why);
  assert.equal(duplicateMcpRefusal("mcp__github__search", bridge), undefined, "a server only Claude Code has");
  assert.equal(duplicateMcpRefusal("mcp__helm__app_mcp_call", { shadowing: () => ({ id: "x", name: "x" }) }), undefined, "Helm's own tools");
  assert.equal(duplicateMcpRefusal("Bash", bridge), undefined);
  assert.equal(duplicateMcpRefusal("mcp__agent-standup__list_items", undefined), undefined, "no MCP client");
  assert.equal(duplicateMcpRefusal("mcp__agent-standup__list_items", bridge, true), undefined, "a sub-agent has no Helm tools to be redirected to");
});
