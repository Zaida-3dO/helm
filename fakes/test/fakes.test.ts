import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { describe, test } from "node:test";
import { fileURLToPath } from "node:url";
import { FakeHost } from "../host/fake-host.ts";

const here = path.dirname(fileURLToPath(import.meta.url));
const FAKE_CLAUDE = path.resolve(here, "../claude/fake-claude.ts");
const FIXTURES = path.resolve(here, "../claude/fixtures");
const BAD_WORKER = path.resolve(here, "bad-worker.ts");

describe("fake host protocol checks", () => {
  for (const [mode, expected] of [
    ["double", /second response/],
    ["garbage", /non-JSON line/],
    ["wrong-command", /names command other/],
  ] as const) {
    test(`flags a worker that sends ${mode}`, async () => {
      const h = new FakeHost({ launcher: BAD_WORKER, launch: { mode } });
      try {
        await h.request({ type: "get_state", id: "ready" });
        await new Promise((r) => setTimeout(r, 100));
        assert.ok(h.violations.some((v) => expected.test(v)), `violations: ${JSON.stringify(h.violations)}`);
      } finally {
        await h.dispose();
      }
    });
  }

  test("a well-behaved worker produces no violations", async () => {
    const h = new FakeHost({ launcher: BAD_WORKER, launch: { mode: "good" } });
    try {
      const r = await h.request({ type: "get_state", id: "ready" });
      assert.equal(r.success, true);
      assert.deepEqual(h.violations, []);
      assert.deepEqual(h.unanswered(), []);
    } finally {
      await h.dispose();
    }
  });
});

function runFake(fixture: string, flags: string[], input: string[]): Promise<{ lines: Record<string, unknown>[]; code: number | null; log: Record<string, unknown>[] }> {
  const logPath = path.join(os.tmpdir(), `helm-fake-self-${process.pid}-${Math.random().toString(36).slice(2)}.log`);
  return new Promise((resolve) => {
    const child = spawn(process.execPath, [FAKE_CLAUDE, path.join(FIXTURES, `${fixture}.json`), ...flags], {
      env: { ...process.env, FAKE_CLAUDE_LOG: logPath },
      stdio: ["pipe", "pipe", "pipe"],
    });
    let out = "";
    child.stdout.setEncoding("utf8");
    child.stdout.on("data", (d: string) => (out += d));
    for (const l of input) child.stdin.write(l + "\n");
    setTimeout(() => child.stdin.end(), 300);
    child.on("close", (code) => {
      const log = fs.existsSync(logPath)
        ? fs.readFileSync(logPath, "utf8").split("\n").filter(Boolean).map((l) => JSON.parse(l) as Record<string, unknown>)
        : [];
      fs.rmSync(logPath, { force: true });
      resolve({ lines: out.split("\n").filter(Boolean).map((l) => JSON.parse(l) as Record<string, unknown>), code, log });
    });
  });
}

describe("fake claude", () => {
  test("answers initialize, echoes user messages once per uuid, and replays the script", async () => {
    const user = JSON.stringify({ type: "user", uuid: "u-1", message: { role: "user", content: "hi" } });
    const { lines, log } = await runFake("streaming", ["--replay-user-messages"], [
      JSON.stringify({ type: "control_request", request_id: "r1", request: { subtype: "initialize" } }),
      user,
      user,
    ]);
    assert.equal(lines[0].type, "control_response");
    const echoes = lines.filter((l) => l.type === "user" && l.uuid === "u-1");
    assert.equal(echoes.length, 1, "duplicate uuid is not echoed twice");
    assert.equal(lines.filter((l) => l.type !== "command_lifecycle").at(-1)?.type, "result");
    assert.equal(log.filter((e) => e.kind === "in").length, 3);
  });

  test("no echo without --replay-user-messages", async () => {
    const { lines } = await runFake("streaming", [], [JSON.stringify({ type: "user", uuid: "u-2", message: { role: "user", content: "hi" } })]);
    assert.equal(lines.some((l) => l.type === "user"), false);
  });

  test("every fixture parses and has steps", () => {
    const names = fs.readdirSync(FIXTURES).filter((f) => f.endsWith(".json"));
    assert.ok(names.length >= 10);
    for (const n of names) {
      const f = JSON.parse(fs.readFileSync(path.join(FIXTURES, n), "utf8")) as { steps: unknown[]; description: string };
      assert.ok(Array.isArray(f.steps) && f.steps.length > 0, n);
      assert.equal(typeof f.description, "string", n);
    }
  });
});
