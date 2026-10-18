import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { test } from "node:test";
import { fileURLToPath } from "node:url";
import { controlArgs, controlRequests, readControlUsage } from "../src/control.ts";
import { normaliseControl, toMillis } from "../src/normalise.ts";

const here = path.dirname(fileURLToPath(import.meta.url));
const FAKE = path.join(here, "fake-usage-claude.ts");

const BODY = {
  subscription_type: "max",
  rate_limits_available: true,
  rate_limits: {
    five_hour: { utilization: 5, resets_at: "2026-10-18T04:00:00.013026+00:00" },
    seven_day: { utilization: 56.5, resets_at: "2026-10-20T12:00:00+00:00" },
    model_scoped: [{ display_name: "Fable", utilization: 3, resets_at: "2026-10-20T12:00:00+00:00" }],
    seven_day_sonnet: { utilization: 9, resets_at: "2026-10-20T12:00:00+00:00" },
  },
};

function scenario(s: unknown): { command: string; commandArgs: string[]; log: string } {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), "helm-usage-test-"));
  const file = path.join(dir, "s.json");
  fs.writeFileSync(file, JSON.stringify(s));
  const log = path.join(dir, "log.jsonl");
  process.env.FAKE_USAGE_LOG = log;
  return { command: process.execPath, commandArgs: [FAKE, file], log };
}

const entries = (log: string): Array<Record<string, any>> =>
  fs.existsSync(log) ? fs.readFileSync(log, "utf8").trim().split("\n").filter(Boolean).map((l) => JSON.parse(l)) : [];

test("the reading comes from the get_usage answer and no user message is ever written", async () => {
  const s = scenario({ usage: BODY, version: "9.9.9" });
  const r = await readControlUsage({ command: s.command, commandArgs: s.commandArgs, timeoutMs: 10_000 });
  assert.ok(r.ok);
  assert.equal(r.version, "9.9.9");
  assert.deepEqual(r.reading.fiveHour, { pct: 5, resetsAt: Date.parse("2026-10-18T04:00:00.013026+00:00") });
  assert.equal(r.reading.weekly?.pct, 56.5);
  assert.equal(r.reading.plan, "max");
  assert.deepEqual(r.reading.perModel.map((m) => [m.model, m.pct]), [["Fable", 3], ["Sonnet", 9]]);
  const got = entries(s.log).filter((e) => e.kind === "in").map((e) => e.rec);
  assert.ok(got.length > 0);
  assert.deepEqual(
    got.map((g) => g.type),
    got.map(() => "control_request"),
    "only control requests reach the binary: a user line would be a model call",
  );
  assert.ok(got.some((g) => g.request.subtype === "get_usage"));
  assert.ok(controlRequests().every((l) => JSON.parse(l).type === "control_request"));
});

test("the child is a standalone CLI: stripped credentials, strict empty MCP, no setting sources, a neutral folder", async () => {
  process.env.ANTHROPIC_API_KEY = "test-key-not-real";
  process.env.CLAUDECODE = "1";
  try {
    const s = scenario({ usage: BODY });
    await readControlUsage({ command: s.command, commandArgs: s.commandArgs, timeoutMs: 10_000 });
    const start = entries(s.log).find((e) => e.kind === "start")!;
    assert.ok(!start.envKeys.includes("ANTHROPIC_API_KEY"));
    assert.ok(!start.envKeys.includes("CLAUDECODE"));
    const argv: string[] = start.argv;
    assert.ok(argv.includes("--strict-mcp-config"));
    assert.equal(argv[argv.indexOf("--mcp-config") + 1], '{"mcpServers":{}}');
    assert.equal(argv[argv.indexOf("--setting-sources") + 1], "");
    assert.ok(!argv.includes("--model"), "no model is named: nothing here can start a turn");
    assert.ok(path.basename(start.cwd).startsWith("helm-usage-"), "a neutral working folder");
    assert.ok(!fs.existsSync(start.cwd), "and it is removed afterwards");
  } finally {
    delete process.env.ANTHROPIC_API_KEY;
    delete process.env.CLAUDECODE;
  }
  assert.ok(controlArgs().includes("-p"));
});

test("a binary that refuses, exits or says nothing is reported, never thrown", async () => {
  const refused = scenario({ usage: { error: "Unknown request: get_usage" }, version: "1.0.0" });
  const r1 = await readControlUsage({ command: refused.command, commandArgs: refused.commandArgs, timeoutMs: 10_000 });
  assert.ok(!r1.ok && r1.reason === "unsupported" && /Unknown request/.test(r1.message) && r1.version === "1.0.0");
  const gone = scenario({ usage: "exit" });
  const r2 = await readControlUsage({ command: gone.command, commandArgs: gone.commandArgs, timeoutMs: 10_000 });
  assert.ok(!r2.ok && r2.reason === "unsupported");
  const hung = scenario({ usage: "hang" });
  const r3 = await readControlUsage({ command: hung.command, commandArgs: hung.commandArgs, timeoutMs: 400 });
  assert.ok(!r3.ok && r3.reason === "timeout");
  const none = scenario({ usage: { rate_limits_available: false } });
  const r4 = await readControlUsage({ command: none.command, commandArgs: none.commandArgs, timeoutMs: 10_000 });
  assert.ok(!r4.ok && r4.reason === "no_limits");
  const missing = await readControlUsage({ command: path.join(os.tmpdir(), "no-such-claude-binary.exe"), timeoutMs: 5_000 });
  assert.ok(!missing.ok && missing.reason === "error");
});

test("normalising: seconds, milliseconds and ISO strings all become milliseconds; unknown shapes give null", () => {
  assert.equal(toMillis(1_792_296_000), 1_792_296_000_000);
  assert.equal(toMillis(1_792_296_000_000), 1_792_296_000_000);
  assert.equal(toMillis("2026-10-18T04:00:00Z"), Date.parse("2026-10-18T04:00:00Z"));
  assert.equal(toMillis("soon"), null);
  assert.equal(normaliseControl({ rate_limits: {} }), null);
  assert.equal(normaliseControl("nope"), null);
  assert.equal(normaliseControl({ rate_limits_available: false, rate_limits: { five_hour: { utilization: 1 } } }), null);
  const only = normaliseControl({ rate_limits: { five_hour: { utilization: null, resets_at: "2026-10-18T04:00:00Z" } } });
  assert.deepEqual(only?.fiveHour, { pct: null, resetsAt: Date.parse("2026-10-18T04:00:00Z") });
  assert.equal(only?.weekly, null);
});

test("the helper's source never touches a credential store or the OAuth endpoints", () => {
  const dir = path.join(here, "..", "src");
  for (const f of fs.readdirSync(dir)) {
    const code = fs
      .readFileSync(path.join(dir, f), "utf8")
      .split("\n")
      .filter((l) => !l.trim().startsWith("//"))
      .join("\n")
      .toLowerCase();
    assert.ok(!code.includes("credentials"), `${f} mentions a credential store`);
    assert.ok(!code.includes("oauth"), `${f} mentions OAuth`);
    assert.ok(!code.includes("api.anthropic.com"), `${f} names an Anthropic endpoint`);
    assert.ok(!code.includes("fetch("), `${f} makes a network call`);
  }
});
