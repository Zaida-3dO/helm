// The worker's MCP client against the fake server: discovery, calls, limits, output shaping,
// timeouts and config changes.

import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { after, test } from "node:test";
import { startHttpFake } from "../../fakes/mcp-server/server.ts";
import { generatedName } from "../src/mcp/index.ts";
import { McpSession, type Transport } from "../src/mcp/session.ts";
import { LIMITS } from "../src/mcp/types.ts";
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

test("browser sign-in: one ask for a fresh header; a refusal or no sign-in becomes signin_required", async () => {
  const f = await startHttpFake({ requireHeader: { name: "authorization", value: "Bearer fresh-1" } });
  const asks: string[] = [];
  // The host answers the ask with "delivered" and sends the header on the worker's own line:
  // for "ok" before the answer (the line can beat it), for "stale" just after.
  const h: ReturnType<typeof harness> = harness({
    hostRequest: async (op, args) => {
      const id = String((args as { serverId?: unknown }).serverId);
      asks.push(id);
      assert.equal(op, "app_mcp_oauth_header");
      if (id === "ok") {
        h.bridge.deliverOauthHeader(id, "Bearer fresh-1");
        return { ok: true, result: { delivered: true } };
      }
      if (id === "stale") {
        setTimeout(() => h.bridge.deliverOauthHeader(id, "Bearer stale-9"), 5);
        return { ok: true, result: { delivered: true } };
      }
      return { ok: false, error: "Not connected: open Settings → MCP and click Connect." };
    },
  });
  open.push(h.bridge, { close: () => void f.close() });
  h.bridge.configure({
    revision: "r1",
    servers: [
      httpServer("none", f.url, { oauth: true }),
      httpServer("ok", f.url, { oauth: true }),
      httpServer("stale", f.url, { oauth: true }),
      httpServer("off", f.url, { oauth: true, error: "Not connected: open Settings → MCP and click Connect." }),
    ],
  });
  // A browser-signed-in server the host could not give a token to asks the user, not a failure.
  assert.equal(h.last("off")?.state, "signin_required");
  // The host has a current header: one refusal, one ask, the retry carries it and works.
  await h.bridge.tools({ server: "ok" });
  assert.deepEqual(asks, ["ok"], "asked once, on the first refusal");
  assert.equal(h.last("ok")?.state, "connected");
  const sent = f.requests.map((q) => q.headers.authorization ?? "(none)");
  assert.ok(sent.includes("(none)") && sent.includes("Bearer fresh-1"));
  // No token material in any status line.
  assert.ok(!JSON.stringify(h.lines).includes("fresh-1") && !JSON.stringify(h.lines).includes("stale-9"));
  // The host says there is no sign-in: no retry, and the server asks the user.
  await assert.rejects(() => h.bridge.tools({ server: "none" }), /Sign in again in Settings/);
  assert.equal(h.last("none")?.state, "signin_required");
  // A fresh header the server still refuses: one retry only, then the same ask to the user.
  await assert.rejects(() => h.bridge.tools({ server: "stale" }), /Sign in again in Settings/);
  assert.equal(h.last("stale")?.state, "signin_required");
  assert.deepEqual(asks, ["ok", "none", "stale"], "one ask per server, never in a loop");
  // A header line nobody asked for is dropped, not kept.
  assert.equal(h.bridge.deliverOauthHeader("ok", "Bearer unasked"), false);
});

test("browser sign-in: every expiry is asked about, and refusals that land together share one ask", async () => {
  const cfg = { requireHeader: { name: "authorization", value: "Bearer t1" } };
  const f = await startHttpFake(cfg);
  const asks: { serverId: string; refused?: string }[] = [];
  let next = 2;
  const h: ReturnType<typeof harness> = harness({
    hostRequest: async (_op, args) => {
      const a = args as { serverId: string; refused?: string };
      asks.push(a);
      const header = `Bearer t${next++}`;
      // The server learns of the new token at the same moment the host hands it over.
      cfg.requireHeader.value = header;
      setTimeout(() => h.bridge.deliverOauthHeader(a.serverId, header), 20);
      return { ok: true, result: { delivered: true } };
    },
  });
  open.push(h.bridge, { close: () => void f.close() });
  h.bridge.configure({ revision: "r1", servers: [httpServer("ex", f.url, { oauth: true, headers: { Authorization: "Bearer t1" } })] });
  await h.bridge.tools({ server: "ex" });
  const print = (t: string) => createHash("sha256").update(t).digest("hex").slice(0, 8);
  // Two expiries one after the other, each recovered: no "sign in again" in between.
  for (const [i, t] of [[1, "t2"], [2, "t3"]] as const) {
    cfg.requireHeader.value = `Bearer expired-${i}`;
    next = Number(t.slice(1));
    await h.bridge.tools({ server: "ex" });
    assert.equal(asks.length, i);
    assert.equal(asks[i - 1]!.refused, print(`t${i}`), "the host is told which token was refused, by fingerprint only");
  }
  // Two requests refused together: one ask answers both.
  cfg.requireHeader.value = "Bearer expired-3";
  next = 4;
  await Promise.all([h.bridge.tools({ server: "ex" }), h.bridge.tools({ server: "ex" })]);
  assert.equal(asks.length, 3);
  assert.ok(!h.lines.some((l) => l.state === "signin_required"), "no sign-in prompt for any of it");
  assert.ok(!JSON.stringify(h.lines).includes("Bearer t4"));
});

test("a host-served server is skipped quietly: no status, no tools, and it retires without a report", async () => {
  const f = await startHttpFake({});
  const h = harness();
  open.push(h.bridge, { close: () => void f.close() });
  h.bridge.configure({
    revision: "r1",
    servers: [httpServer("live", f.url), stdioServer("gone"), { id: "built_in", name: "Built in", transport: "stdio", command: "uvx", hostServed: true }],
  });
  await h.bridge.tools({ server: "live" });
  await h.bridge.tools({ server: "gone" });
  assert.ok(h.last("live"), "the live server reports normally");
  assert.equal(h.last("built_in"), undefined, "a host-served server never gets a status row");
  assert.ok(!JSON.stringify(h.lines).includes("built_in"), "no line at all names it");
  await assert.rejects(h.bridge.tools({ server: "built_in" }), /No enabled MCP server/);
  // An entry that turns host-served on the next edit retires quietly: no failure, no new row.
  const goneBefore = h.lines.filter((l) => l.serverId === "gone").length;
  h.bridge.configure({ revision: "r2", servers: [httpServer("live", f.url), { id: "gone", name: "Fake gone", transport: "stdio", command: "node", hostServed: true }] });
  assert.equal(h.lines.filter((l) => l.serverId === "gone").length, goneBefore, "retiring adds nothing to its status");
  await assert.rejects(h.bridge.tools({ server: "gone" }), /No enabled MCP server/);
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

test("a request that times out while its send is still pending fails the call and not the worker", async () => {
  const stray: unknown[] = [];
  const onStray = (e: unknown) => stray.push(e);
  process.on("unhandledRejection", onStray);
  try {
    const hung: Transport = {
      // Like an HTTP post whose reply never comes: it only ends when the request is aborted.
      send: (_msg, signal) =>
        new Promise<void>((_ok, fail) => signal?.addEventListener("abort", () => setTimeout(() => fail(new Error("aborted")), 20), { once: true })),
      onMessage: () => {},
      onClose: () => {},
      close: () => {},
    };
    const session = new McpSession(hung, { ...LIMITS, pageMs: 50 }, "Slow server");
    await assert.rejects(session.listTools(), /Slow server did not answer tools\/list/);
    await new Promise((r) => setTimeout(r, 100));
    assert.deepEqual(stray, [], "no unhandled rejection is left behind");
  } finally {
    process.off("unhandledRejection", onStray);
  }
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
  const h = harness({ env: { ...process.env, ANTHROPIC_API_KEY: "should-not-pass", HELM_CLAUDE_TOKEN: "should-not-pass", HELM_FAKE_VISIBLE: "yes" } });
  open.push(h.bridge);
  h.bridge.configure({ revision: "r1", servers: [stdioServer("env", {}, { env: { FROM_CONFIG: "cfg" } })] });
  await h.bridge.tools({ server: "env", query: "env" });
  const name = generatedName("env", "env");
  assert.equal(text(await h.bridge.call(name, { name: "ANTHROPIC_API_KEY" })), "unset");
  assert.equal(text(await h.bridge.call(name, { name: "HELM_CLAUDE_TOKEN" })), "unset", "the saved Claude token's hand-over name stays out too");
  assert.equal(text(await h.bridge.call(name, { name: "HELM_FAKE_VISIBLE" })), "set:yes");
  assert.equal(text(await h.bridge.call(name, { name: "FROM_CONFIG" })), "set:cfg");
});
