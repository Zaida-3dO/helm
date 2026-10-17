// MC5: about ten servers in one worker (8 stdio, 2 http). Discovery and calls succeed on every
// working server, a failing one does not block the others, and each shows its own status.

import assert from "node:assert/strict";
import { test } from "node:test";
import { startHttpFake } from "../../fakes/mcp-server/server.ts";
import { generatedName } from "../src/mcp/index.ts";
import { harness, httpServer, stdioServer, text } from "./mcp-helpers.ts";

test("ten servers, one failing", async () => {
  const fakes = await Promise.all([startHttpFake({}), startHttpFake({ sse: true })]);
  const h = harness();
  try {
    const servers = [
      ...Array.from({ length: 7 }, (_, i) => stdioServer(`s${i}`, { tools: i })),
      stdioServer("broken", { failStart: "exit" }),
      httpServer("h0", fakes[0]!.url),
      httpServer("h1", fakes[1]!.url),
    ];
    h.bridge.configure({ revision: "r1", servers });
    assert.equal(h.bridge.serverIds().length, 10);
    const results = await Promise.allSettled(
      servers.map(async (s) => {
        await h.bridge.tools({ server: s.id, query: "echo" });
        return text(await h.bridge.call(generatedName(s.id, "echo"), { text: s.id }));
      }),
    );
    const ok = results.filter((r) => r.status === "fulfilled").map((r) => (r as PromiseFulfilledResult<string>).value);
    assert.equal(ok.length, 9);
    for (const s of servers.filter((x) => x.id !== "broken")) assert.ok(ok.includes(`{"text":"${s.id}"}`), s.id);
    assert.equal(results[7]!.status, "rejected");
    const states = Object.fromEntries(h.bridge.status().map((s) => [s.serverId, s.state]));
    assert.equal(states.broken, "failed");
    for (const s of servers.filter((x) => x.id !== "broken")) assert.equal(states[s.id], "connected", s.id);
  } finally {
    h.bridge.close();
    await Promise.all(fakes.map((f) => f.close()));
  }
});
