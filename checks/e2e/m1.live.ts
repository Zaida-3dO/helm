// Live checks L1–L5: the real app with the real claude executable.
// They spend the signed-in plan's usage, so they are skipped unless HELM_LIVE_CLAUDE=1 and are run
// once per change: `HELM_LIVE_CLAUDE=1 npm run live -w checks` (after `npm run build:debug`).
// Haiku throughout; each chat works in a neutral temp folder; the app's environment has CLAUDECODE
// removed (and the worker strips it again). The default settings apply: asap, 30 s wait limit, redirect.

import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";
import { after, before, describe, test } from "node:test";
import {
  api,
  dispose,
  launchApp,
  newChat,
  openChat,
  quitGracefully,
  scratchDir,
  send,
  shot,
  tid,
  typeMessage,
  waitStatus,
  waitText,
  type App,
} from "./harness.ts";

const live = process.env.HELM_LIVE_CLAUDE === "1";
const OUT = process.env.HELM_E2E_OUT ?? scratchDir("helm-live-out-");
const profile = scratchDir("helm-live-profile-");
let app: App | undefined;
const page = () => app!.page;
const MIN = 60_000;

function workFolder(files: Record<string, string> = {}): string {
  const dir = scratchDir("helm-live-chat-");
  for (const [name, text] of Object.entries(files)) fs.writeFileSync(path.join(dir, name), text);
  return dir;
}

async function haikuChat(files: Record<string, string> = {}): Promise<string> {
  const id = await newChat(page(), workFolder(files));
  await page().selectOption(tid("model-picker"), { label: "Haiku" });
  await page().waitForTimeout(500);
  return id;
}

/**
 * The open chat's transcript as the host stored it. Read through the API rather than the DOM: the
 * UI paces streamed text and folds settled work, so the DOM can lag behind or hide what the host has.
 */
async function transcriptText(): Promise<string> {
  const chatId = await page().locator(tid("chat-view")).getAttribute("data-chat-id");
  const data = (await api(page(), "transcript_page", { chatId })) as { messages: { role: string; text: string; toolName: string | null }[] };
  return data.messages.map((m) => `[${m.role}${m.toolName ? ` ${m.toolName}` : ""}] ${m.text}`).join("\n");
}

/** HELM_LIVE_ONLY=L2,L4 reruns only those checks (each spends plan usage). */
const only = (process.env.HELM_LIVE_ONLY ?? "").split(",").filter(Boolean);
const want = (id: string) => only.length === 0 || only.includes(id);

async function record(name: string): Promise<void> {
  await shot(page(), OUT, name);
  fs.writeFileSync(path.join(OUT, `${name}.txt`), await transcriptText());
}

describe("live with the real claude", { skip: !live, timeout: 30 * MIN }, () => {
  before(async () => {
    app = await launchApp(profile, { fake: false, skipPermissions: true });
  });

  after(async () => {
    await dispose(app);
    process.stderr.write(`live: evidence in ${OUT}\n`);
  });

  test("L1: a real chat with streaming and a tool card", { skip: !want("L1") }, async () => {
    await haikuChat();
    await send(page(), "Use the Bash tool to run `echo hello-from-helm`, then reply with one short sentence saying what it printed.");
    await page().waitForSelector(tid("tool-card", '[data-tool="Bash"]'), { timeout: 3 * MIN });
    await page().waitForSelector(tid("transcript-row", '[data-role="assistant"][data-status="complete"]'), { timeout: 3 * MIN });
    await waitStatus(page(), "idle", 3 * MIN);
    await record("L1");
    assert.match(await transcriptText(), /\[toolResult Bash\] hello-from-helm/, "the tool ran and its output came back");
  });

  test("L2: a steer sent during the first tool call is Pending, then delivered, and the same turn acts on it", { skip: !want("L2") }, async () => {
    await haikuChat({ "a.txt": "alpha one\n", "b.txt": "bravo two three four\n", "c.txt": "charlie\n" });
    await send(page(), "Step 1: run `sleep 8 && ls` with the Bash tool. Step 2: read a.txt, b.txt and c.txt one at a time with the Read tool. Then summarise them in two sentences.");
    await page().waitForSelector(tid("tool-card", '[data-status="running"]'), { timeout: 3 * MIN });
    await send(page(), "Also: end your final answer with the exact word PINEAPPLE.");
    await page().waitForSelector(tid("queue-pending"), { timeout: 30_000 });
    await shot(page(), OUT, "L2-pending");
    await page().waitForFunction(() => !document.querySelector('[data-testid="queue-pending"]'), null, { timeout: 3 * MIN });
    await waitStatus(page(), "idle", 5 * MIN);
    await record("L2");
    const text = await transcriptText();
    const lines = text.split(/\n(?=\[)/);
    const steerAt = lines.findIndex((l) => l.startsWith("[user]") && l.includes("PINEAPPLE"));
    const firstTool = lines.findIndex((l) => l.startsWith("[toolResult"));
    const answeredAt = lines.findIndex((l, i) => i > steerAt && l.startsWith("[assistant]") && l.includes("PINEAPPLE"));
    assert.ok(firstTool !== -1 && firstTool < steerAt, "the steer was read after the first tool call returned");
    assert.ok(lines.slice(steerAt + 1).some((l) => l.startsWith("[toolResult")), "the same turn carried on with more tool calls after reading it");
    assert.ok(answeredAt > steerAt, "Claude's own reply acted on the steer");
  });

  test("L3: `npm test` in the foreground is denied once and re-run in the background; the chip appears", { skip: !want("L3") }, async () => {
    await haikuChat({ "package.json": JSON.stringify({ name: "live-l3", private: true, scripts: { test: "node -e \"setTimeout(()=>console.log('all 3 tests passed'),20000)\"" } }, null, 2) });
    await send(page(), "Run `npm test` in this folder with the Bash tool and tell me the result when it finishes.");
    await page().waitForSelector(tid("background-chip", '[data-count="1"]'), { timeout: 3 * MIN });
    await shot(page(), OUT, "L3-chip");
    await waitStatus(page(), "idle", 5 * MIN);
    await page().waitForFunction(() => [...document.querySelectorAll('[data-testid="transcript-row"]')].some((r) => /3 tests passed|passed/i.test(r.textContent ?? "")), null, { timeout: 3 * MIN }).catch(() => undefined);
    await record("L3");
    const text = await transcriptText();
    assert.match(text, /PreToolUse:Bash hook error: Helm: this looks like a long-running command/, "the foreground run was denied by the rule");
    assert.match(text, /running in background with ID/, "and re-run in the background");
  });

  test("L4: at the 30 s default wait limit a blocking call moves to the background and the steer is delivered, without ending the turn", { skip: !want("L4") }, async () => {
    await haikuChat();
    await send(page(), 'Run exactly this command with the Bash tool, in the foreground and without run_in_background: node -e "setTimeout(()=>{},90000)" . After it returns, reply with one short sentence.');
    await page().waitForSelector(tid("tool-card", '[data-status="running"]'), { timeout: 3 * MIN });
    await page().waitForTimeout(2_000);
    const t0 = Date.now();
    await send(page(), "Also: end your reply with the exact word KIWI.");
    await page().waitForSelector(tid("queue-pending"), { timeout: 30_000 });
    await page().waitForFunction(() => !document.querySelector('[data-testid="queue-pending"]'), null, { timeout: 2 * MIN });
    const tookMs = Date.now() - t0;
    await shot(page(), OUT, "L4-delivered");
    await waitStatus(page(), "idle", 5 * MIN);
    await record("L4");
    fs.writeFileSync(path.join(OUT, "L4-timing.txt"), `steer sent to delivered: ${tookMs} ms\n`);
    process.stderr.write(`live L4: steer delivered after ${tookMs} ms\n`);
    assert.ok(tookMs >= 25_000 && tookMs <= 45_000, `delivered at the wait limit (${tookMs} ms)`);
    const text = await transcriptText();
    assert.match(text, /manually backgrounded/, "the blocking call was moved to the background");
    assert.ok(text.split(/\n(?=\[)/).some((l) => l.startsWith("[assistant]") && l.includes("KIWI")), "Claude's reply acted on the steer");
    assert.equal(await page().locator(tid("transcript-row", '[data-status="interrupted"]')).count(), 0, "the turn did not end");
  });

  test("L5: quit during a turn and relaunch: history intact, the continuation runs, the user-stopped chat is not continued", { skip: !want("L5") }, async () => {
    const stoppedChat = await haikuChat();
    await send(page(), "Run `sleep 9` with the Bash tool, then run `sleep 9` again, then say done.");
    await page().waitForSelector(tid("tool-card", '[data-status="running"]'), { timeout: 3 * MIN });
    await page().click(tid("stop"));
    await waitStatus(page(), "stopped", 2 * MIN);
    const runningChat = await haikuChat();
    await send(page(), "Run `sleep 9` with the Bash tool, then run `sleep 9` again, then say the word MANGO.");
    await page().waitForSelector(tid("tool-card", '[data-status="running"]'), { timeout: 3 * MIN });
    await record("L5-before-quit");
    await quitGracefully(app!);

    app = await launchApp(profile, { fake: false });
    await openChat(page(), runningChat);
    await waitText(page(), "say the word MANGO", 60_000);
    await page().waitForFunction(() => [...document.querySelectorAll('[data-testid="transcript-row"]')].some((r) => r.getAttribute("data-role") === "user" || r.getAttribute("data-role") === "context"), null, { timeout: 60_000 });
    await waitStatus(page(), "idle", 5 * MIN);
    await record("L5-running-after");
    const after = await transcriptText();
    const afterLines = after.split(/\n(?=\[)/);
    const restartAt = afterLines.findIndex((l) => l.startsWith("[context]") && l.includes("restarted"));
    assert.ok(restartAt !== -1, "a restart context row");
    assert.equal(afterLines.filter((l) => l.startsWith("[context]") && l.includes("restarted")).length, 1, "continued once");
    assert.ok(afterLines.slice(restartAt).some((l) => l.startsWith("[assistant]") && l.trim().length > 15), "the continuation turn ran");
    assert.match(after, /\[user\] Run `sleep 9`/, "history intact");
    await openChat(page(), stoppedChat);
    await waitText(page(), "then say done", 60_000);
    await record("L5-stopped-after");
    const stoppedText = await transcriptText();
    assert.doesNotMatch(stoppedText, /restarted/, "the user-stopped chat is not continued");
    assert.match(stoppedText, /then say done/, "its history is intact");
    void typeMessage;
  });
});
