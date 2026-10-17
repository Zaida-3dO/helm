// The worker's MCP client against the fake server: discovery, calls, limits, output shaping,
// timeouts and config changes.

import assert from "node:assert/strict";
import { after, test } from "node:test";
import { startHttpFake } from "../../fakes/mcp-server/server.ts";
import { generatedName } from "../src/mcp/index.ts";
import { FIXED_TOOLS, harness, httpServer, stdioServer, text } from "./mcp-helpers.ts";

const open: { close(): void }[] = [];
after(() => open.forEach((b) => b.close()));

test("stdio: lazy connect, discovery caps at 20, calls by generated name", async () => {
  const h = harness();
  open.push(h.bridge);
  h.bridge.configure({ revision: "r1", servers: [stdioServer("fake", { tools: 30 })] });
  assert.equal(h.last("fake")?.state, "idle", "nothing connects before the first use");
  const d = await h.bridge.tools({ server: "fake" });
  assert.equal(d.total, FIXED_TOOLS + 30);
  assert.equal(d.loaded.length, 20);
  assert.equal(d.more, true);
  assert.equal(h.last("fake")?.state, "connected");
  assert.equal(h.last("fake")?.toolCount, FIXED_TOOLS + 30);
  assert.ok(!d.loaded.some((t) => t.name.includes("bad_schema")), "a non-object schema is skipped");
  const echo = generatedName("fake", "echo");
  assert.match(echo, /^mcp_fake_echo_[0-9a-f]{12}$/);
  const r = await h.bridge.call(echo, { text: "hi" });
  assert.equal(text(r), '{"text":"hi"}');
  assert.deepEqual(r.source, { kind: "mcp", server: "Fake fake", tool: "echo" });
});

test("search: terms fold case and separators; a separators-only query matches nothing", async () => {
  const h = harness();
  open.push(h.bridge);
  h.bridge.configure({ revision: "r1", servers: [stdioServer("s", {})] });
  const a = await h.bridge.tools({ server: "s", query: "SECRET-echo" });
  assert.deepEqual(a.loaded.map((t) => t.label), ["Fake s · secret_echo"]);
  const b = await h.bridge.tools({ server: "s", query: "mcp_s_structured" });
  assert.deepEqual(b.loaded.map((t) => t.label), ["Fake s · structured"]);
  const c = await h.bridge.tools({ server: "s", query: "-- __" });
  assert.equal(c.matches, 0);
});

test("a preset's guidance comes back with discovery", async () => {
  const h = harness();
  open.push(h.bridge);
  h.bridge.configure({ revision: "r1", servers: [stdioServer("g", {}, { guidance: "Dry runs only." })] });
  const d = await h.bridge.tools({ server: "g", query: "echo" });
  assert.equal(d.guidance, "Dry runs only.");
});

test("a tool must be discovered before it can be called", async () => {
  const h = harness();
  open.push(h.bridge);
  h.bridge.configure({ revision: "r1", servers: [stdioServer("s", {})] });
  await assert.rejects(h.bridge.call(generatedName("s", "echo"), {}), /app_mcp_tools first/);
});

test("pages of tools/list are followed", async () => {
  const h = harness();
  open.push(h.bridge);
  h.bridge.configure({ revision: "r1", servers: [stdioServer("p", { tools: 7, pageSize: 3 })] });
  const d = await h.bridge.tools({ server: "p" });
  assert.equal(d.total, FIXED_TOOLS + 7);
});

test("output: line and byte caps, images, structured content and isError", async () => {
  const h = harness();
  open.push(h.bridge);
  h.bridge.configure({ revision: "r1", servers: [stdioServer("o", {})] });
  await h.bridge.tools({ server: "o" });
  const big = generatedName("o", "big");
  const lines = text(await h.bridge.call(big, { lines: 3000 }));
  assert.ok(lines.split("\n").length <= 2001, "≤ 2,000 lines plus the note");
  assert.match(lines, /output truncated/);
  const bytes = text(await h.bridge.call(big, { bytes: 60_000 }));
  assert.ok(Buffer.byteLength(bytes) < 50_100);
  assert.match(bytes, /output truncated/);
  const small = text(await h.bridge.call(big, { bytes: 100 }));
  assert.doesNotMatch(small, /truncated/);
  const img = await h.bridge.call(generatedName("o", "image"), { count: 5 });
  assert.equal(img.content.filter((c) => c.type === "image").length, 4);
  assert.match(text(img), /2 image\(s\) omitted/);
  const st = text(await h.bridge.call(generatedName("o", "structured"), {}));
  assert.equal(st, '{"a":1}', "structured content identical to a text block is not repeated");
  const err = await h.bridge.call(generatedName("o", "tool_error"), {});
  assert.equal(err.isError, true);
});

test("http (JSON replies): session id and protocol version are sent back", async () => {
  const f = await startHttpFake({});
  const h = harness();
  open.push(h.bridge, { close: () => void f.close() });
  h.bridge.configure({ revision: "r1", servers: [httpServer("web", f.url)] });
  await h.bridge.tools({ server: "web" });
  const r = await h.bridge.call(generatedName("web", "echo"), { text: "x" });
  assert.equal(text(r), '{"text":"x"}');
  const later = f.requests.slice(1);
  assert.ok(later.length >= 2);
  for (const q of later) {
    assert.equal(q.headers["mcp-session-id"], "fake-session-1");
    assert.equal(q.headers["mcp-protocol-version"], "2025-06-18");
  }
  assert.equal(f.requests[0]!.headers["mcp-session-id"], undefined);
});

test("http (SSE replies): notifications are skipped and the reply is matched", async () => {
  const f = await startHttpFake({ sse: true });
  const h = harness();
  open.push(h.bridge, { close: () => void f.close() });
  h.bridge.configure({ revision: "r1", servers: [httpServer("sse", f.url)] });
  await h.bridge.tools({ server: "sse" });
  const r = await h.bridge.call(generatedName("sse", "echo"), { text: "y" });
  assert.equal(text(r), '{"text":"y"}');
});

test("startup timeout fails the server, and the next use tries again", async () => {
  const h = harness({ limits: { startupMs: 300 } });
  open.push(h.bridge);
  h.bridge.configure({ revision: "r1", servers: [stdioServer("hang", { failStart: "hang" })] });
  await assert.rejects(h.bridge.tools({ server: "hang" }), /did not start within/);
  assert.equal(h.last("hang")?.state, "failed");
  await assert.rejects(h.bridge.tools({ server: "hang" }), /did not start within/);
  assert.equal(h.lines.filter((l) => l.serverId === "hang" && l.state === "connecting").length, 2);
});

test("a server that exits at start reports why", async () => {
  const h = harness();
  open.push(h.bridge);
  h.bridge.configure({ revision: "r1", servers: [stdioServer("dead", { failStart: "exit" })] });
  await assert.rejects(h.bridge.tools({ server: "dead" }), /exited/);
  assert.match(String(h.last("dead")?.error), /exited/);
});

test("a call that times out says it may have happened", async () => {
  const h = harness({ limits: { callMs: 200 } });
  open.push(h.bridge);
  h.bridge.configure({ revision: "r1", servers: [stdioServer("slow", { slowMs: 2000 })] });
  await h.bridge.tools({ server: "slow" });
  await assert.rejects(h.bridge.call(generatedName("slow", "slow"), {}), (e: Error & { changed?: string }) => {
    assert.match(e.message, /may have happened/);
    assert.equal(e.changed, "maybe");
    return true;
  });
});

test("changing a server's definition drops its loaded tools; an unchanged one keeps them", async () => {
  const h = harness();
  open.push(h.bridge);
  const a = stdioServer("a", {});
  const b = stdioServer("b", {});
  h.bridge.configure({ revision: "r1", servers: [a, b] });
  await h.bridge.tools({ server: "a", query: "echo" });
  await h.bridge.tools({ server: "b", query: "echo" });
  h.bridge.configure({ revision: "r2", servers: [{ ...a, name: "Renamed" }, b] });
  assert.equal(h.bridge.isLoaded(generatedName("a", "echo")), false);
  assert.equal(h.bridge.isLoaded(generatedName("b", "echo")), true);
  await assert.rejects(h.bridge.call(generatedName("a", "echo"), {}), /app_mcp_tools/);
  h.bridge.configure({ revision: "r3", servers: [b] });
  await assert.rejects(h.bridge.tools({ server: "a" }), /No enabled MCP server/);
});

test("at most maxLoaded tools are loaded per session", async () => {
  const h = harness({ limits: { maxLoaded: 5 } });
  open.push(h.bridge);
  h.bridge.configure({ revision: "r1", servers: [stdioServer("cap", { tools: 10 })] });
  const d1 = await h.bridge.tools({ server: "cap" });
  assert.equal(d1.loaded.length, 5);
  const d2 = await h.bridge.tools({ server: "cap", query: "tool 9" });
  assert.equal(d2.loaded.length, 0);
  assert.equal(d2.more, true);
});

test("a reference the host could not resolve fails the server without connecting", async () => {
  const h = harness();
  open.push(h.bridge);
  h.bridge.configure({ revision: "r1", servers: [stdioServer("e", {}, { error: "Environment TOKEN: the variable X is not set." })] });
  assert.equal(h.last("e")?.state, "failed");
  await assert.rejects(h.bridge.tools({ server: "e" }), /not set/);
  assert.equal(h.lines.filter((l) => l.state === "connecting").length, 0);
});

test("stdio servers never inherit the Anthropic credentials", async () => {
  const h = harness({ env: { ...process.env, ANTHROPIC_API_KEY: "should-not-pass", HELM_FAKE_VISIBLE: "yes" } });
  open.push(h.bridge);
  h.bridge.configure({ revision: "r1", servers: [stdioServer("env", {}, { env: { FROM_CONFIG: "cfg" } })] });
  await h.bridge.tools({ server: "env", query: "env" });
  const name = generatedName("env", "env");
  assert.equal(text(await h.bridge.call(name, { name: "ANTHROPIC_API_KEY" })), "unset");
  assert.equal(text(await h.bridge.call(name, { name: "HELM_FAKE_VISIBLE" })), "set:yes");
  assert.equal(text(await h.bridge.call(name, { name: "FROM_CONFIG" })), "set:cfg");
});
