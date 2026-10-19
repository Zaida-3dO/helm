// v0.2 latency: the host sets NODE_COMPILE_CACHE for the worker's own start; it never reaches
// the claude child (nor anything the agent runs).

import assert from "node:assert/strict";
import { test } from "node:test";
import { startWithFixture } from "./helpers.ts";

test("NODE_COMPILE_CACHE is dropped from claude's environment", async () => {
  const s = startWithFixture("streaming", { env: { NODE_COMPILE_CACHE: "C:/nowhere/node-cache" } });
  try {
    await s.host.ready();
    const keys = (s.fakeStart()?.envKeys as string[]).map((k) => k.toUpperCase());
    assert.ok(keys.includes("PATH"), "the environment was recorded");
    assert.equal(keys.includes("NODE_COMPILE_CACHE"), false);
  } finally {
    await s.done();
  }
});
