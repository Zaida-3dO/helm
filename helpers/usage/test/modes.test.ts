import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { test } from "node:test";
import { classify, countModes, encodeCwd } from "../src/modes.ts";

const SID = "11111111-2222-3333-4444-555555555555";
const CWD = "C:/work/proj one";

function rec(id: string, ts: string, usage: Record<string, unknown> | null, extra: Record<string, unknown> = {}): string {
  return JSON.stringify({ type: "assistant", timestamp: ts, requestId: `req_${id}`, uuid: `u_${id}`, message: { id: `msg_${id}`, ...(usage ? { usage } : {}) }, ...extra });
}

function setup(lines: string[], sub: string[] = []): { dir: string; job: Parameters<typeof countModes>[0] } {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), "helm-modes-"));
  const proj = path.join(dir, encodeCwd(CWD));
  fs.mkdirSync(path.join(proj, SID, "subagents"), { recursive: true });
  fs.writeFileSync(path.join(proj, `${SID}.jsonl`), lines.join("\n") + "\n");
  if (sub.length) fs.writeFileSync(path.join(proj, SID, "subagents", "agent-1.jsonl"), sub.join("\n") + "\n");
  return { dir, job: { projectsDir: dir, sessions: [{ sessionId: SID, cwd: CWD }], since: Date.parse("2026-10-01T00:00:00Z"), until: Date.parse("2026-10-31T00:00:00Z") } };
}

test("modes: fast, priority and ultrafast are fast; standard, default, flex and batch standard; the rest unknown", () => {
  for (const v of ["fast", "Priority", "ultrafast"]) assert.equal(classify(v, undefined), "fast");
  for (const v of ["standard", "default", "flex", "batch"]) assert.equal(classify(undefined, v), "standard");
  assert.equal(classify("turbo", 7), "unknown");
  assert.equal(classify(undefined, undefined), "unknown");
  assert.equal(classify("fast", "standard"), "fast", "the per-request speed field comes first");
});

test("each request counts once even when its blocks are split across records, and the range applies", () => {
  const { dir, job } = setup(
    [
      rec("1", "2026-10-10T10:00:00Z", { speed: "fast" }),
      rec("1", "2026-10-10T10:00:01Z", { speed: "fast" }, { apiBlockIndex: 1 }),
      rec("2", "2026-10-11T10:00:00Z", { speed: "standard", service_tier: "standard" }),
      rec("3", "2026-10-12T10:00:00Z", { service_tier: "priority" }),
      rec("4", "2026-10-13T10:00:00Z", { input_tokens: 3 }),
      rec("5", "2026-10-14T10:00:00Z", null),
      rec("6", "2026-09-01T10:00:00Z", { speed: "fast" }),
      rec("7", "2026-11-01T10:00:00Z", { speed: "fast" }),
      JSON.stringify({ type: "user", timestamp: "2026-10-10T10:00:00Z", message: { usage: { speed: "fast" } } }),
      "not json but mentions \"assistant\"",
    ],
    [rec("8", "2026-10-15T10:00:00Z", { speed: "standard" })],
  );
  try {
    const r = countModes(job);
    assert.deepEqual({ fast: r.fast, standard: r.standard, unknown: r.unknown }, { fast: 2, standard: 2, unknown: 1 });
    assert.equal(r.files, 2, "the session and its sub-agent transcript");
    assert.equal(r.malformed, 1);
  } finally {
    fs.rmSync(dir, { recursive: true, force: true });
  }
});

test("a transcript last written before the range is not read, and a missing one is skipped", () => {
  const { dir, job } = setup([rec("1", "2026-10-10T10:00:00Z", { speed: "fast" })]);
  try {
    const old = new Date("2026-01-01T00:00:00Z");
    fs.utimesSync(path.join(dir, encodeCwd(CWD), `${SID}.jsonl`), old, old);
    assert.equal(countModes(job).files, 0);
    assert.deepEqual(
      countModes({ ...job, sessions: [{ sessionId: "99999999-0000-0000-0000-000000000000", cwd: CWD }] }).files,
      0,
    );
    assert.equal(countModes({ ...job, sessions: [{ sessionId: "../../etc", cwd: CWD }] }).files, 0, "a session id is never a path");
  } finally {
    fs.rmSync(dir, { recursive: true, force: true });
  }
});

test("the folder name is every non-alphanumeric character turned into a dash", () => {
  assert.equal(encodeCwd("c:\\Users\\a b\\x.y"), "c--Users-a-b-x-y");
  assert.equal(encodeCwd("C:/Users/a"), "C--Users-a");
});
