// Opt-in smoke test against the real claude executable. Skipped unless HELM_LIVE_CLAUDE=1.
// It spends a little of the signed-in plan's usage (one haiku turn), so run it once per
// change, never in a loop. Neutral temp working folder; user settings (and so the user's
// hooks) are not loaded; CLAUDECODE and credential variables are stripped by the worker.

import assert from "node:assert/strict";
import { randomUUID } from "node:crypto";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { test } from "node:test";
import { FakeHost } from "../../../fakes/host/fake-host.ts";
import { WORKER } from "../helpers.ts";

const live = process.env.HELM_LIVE_CLAUDE === "1";

function startReal(claude: Record<string, unknown>) {
  const cwd = fs.mkdtempSync(path.join(os.tmpdir(), "helm-live-"));
  const host = new FakeHost({
    launcher: WORKER,
    launch: { chatId: randomUUID(), harness: "claude", cwd, claude: { settingSources: ["project", "local"], ...claude } },
    env: { ...process.env, CLAUDECODE: "1" }, // must be stripped before claude sees it
  });
  return {
    host,
    done: async () => {
      await host.dispose();
      fs.rmSync(cwd, { recursive: true, force: true });
    },
  };
}

test("live: one haiku turn end to end through the real claude executable", { skip: !live, timeout: 180_000 }, async () => {
  const { host, done } = startReal({ model: "haiku" });
  try {
    const ready = await host.ready(60_000);
    assert.equal(ready.success, true, String(ready.error));
    const data = ready.data as Record<string, unknown>;
    process.stderr.write(`live: executable ${String(data.executable)} (${String(data.executableSource)}), ${(data.models as unknown[]).length} models\n`);
    const sid = randomUUID();
    const r = await host.request({ type: "prompt", submissionId: sid, text: "Reply with exactly one word: pong" }, 60_000);
    assert.equal(r.success, true, `prompt: ${JSON.stringify(r)}`);
    const settled = await host.waitFor((e) => e.type === "agent_settled", 120_000, 0);
    assert.equal(settled.outcome, "complete", JSON.stringify(settled));
    const text = host
      .ofType("message_end")
      .map((e) => e.message as { role: string; content: { type: string; text?: string }[] })
      .filter((m) => m.role === "assistant")
      .flatMap((m) => m.content.filter((b) => b.type === "text").map((b) => b.text ?? ""))
      .join("");
    process.stderr.write(`live: reply ${JSON.stringify(text)}; events ${[...new Set(host.events.map((e) => e.type))].join(",")}\n`);
    assert.match(text, /pong/i);
    assert.ok(host.ofType("message_update").length > 0, "partial messages streamed");
    assert.equal(host.ofType("usage").length, 1);
    assert.ok(host.ofType("native_session").length >= 1);
    assert.deepEqual(host.violations, []);
  } finally {
    await done();
  }
});

test("live: model and effort changes are acknowledged by the real CLI (no model request)", { skip: !live, timeout: 120_000 }, async () => {
  const { host, done } = startReal({});
  try {
    const ready = await host.ready(60_000);
    assert.equal(ready.success, true, String(ready.error));
    const e = await host.request({ type: "set_thinking_level", level: "low" }, 10_000);
    assert.equal(e.success, true, `effort: ${JSON.stringify(e)}`);
    const m = await host.request({ type: "set_model", model: "sonnet" }, 10_000);
    assert.equal(m.success, true, `model: ${JSON.stringify(m)}`);
    assert.deepEqual(host.violations, []);
  } finally {
    await done();
  }
});
