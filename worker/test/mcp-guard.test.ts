// The connect-time address rule, the redirect rule, and the native-server de-duplication in the
// bridge.

import assert from "node:assert/strict";
import { after, test } from "node:test";
import { startHttpFake } from "../../fakes/mcp-server/server.ts";
import { classifyAddress, connectRule, deniedNative, generatedName, normName, normUrl } from "../src/mcp/index.ts";
import { harness, httpServer, stdioServer } from "./mcp-helpers.ts";

const open: { close(): void }[] = [];
after(() => open.forEach((b) => b.close()));

test("address classes", () => {
  for (const a of ["127.0.0.1", "127.9.9.9", "::1", "::ffff:127.0.0.1"]) assert.equal(classifyAddress(a), "loopback", a);
  for (const a of ["10.0.0.1", "172.16.0.1", "172.31.0.1", "192.168.0.1", "100.64.0.1", "100.127.1.1", "fd7a:115c:a1e0::1", "fc00::1", "::ffff:10.0.0.1"]) {
    assert.equal(classifyAddress(a), "private", a);
  }
  for (const a of ["8.8.8.8", "172.32.0.1", "100.128.0.1", "2001:db8::1", "::ffff:8.8.8.8", "fe80::1", "::", "0.0.0.0", "169.254.1.1", "nonsense"]) {
    assert.equal(classifyAddress(a), "public", a);
  }
});

test("connectRule: https anywhere, http loopback, private only with the opt-in", () => {
  assert.equal(connectRule(new URL("https://example.com/mcp"), false, "x"), undefined);
  assert.equal(connectRule(new URL("http://127.0.0.1:1/mcp"), false, "x"), undefined);
  assert.throws(() => connectRule(new URL("http://10.0.0.5/mcp"), false, "x"), /Allow unencrypted/);
  assert.equal(connectRule(new URL("http://10.0.0.5/mcp"), true, "x"), undefined);
  assert.throws(() => connectRule(new URL("http://8.8.8.8/mcp"), true, "x"), /private-network/);
  assert.throws(() => connectRule(new URL("http://hub.example/mcp"), false, "x"), /Allow unencrypted/);
  assert.equal(typeof connectRule(new URL("http://hub.example/mcp"), true, "x"), "function", "a name is checked at connect time");
  assert.equal(typeof connectRule(new URL("http://localhost:1/mcp"), false, "x"), "function");
});

test("a name that resolves to a public address is refused at connect time", async () => {
  const f = await startHttpFake({});
  const port = new URL(f.url).port;
  open.push({ close: () => void f.close() });
  const resolver = async (host: string) =>
    host === "hub.test" ? [{ address: "127.0.0.1", family: 4 }] : [{ address: "93.184.216.34", family: 4 }];
  const h = harness({ resolver });
  open.push(h.bridge);
  h.bridge.configure({
    revision: "r1",
    servers: [
      httpServer("good", `http://hub.test:${port}/mcp`, { allowPrivateHttp: true }),
      httpServer("evil", `http://evil.test:${port}/mcp`, { allowPrivateHttp: true }),
    ],
  });
  const d = await h.bridge.tools({ server: "good", query: "echo" });
  assert.ok(d.loaded.length >= 1, "the name resolved to loopback through the hook and connected");
  await assert.rejects(h.bridge.tools({ server: "evil" }), /outside this computer or the private network/);
  assert.equal(f.requests.length >= 2, true);
  assert.ok(f.requests.every((r) => r.headers.host === `hub.test:${port}`), "nothing was sent for evil.test");
});

test("redirects: 307/308 to the same origin are followed; others are refused", async () => {
  const same = await startHttpFake({ redirect: { status: 307, location: "/mcp/next" } });
  const found = await startHttpFake({ redirect: { status: 302, location: "/mcp/next" } });
  const away = await startHttpFake({ redirect: { status: 308, location: "http://127.0.0.2:9/mcp" } });
  open.push(...[same, found, away].map((f) => ({ close: () => void f.close() })));
  const h = harness();
  open.push(h.bridge);
  h.bridge.configure({ revision: "r1", servers: [httpServer("same", same.url), httpServer("found", found.url), httpServer("away", away.url)] });
  const d = await h.bridge.tools({ server: "same", query: "echo" });
  assert.ok(d.loaded.length >= 1);
  assert.equal(same.requests[1]!.path, "/mcp/next");
  await assert.rejects(h.bridge.tools({ server: "found" }), /only 307 and 308/);
  await assert.rejects(h.bridge.tools({ server: "away" }), /another site/);
});

test("a server in both Helm and Claude Code: Helm's copy is kept and shadows Claude Code's", async () => {
  const h = harness();
  open.push(h.bridge);
  h.bridge.configure({
    revision: "r1",
    servers: [
      stdioServer("agent_standup", {}, { name: "Agent Standup" }),
      httpServer("hub", "https://hub.example/mcp/"),
      stdioServer("ha", {}, { name: "Home Assistant" }),
      stdioServer("broken", {}, { name: "Broken", error: "not resolved" }),
    ],
    native: [{ name: "agent-standup" }, { name: "memory", url: "https://HUB.example/mcp" }],
  });
  assert.equal(h.last("agent_standup")?.state, "idle", "never hidden behind Claude Code's copy");
  assert.equal(h.last("hub")?.state, "idle");
  assert.deepEqual(h.bridge.serverIds(), ["agent_standup", "hub", "ha", "broken"]);
  await h.bridge.tools({ server: "ha", query: "echo" });
  // The CLI's start-up report arrives later and names Home Assistant too.
  h.bridge.noteNative([{ name: "home_assistant" }]);
  assert.equal(h.bridge.isLoaded(generatedName("ha", "echo")), true, "its loaded tools stay");
  assert.deepEqual(h.bridge.shadowing("Home-Assistant"), { id: "ha", name: "Home Assistant" });
  assert.deepEqual(h.bridge.shadowing("agent-standup"), { id: "agent_standup", name: "Agent Standup" }, "by name, reported or not");
  assert.equal(h.bridge.shadowing("memory"), undefined, "the URL tie is gone with the report that carried it");
  h.bridge.noteNative([{ name: "memory", url: "https://hub.example/mcp" }]);
  assert.equal(h.bridge.shadowing("memory")?.id, "hub", "tied by URL");
  assert.equal(h.bridge.shadowing("broken"), undefined, "a Helm server that could not be resolved leaves Claude Code's copy usable");
  assert.equal(h.bridge.shadowing("github"), undefined);
});

test("normalisation and the denied native name", () => {
  assert.equal(normName("Agent-Standup"), normName("agent_standup"));
  assert.equal(normUrl("https://X.example/mcp/"), normUrl("https://x.example/mcp"));
  assert.equal(normUrl("not a url"), undefined);
  assert.deepEqual(deniedNative(["helm", "HELM", "h-e-l-m", "helmet", "github"]), ["helm", "HELM", "h-e-l-m"]);
});
