// Send-path latency, timed end to end with the fake claude, so pre-warm, the cheaper worker start
// and the hot-path writes cannot quietly regress.
//
// - Host-side marks come from the send timeline (`<profile>/diagnostics/send-timeline.log`, one
//   line per turn-opening send, times in ms after the send was saved).
// - Screen times come from the page: the click, and the first animation frame that shows the
//   reply's text.
// - Thresholds guard the gains against regression, measured on a Windows dev machine over Remote Desktop
//   (32 Hz, so screen times move in 31 ms frames). Before v0.2: cold saved-to-claude about 349 ms,
//   no pre-warm (a first send took 555 ms to its first token), warm delta-to-screen 64-72 ms.
//   After: about 205-245 ms, about 190 ms, and 51-82 ms (noisy: one 32 Hz frame is 31 ms and
//   the poll runs once a frame). The targets were 200, 250 and 50 ms: L2 meets its
//   target and guards pre-warm; L1 guards against a return to the old cold start (with headroom
//   for a busy machine); L3 only catches a large screen-side slowdown. The first-delta flush
//   itself is guarded by helm-chats' unit tests, not here.
//   HELM_E2E_LATENCY_SCALE (default 1) stretches them on a slower machine such as CI.
//
// Run: `npm run build:debug` at the root, then `node --test e2e/v02-latency.e2e.ts` in checks/.

import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";
import { after, before, describe, test } from "node:test";
import { api, dispose, FAKE_CLAUDE, launchApp, newChat, REPO, scratchDir, shot, tid, typeMessage, waitStatus, type App } from "./harness.ts";

const FIXTURE = path.join(REPO, "fakes", "claude", "fixtures", "v02-latency.json");
const SCALE = Number(process.env.HELM_E2E_LATENCY_SCALE ?? 1) || 1;
/** The reply's first word: "first token on screen" is when it shows. */
const REPLY = "pong";
const OUT = process.env.HELM_E2E_OUT ?? scratchDir("helm-e2e-out-");
const profile = scratchDir("helm-e2e-latency-");
const folder = () => scratchDir("helm-e2e-chat-");
let app: App | undefined;
const page = () => app!.page;
const results: Record<string, unknown> = {};

interface Line {
  saved: number;
  chat: string;
  marks: Record<string, number>;
  kind: string;
}

/** The send-timeline lines so far, parsed: `<saved> chat <id>: send <sid> timeline (...): a +1, b +2; kind`. */
function timeline(): Line[] {
  const file = path.join(profile, "diagnostics", "send-timeline.log");
  if (!fs.existsSync(file)) return [];
  return fs
    .readFileSync(file, "utf8")
    .split("\n")
    .filter(Boolean)
    .map((l) => {
      const m = /^(\d+) chat (\S+): send \S+ timeline \([^)]*\): (.*); (\S+)$/.exec(l);
      assert.ok(m, `a timeline line: ${l}`);
      const marks: Record<string, number> = {};
      for (const part of m[3].split(", ")) {
        const p = /^(.*) ([+-]\d+)$/.exec(part);
        if (p) marks[p[1]] = Number(p[2]);
      }
      return { saved: Number(m[1]), chat: m[2], marks, kind: m[4] };
    });
}

async function lineFor(chat: string, n: number): Promise<Line> {
  const end = Date.now() + 20_000;
  for (;;) {
    const lines = timeline().filter((l) => l.chat === chat);
    if (lines.length >= n) return lines[n - 1];
    if (Date.now() > end) throw new Error(`no timeline line ${n} for ${chat}: ${JSON.stringify(timeline())}`);
    await new Promise((r) => setTimeout(r, 50));
  }
}

/** Clicks Send and answers the click time and the first frame that shows reply number `n`. */
async function timedSend(n: number): Promise<{ click: number; token: number }> {
  return page().evaluate(
    ([sel, want, count]) =>
      new Promise<{ click: number; token: number }>((resolve, reject) => {
        const shown = () =>
          [...document.querySelectorAll('[data-testid="transcript-row"][data-role="assistant"]')].filter((r) => (r.textContent ?? "").includes(want as string)).length >= (count as number);
        const click = Date.now();
        (document.querySelector(sel as string) as HTMLElement).click();
        const end = click + 20_000;
        const poll = () => {
          if (shown()) return resolve({ click, token: Date.now() });
          if (Date.now() > end) return reject(new Error("the reply never showed"));
          requestAnimationFrame(poll);
        };
        poll();
      }),
    [tid("send"), REPLY, n] as const,
  );
}

/** The chat's worker has answered `get_state` (the fake reports every capability). */
async function workerReady(chat: string): Promise<void> {
  const end = Date.now() + 20_000;
  for (;;) {
    const r = (await api(page(), "chat_get", { chatId: chat })) as { thread: { capabilities: { compact?: boolean } } };
    if (r.thread.capabilities.compact) return;
    if (Date.now() > end) throw new Error("the pre-warmed worker never became ready");
    await new Promise((res) => setTimeout(res, 50));
  }
}

async function failShot(name: string, fn: () => Promise<void>): Promise<void> {
  try {
    await fn();
  } catch (e) {
    await shot(page(), OUT, `${name}-FAILED`).catch(() => undefined);
    throw new Error(`${(e as Error).message}\n--- app log tail ---\n${app?.log().slice(-3000)}`);
  }
}

describe("v0.2 latency with the fake claude", { timeout: 10 * 60_000 }, () => {
  before(async () => {
    process.stderr.write(`e2e: profile ${profile}\n`);
    app = await launchApp(profile, {
      skipPermissions: true,
      env: { HELM_TEST_CLAUDE: JSON.stringify({ command: process.execPath, args: [FAKE_CLAUDE, FIXTURE] }) },
    });
    // A first chat stages the worker and fills Node's compile cache, as any profile has after
    // its first chat; the measured sends below are not a brand-new install's.
    await newChat(page(), folder());
    await typeMessage(page(), "warm up");
    await timedSend(1);
    await waitStatus(page(), "idle");
  });

  after(async () => {
    process.stderr.write(`e2e: v02-latency results ${JSON.stringify(results)}\n`);
    await dispose(app);
  });

  test("L1: a cold send reaches claude's start within 300 ms of being saved", async () => {
    await failShot("L1", async () => {
      const chat = await newChat(page(), folder());
      const sid = crypto.randomUUID();
      await api(page(), "chat_send", { chatId: chat, submissionId: sid, text: "cold one", action: "default", draftRevision: 0, origin: "action" });
      await waitStatus(page(), "idle");
      const line = await lineFor(chat, 1);
      results.cold = line;
      assert.equal(line.kind, "cold", JSON.stringify(line));
      assert.ok(line.marks.claude !== undefined, JSON.stringify(line));
      assert.ok(line.marks.claude < 300 * SCALE, `saved to claude start ${line.marks.claude} ms: ${JSON.stringify(line)}`);
    });
  });

  test("L2: typing pre-warms the worker, so the first send shows its reply within 250 ms", async () => {
    await failShot("L2", async () => {
      const chat = await newChat(page(), folder());
      await typeMessage(page(), "warm one");
      await workerReady(chat);
      const t = await timedSend(1);
      const ms = t.token - t.click;
      const line = await lineFor(chat, 1);
      results.prewarmed = { clickToFirstTokenMs: ms, line };
      assert.equal(line.kind, "warm", JSON.stringify(line));
      assert.ok(ms < 250 * SCALE, `pre-warmed click to first token ${ms} ms`);
      await waitStatus(page(), "idle");
    });
  });

  test("L3: on a warm worker the first delta reaches the screen within 120 ms of the host (no large UI slowdown)", async () => {
    await failShot("L3", async () => {
      const chat = await newChat(page(), folder());
      await typeMessage(page(), "first");
      await timedSend(1);
      await waitStatus(page(), "idle");
      const samples: number[] = [];
      for (let i = 2; i <= 4; i++) {
        await typeMessage(page(), `again ${i}`);
        const t = await timedSend(i);
        const line = await lineFor(chat, i);
        assert.equal(line.kind, "warm", JSON.stringify(line));
        samples.push(t.token - (line.saved + line.marks["first output"]));
        await waitStatus(page(), "idle");
      }
      samples.sort((a, b) => a - b);
      results.warmDeltaToScreenMs = samples;
      const median = samples[1];
      assert.ok(median < 120 * SCALE, `delta to screen (median of 3) ${median} ms: ${JSON.stringify(samples)}`);
    });
  });
});
