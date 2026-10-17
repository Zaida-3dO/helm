// Driver-level coverage for the end-of-turn reset of the long-command escape, and for abort not
// reporting a steer the CLI already read as dropped.

import assert from "node:assert/strict";
import { describe, test } from "node:test";
import type { Json } from "../../fakes/host/fake-host.ts";
import { prompt, startWithFixture, uuid, type Started } from "./helpers.ts";

const hookAnswers = (s: Started) =>
  s
    .received()
    .filter((r) => r.type === "control_response" && String((r.response as Json)?.request_id ?? "").startsWith("fake-hook-"))
    .map((r) => ((r.response as Json).response as Json) ?? {});
const decision = (a: Json) => ((a.hookSpecificOutput as Json | undefined)?.permissionDecision as string | undefined) ?? "allow";

describe("long-command escape does not carry into the next turn", () => {
  test("a denial followed by a turn end, then the same command in the next turn, is denied again", async () => {
    const s = startWithFixture("leaf-endturn", { sendBehavior: { longCommands: "redirect" } });
    try {
      await s.host.ready();
      await prompt(s.host, "first");
      await s.host.waitFor(() => s.host.ofType("agent_settled").length === 1, 8000, 0);
      await prompt(s.host, "second");
      await s.host.waitFor(() => s.host.ofType("agent_settled").length === 2, 8000, 0);
      assert.deepEqual(hookAnswers(s).map(decision), ["deny", "deny"]);
      assert.deepEqual(
        s.host.ofType("long_command").map((e) => [e.toolCallId, e.action]),
        [["e1", "denied"], ["e2", "denied"]],
        "no escape in turn two",
      );
    } finally {
      await s.done();
    }
  });
});

describe("abort reports only steers the CLI really dropped", () => {
  test("a steer already read is not in cancelledSubmissionIds even if the CLI lists it", async () => {
    const s = startWithFixture("leaf-abort-delivered");
    try {
      await s.host.ready();
      await prompt(s.host, "work");
      await s.host.waitForType("tool_execution_start", 5000, 0);
      const steer = uuid();
      await prompt(s.host, "also this", { streamingBehavior: "steer" }, steer);
      await s.host.waitFor((e) => e.type === "submission_delivered" && e.submissionId === steer, 5000, 0);
      const r = await s.host.request({ type: "abort" });
      assert.equal(r.success, true, String(r.error));
      assert.deepEqual((r.data as Json).cancelledSubmissionIds, []);
      await s.host.waitForType("agent_settled", 5000, 0);
    } finally {
      await s.done();
    }
  });
});
