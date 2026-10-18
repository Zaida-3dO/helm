// The run-slot queue end to end. The real debug app runs two chats on
// a turn that never ends, with lowRunningChats = 2, and an outside agent messages other chats
// through a real helm-hub (an automated, gated send):
//
// - S1  with both slots held, the message waits: the tool answers waitingForSlot and a position,
//       and the queue shows it; the chat does not start;
// - S2  Start now starts it at once, over the limit;
// - S3  Cancel wait ends a wait and leaves its chat idle;
// - S4  when a running chat stops, the next wait starts by itself (no overtaking: in order);
// - S5  the user's own message to a chat never waits, even with every slot held.
//
// The usage governor's hold (wind-down and stop: nothing new starts) is pinned in Rust
// (helm-chats runslots tests, helm-governor service tests). Run: `npm run build:debug` at the root
// and `cargo build` in hub/, then `node --test e2e/w7-runslot.e2e.ts` in checks/.

import assert from "node:assert/strict";
import { spawn, type ChildProcess } from "node:child_process";
import net from "node:net";
import path from "node:path";
import { after, before, describe, test } from "node:test";
import { api, dispose, launchApp, newChat, openChat, REPO, scratchDir, send, waitText, type App } from "./harness.ts";

const HUB_EXE =
  process.env.HELM_E2E_HUB_EXE ?? path.join(REPO, "hub", "target", "debug", process.platform === "win32" ? "helm-hub.exe" : "helm-hub");
// Made-up test tokens, used only between this test's own processes.
const LINE = "e2e-line-token-0123456789abcdefgh";
const AGENT = "e2e-agent-token-0123456789abcdefg";

const profile = scratchDir("helm-e2e-profile-");
const work = scratchDir("helm-e2e-work-");
let app: App | undefined;
let hub: ChildProcess | undefined;
let hubLog = "";
let hubUrl = "";
type Json = Record<string, any>;

async function freePort(): Promise<number> {
  return new Promise((resolve) => {
    const s = net.createServer();
    s.listen(0, "127.0.0.1", () => {
      const p = (s.address() as net.AddressInfo).port;
      s.close(() => resolve(p));
    });
  });
}

async function until<T>(what: string, f: () => T | undefined | Promise<T | undefined>, ms = 30_000): Promise<T> {
  const end = Date.now() + ms;
  for (;;) {
    const v = await f();
    if (v !== undefined && v !== null && v !== false) return v as T;
    if (Date.now() > end) throw new Error(`timed out waiting for ${what}; hub log: ${hubLog.slice(-1500)}; app log: ${app?.log().slice(-1500)}`);
    await new Promise((r) => setTimeout(r, 200));
  }
}

async function outsideSend(chatId: string, message: string): Promise<Json> {
  const r = await fetch(`${hubUrl}/mcp`, {
    method: "POST",
    headers: { authorization: `Bearer ${AGENT}`, "content-type": "application/json" },
    body: JSON.stringify({ jsonrpc: "2.0", id: 1, method: "tools/call", params: { name: "app_send_message", arguments: { chatId, message, mode: "queue" } } }),
  });
  assert.equal(r.status, 200);
  const res = ((await r.json()) as Json).result as Json;
  assert.notEqual(res.isError, true, JSON.stringify(res));
  return res.structuredContent as Json;
}

const statusOf = async (id: string) => ((await api(app!.page, "chat_list", {})) as Json[]).find((c) => c.id === id)?.status as string;
const queue = async () => (await api(app!.page, "resource_queue_snapshot", {})) as Json;

describe("the run-slot queue", { timeout: 10 * 60_000 }, () => {
  const holders: string[] = [];
  let c = "";
  let d = "";
  let e = "";
  let f = "";
  before(async () => {
    const port = await freePort();
    hubUrl = `http://127.0.0.1:${port}`;
    hub = spawn(HUB_EXE, ["serve"], {
      env: {
        ...process.env,
        HUB_DB: path.join(scratchDir("helm-e2e-hub-"), "hub.db"),
        HUB_BIND: `127.0.0.1:${port}`,
        HUB_TOKENS: `helm-line:${LINE}:relay,agent:${AGENT}:ro+app`,
      },
      stdio: ["ignore", "pipe", "pipe"],
    });
    hub.stderr?.on("data", (x: Buffer) => (hubLog = (hubLog + x.toString()).slice(-20_000)));
    await until("the hub to listen", async () => (await fetch(`${hubUrl}/health`).then((r) => r.ok).catch(() => false)) || undefined);
    app = await launchApp(profile, { env: { HELM_TEST_RELAY_URL: `ws://127.0.0.1:${port}/relay`, HELM_TEST_RELAY_TOKEN: LINE } });
    await until("the line to come up", () => (hubLog.includes("relay: line up") ? true : undefined));
    const v = (await api(app.page, "run_slots_settings", { settings: { lowRunningChats: 2 } })) as Json;
    assert.equal(v.limit, 2);
    // Two chats on a turn that never ends hold both slots.
    for (let i = 0; i < 2; i++) {
      const id = await newChat(app.page, work);
      await send(app.page, "[E8-run] hold a slot");
      await until(`chat ${i} to run`, async () => ((await statusOf(id)) === "running" ? true : undefined));
      holders.push(id);
    }
    c = await newChat(app.page, work);
    d = await newChat(app.page, work);
    e = await newChat(app.page, work);
    f = await newChat(app.page, work);
  });
  after(async () => {
    await dispose(app);
    hub?.kill();
  });

  test("S1: with both slots held, an automated message waits and the chat does not start", async () => {
    const r = await outsideSend(c, "slot message for C");
    assert.equal(r.waitingForSlot, true, JSON.stringify(r));
    assert.equal(r.position, 1);
    assert.equal(r.delivered, false);
    const q = await queue();
    assert.deepEqual(q.waiting.map((w: Json) => w.chatId), [c]);
    await new Promise((x) => setTimeout(x, 6_000)); // past a pump tick
    assert.equal(await statusOf(c), "idle", "still waiting");
    assert.equal((await queue()).waiting.length, 1);
  });

  test("S2: Start now starts it over the limit", async () => {
    const w = (await queue()).waiting[0];
    await api(app!.page, "resource_queue_start_now", { chatId: c, submissionId: w.submissionId });
    await openChat(app!.page, c);
    await waitText(app!.page, "You said:");
    await waitText(app!.page, "slot message for C");
    assert.equal((await queue()).waiting.length, 0);
  });

  test("S3: Cancel wait ends the wait and the chat stays idle", async () => {
    const r = await outsideSend(d, "to be cancelled");
    assert.equal(r.waitingForSlot, true);
    const w = (await queue()).waiting.find((x: Json) => x.chatId === d);
    await api(app!.page, "resource_queue_cancel", { submissionId: w.submissionId });
    assert.equal((await queue()).waiting.length, 0);
    await new Promise((x) => setTimeout(x, 6_000));
    assert.equal(await statusOf(d), "idle", "never started");
  });

  test("S4: when a slot frees, the waits start by themselves, in order", async () => {
    // e's turn never ends, so it keeps the slot it gets and f has to keep waiting.
    const r1 = await outsideSend(e, "[E8-run] first in line");
    const r2 = await outsideSend(f, "second in line");
    assert.deepEqual([r1.position, r2.position], [1, 2]);
    await api(app!.page, "chat_stop", { chatId: holders[0], continueQueue: false });
    // One slot freed: the first wait starts, the second keeps waiting.
    await until("the first wait to start", async () => ((await queue()).waiting.length === 1 ? true : undefined), 20_000);
    assert.deepEqual((await queue()).waiting.map((w: Json) => w.chatId), [f], "no overtaking");
    await openChat(app!.page, e);
    await waitText(app!.page, "first in line");
  });

  test("S5: the user's own message never waits", async () => {
    // Both slots are held again (the second holder and e): a typed message still starts.
    assert.equal((await queue()).running.length >= 2, true);
    const g = await newChat(app!.page, work);
    await send(app!.page, "typed by the user");
    await waitText(app!.page, "You said:");
    assert.ok(!(await queue()).waiting.some((w: Json) => w.chatId === g), "never queued");
  });
});
