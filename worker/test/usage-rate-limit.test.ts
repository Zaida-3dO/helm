// The passive plan-usage reader: rate_limit_event lines from the claude binary
// become `plan_usage` lines to the host, and nothing else changes in the turn.

import assert from "node:assert/strict";
import { describe, test } from "node:test";
import { planUsageFromRateLimit } from "../src/usage/rateLimit.ts";
import { prompt, startWithFixture } from "./helpers.ts";

describe("planUsageFromRateLimit", () => {
  test("a fraction becomes a percent, seconds become milliseconds, and the window name is kept", () => {
    const l = planUsageFromRateLimit({ rate_limit_info: { rateLimitType: "five_hour", utilization: 0.82, resetsAt: 1_792_296_000 } }, 5);
    assert.deepEqual(l, {
      type: "plan_usage",
      source: "rate_limit_event",
      windows: [{ kind: "five_hour", pct: 82, resetsAt: 1_792_296_000_000 }],
      at: 5,
    });
  });

  test("a percent above 1 is kept as is and capped at 100; a reset-only line has a null number", () => {
    const high = planUsageFromRateLimit({ rate_limit_info: { rateLimitType: "seven_day", utilization: 250, resetsAt: 1_792_296_000_000 } }, 1);
    assert.equal(high?.windows[0].pct, 100);
    assert.equal(high?.windows[0].resetsAt, 1_792_296_000_000);
    const resetOnly = planUsageFromRateLimit({ rate_limit_info: { rateLimitType: "seven_day_opus", resetsAt: 1_792_296_000 } }, 1);
    assert.equal(resetOnly?.windows[0].pct, null);
  });

  test("overage, unknown windows and empty events say nothing", () => {
    assert.equal(planUsageFromRateLimit({ rate_limit_info: { rateLimitType: "overage", utilization: 0.5 } }, 1), null);
    assert.equal(planUsageFromRateLimit({ rate_limit_info: { rateLimitType: "mystery", utilization: 0.5 } }, 1), null);
    assert.equal(planUsageFromRateLimit({ rate_limit_info: { rateLimitType: "five_hour" } }, 1), null);
    assert.equal(planUsageFromRateLimit({}, 1), null);
  });
});

describe("the Claude driver forwards rate_limit_event", () => {
  test("two usable events reach the host as plan_usage lines and the turn still settles", async () => {
    const s = startWithFixture("../../../worker/test/fixtures/usage-rate-limit");
    try {
      await s.host.ready();
      await prompt(s.host, "hello");
      await s.host.waitFor(() => s.host.ofType("agent_settled").length === 1, 8000, 0);
      const lines = s.host.ofType("plan_usage");
      assert.equal(lines.length, 2, "the overage line is not forwarded");
      const first = (lines[0].windows as Array<Record<string, unknown>>)[0];
      assert.equal(first.kind, "five_hour");
      assert.equal(first.pct, 82);
      assert.equal(first.resetsAt, 1_792_296_000_000);
      const second = (lines[1].windows as Array<Record<string, unknown>>)[0];
      assert.equal(second.kind, "seven_day");
      assert.equal(second.pct, null);
    } finally {
      await s.done();
    }
  });
});
