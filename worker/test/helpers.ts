import assert from "node:assert/strict";
import { randomUUID } from "node:crypto";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { FakeHost, type Json } from "../../fakes/host/fake-host.ts";

const here = path.dirname(fileURLToPath(import.meta.url));
export const WORKER = path.resolve(here, "../src/main.ts");
export const FAKE_CLAUDE = path.resolve(here, "../../fakes/claude/fake-claude.ts");
export const FIXTURES = path.resolve(here, "../../fakes/claude/fixtures");

export interface Started {
  host: FakeHost;
  logPath: string;
  /** Everything the fake claude received on stdin, in order. */
  received: () => Json[];
  /** The fake claude's start entry: argv and environment variable names. */
  fakeStart: () => Json | undefined;
  /** Asserts the protocol stayed clean, then ends the worker and removes temp files. */
  done: () => Promise<void>;
}

export interface StartOpts {
  claude?: Json;
  limits?: Json;
  env?: Record<string, string | undefined>;
  sendBehavior?: Json;
}

export function readJsonl(p: string): Json[] {
  if (!fs.existsSync(p)) return [];
  return fs
    .readFileSync(p, "utf8")
    .split("\n")
    .filter((l) => l.trim() !== "")
    .map((l) => JSON.parse(l) as Json);
}

export function startWithFixture(name: string, opts: StartOpts = {}): Started {
  const fixturePath = path.join(FIXTURES, `${name}.json`);
  assert.ok(fs.existsSync(fixturePath), `fixture ${name} exists`);
  const logPath = path.join(os.tmpdir(), `helm-fc-${randomUUID()}.log`);
  const limits: Json = { promptAckMs: 5000, stopSettleMs: 1500, controlTimeoutMs: 5000, shutdownGraceMs: 1000, ...(opts.limits ?? {}) };
  const launch: Json = {
    chatId: randomUUID(),
    harness: "claude",
    claude: { command: process.execPath, commandArgs: [FAKE_CLAUDE, fixturePath], ...(opts.claude ?? {}) },
    limits,
    ...(opts.sendBehavior ? { sendBehavior: opts.sendBehavior } : {}),
  };
  const host = new FakeHost({
    launcher: WORKER,
    launch,
    env: { ...process.env, ...(opts.env ?? {}), FAKE_CLAUDE_LOG: logPath },
    maxRecordBytes: typeof limits.maxRecordBytes === "number" ? limits.maxRecordBytes : undefined,
  });
  const entries = () => readJsonl(logPath);
  return {
    host,
    logPath,
    received: () => entries().filter((e) => e.kind === "in").map((e) => e.rec as Json),
    fakeStart: () => entries().find((e) => e.kind === "start"),
    done: async () => {
      try {
        assert.deepEqual(host.violations, [], "worker protocol violations");
      } finally {
        await host.dispose();
        fs.rmSync(logPath, { force: true });
      }
    },
  };
}

export function uuid(): string {
  return randomUUID();
}

/** Sends a prompt and returns its response. */
export function prompt(host: FakeHost, text: string, extra: Json = {}, submissionId: string = randomUUID()): Promise<Json> {
  return host.request({ type: "prompt", submissionId, text, ...extra });
}

/** Waits until the fake claude has received a record matching pred. */
export async function waitReceived(s: Started, pred: (r: Json) => boolean, timeoutMs = 5000): Promise<Json> {
  const end = Date.now() + timeoutMs;
  for (;;) {
    const hit = s.received().find(pred);
    if (hit) return hit;
    if (Date.now() > end) throw new Error(`fake claude never received the expected record; got ${JSON.stringify(s.received().map((r) => r.type))}`);
    await new Promise((r) => setTimeout(r, 25));
  }
}

export function sleep(ms: number): Promise<void> {
  return new Promise((r) => setTimeout(r, ms));
}
