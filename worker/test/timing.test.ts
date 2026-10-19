// The send timeline's worker marks (v0.2 latency): launch marks on the get_state response, prompt
// marks on the prompt's response, both as a top-level `timing` field.

import assert from "node:assert/strict";
import { describe, test } from "node:test";
import { prompt, startWithFixture } from "./helpers.ts";
import { markPrompt, promptMarks } from "../src/protocol/timing.ts";

describe("send timeline marks", () => {
  test("get_state carries the launch marks in order", async () => {
    const s = startWithFixture("streaming");
    try {
      const r = await s.host.ready();
      assert.equal(r.success, true);
      const t = r.timing as Record<string, number>;
      const order = ["workerStart", "mainStart", "claudeSpawned", "initialized", "versioned", "ready"];
      for (const k of order) assert.equal(typeof t?.[k], "number", `${k} in ${JSON.stringify(t)}`);
      for (let i = 1; i < order.length; i++) {
        assert.ok(t[order[i]] >= t[order[i - 1]], `${order[i - 1]} <= ${order[i]}: ${JSON.stringify(t)}`);
      }
      assert.ok(Math.abs(t.ready - Date.now()) < 60_000, "epoch milliseconds");
    } finally {
      await s.done();
    }
  });

  test("a prompt's response carries when it arrived and when it reached Claude", async () => {
    const s = startWithFixture("streaming");
    try {
      await s.host.ready();
      const r = await prompt(s.host, "hello");
      assert.equal(r.success, true);
      assert.deepEqual(Object.keys(r.data as object).sort(), ["state", "submissionId"], "data is unchanged");
      const t = r.timing as Record<string, number>;
      assert.equal(typeof t?.received, "number", JSON.stringify(r));
      assert.equal(typeof t?.written, "number", JSON.stringify(r));
      assert.ok(t.written >= t.received);
    } finally {
      await s.done();
    }
  });

  test("the v02-latency fixture answers prompt after prompt, each with its own marks", async () => {
    const s = startWithFixture("v02-latency");
    try {
      await s.host.ready();
      for (const [i, text] of ["one", "two"].entries()) {
        const r = await prompt(s.host, text);
        assert.equal(r.success, true);
        assert.equal(typeof (r.timing as Record<string, number>)?.written, "number");
        const end = Date.now() + 5000;
        while (s.host.ofType("agent_settled").length < i + 1) {
          assert.ok(Date.now() < end, `turn ${i + 1} settled`);
          await new Promise((res) => setTimeout(res, 20));
        }
        assert.equal(s.host.ofType("agent_settled")[i]?.outcome, "complete");
      }
    } finally {
      await s.done();
    }
  });

  test("prompt marks keep the first value and the table stays bounded", () => {
    markPrompt("sub-a", "received", 10);
    markPrompt("sub-a", "received", 20);
    markPrompt(undefined, "received", 30);
    assert.deepEqual(promptMarks("sub-a"), { received: 10 });
    assert.deepEqual(promptMarks("sub-a"), { received: 10 }, "a repeat gets the same marks");
    for (let i = 0; i < 64; i++) markPrompt(`sub-${i}`, "received", i);
    assert.equal(promptMarks("sub-a"), undefined, "the oldest is dropped past 64");
    assert.deepEqual(promptMarks("sub-63"), { received: 63 });
  });
});
