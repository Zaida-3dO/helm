// The composer's permission-mode pick: the host's `set_permission_mode` line
// becomes Claude Code's `set_permission_mode` control request, at once when idle and between turns
// when a turn is open; the worker then reports the live mode.

import assert from "node:assert/strict";
import { describe, test } from "node:test";
import { prompt, startWithFixture, waitReceived } from "./helpers.ts";

const isUser = (r: Record<string, unknown>) => r.type === "user";
const ctl = (subtype: string) => (r: Record<string, unknown>) =>
  r.type === "control_request" && (r.request as Record<string, unknown>)?.subtype === subtype;

describe("permission mode", () => {
  test("idle: the pick is applied at once and reported as the live mode", async () => {
    const s = startWithFixture("streaming");
    try {
      await s.host.ready();
      s.host.send({ type: "set_permission_mode", mode: "plan" });
      const req = await waitReceived(s, ctl("set_permission_mode"));
      assert.equal((req.request as Record<string, unknown>).mode, "plan");
      const ev = await s.host.waitFor((e) => e.type === "permission_mode" && e.mode === "plan", 5000, 0);
      assert.equal(ev.mode, "plan");
      const kept = await s.host.waitFor((e) => e.type === "permission_mode_kept", 5000, 0);
      assert.equal(kept.mode, "plan", "an accepted pick is what the host keeps");
    } finally {
      await s.done();
    }
  });

  test("an unknown mode is not sent to Claude Code", async () => {
    const s = startWithFixture("streaming");
    try {
      await s.host.ready();
      s.host.send({ type: "set_permission_mode", mode: "yolo" });
      s.host.send({ type: "set_permission_mode", mode: "acceptEdits" });
      const req = await waitReceived(s, ctl("set_permission_mode"));
      assert.equal((req.request as Record<string, unknown>).mode, "acceptEdits", "the first control request is the valid one");
      assert.equal(s.received().filter(ctl("set_permission_mode")).length, 1);
    } finally {
      await s.done();
    }
  });

  test("mid-turn: the pick waits and is applied before the next prompt", async () => {
    const s = startWithFixture("two-turns");
    try {
      await s.host.ready();
      await prompt(s.host, "first");
      await s.host.waitForType("message_start", 5000, 0);
      s.host.send({ type: "set_permission_mode", mode: "default" });
      await s.host.waitForType("agent_settled", 5000, 0);
      assert.equal(s.received().some(ctl("set_permission_mode")), false, "nothing changes mid-turn");
      const r2 = await prompt(s.host, "second");
      assert.equal(r2.success, true);
      const rec = s.received();
      const iMode = rec.findIndex(ctl("set_permission_mode"));
      const users = rec.map((r, i) => (isUser(r) ? i : -1)).filter((i) => i >= 0);
      assert.equal(users.length, 2);
      assert.ok(iMode > users[0] && iMode < users[1], "mode applied between the turns");
    } finally {
      await s.done();
    }
  });

  test("skip permission prompts allows tool requests until a pick that asks; then they become approval cards", async () => {
    const run = async (pick?: string) => {
      const s = startWithFixture("approval", { claude: { skipPermissions: true } });
      await s.host.ready();
      if (pick) {
        s.host.send({ type: "set_permission_mode", mode: pick });
        await waitReceived(s, ctl("set_permission_mode"));
      }
      await prompt(s.host, "list files");
      return s;
    };
    const plain = await run();
    try {
      const answer = await waitReceived(plain, (r) => r.type === "control_response");
      assert.equal((answer.response as { response: Record<string, unknown> }).response.behavior, "allow", "no pick: allowed without asking");
      assert.equal(plain.host.ofType("extension_ui_request").length, 0);
    } finally {
      await plain.done();
    }
    const asks = await run("default");
    try {
      const req = await asks.host.waitFor((e) => e.type === "extension_ui_request" && e.attention === "approval", 5000, 0);
      assert.equal(req.title, "Allow Bash?", "after picking Asks permission the request is an approval card");
      asks.host.send({ type: "extension_ui_response", id: req.id, value: "Deny" });
      await asks.host.waitForType("agent_settled", 5000, 0);
    } finally {
      await asks.done();
    }
  });

  test("after a pick, a mode Claude Code reports itself is the one the host keeps", async () => {
    // The approval fixture's init reports "default", as Claude Code would after leaving plan mode.
    const s = startWithFixture("approval");
    try {
      await s.host.ready();
      s.host.send({ type: "set_permission_mode", mode: "plan" });
      await s.host.waitFor((e) => e.type === "permission_mode" && e.mode === "plan", 5000, 0);
      await prompt(s.host, "list files");
      const kept = await s.host.waitFor((e) => e.type === "permission_mode_kept" && e.mode !== "plan", 5000, 0);
      assert.equal(kept.mode, "default");
      // The two lines are written back to back; wait for the second before counting.
      await s.host.waitFor((e) => e.type === "permission_mode" && e.mode === "default", 5000, 0);
      const live = s.host.ofType("permission_mode").map((e) => e.mode);
      assert.deepEqual(live, ["plan", "default"], "each change reaches the host once");
      const req = await s.host.waitFor((e) => e.type === "extension_ui_request", 5000, 0);
      s.host.send({ type: "extension_ui_response", id: req.id, value: "Deny" });
      await s.host.waitForType("agent_settled", 5000, 0);
    } finally {
      await s.done();
    }
  });

  test("without a pick, Claude Code's reports change nothing the host keeps", async () => {
    const s = startWithFixture("approval");
    try {
      await s.host.ready();
      await prompt(s.host, "list files");
      const req = await s.host.waitFor((e) => e.type === "extension_ui_request", 5000, 0);
      s.host.send({ type: "extension_ui_response", id: req.id, value: "Deny" });
      await s.host.waitForType("agent_settled", 5000, 0);
      assert.equal(s.host.ofType("permission_mode_kept").length, 0);
      assert.deepEqual(s.host.ofType("permission_mode").map((e) => e.mode), ["default"]);
    } finally {
      await s.done();
    }
  });
});
