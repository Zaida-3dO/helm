// The question gate end to end in the real debug app with the fake claude
// (fixtures/gatekeeper.json, which also scripts the gatekeeper helper's answers):
//   - decided: a question card is answered for the user, the transcript gets an "Answered for
//     you" row, the Needs attention dialog lists it under "Decided for you", and Change answer
//     lands a correction in the chat;
//   - escalated: the card stays with the gate's brief and counts in attention; the user answers
//     it, and Settings → Gatekeeper → History shows both records;
//   - hang: a helper that never answers lets the card through as a normal one once the helper's
//     time runs out (about a minute: the app has no shorter setting for it).
//
// Run: `npm run build:debug` at the root, then `npm run e2e -w checks`.

import assert from "node:assert/strict";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { after, before, describe, test } from "node:test";
import { api, dispose, FAKE_CLAUDE, launchApp, newChat, scratchDir, shot, tid, typeMessage, waitText, type App } from "./harness.ts";

const FIX = path.join(path.dirname(fileURLToPath(import.meta.url)), "fixtures");
const OUT = process.env.HELM_E2E_OUT ?? scratchDir("helm-e2e-out-");
const profile = scratchDir("helm-e2e-profile-");
const work = scratchDir("helm-e2e-work-");
const claudeDir = scratchDir("helm-e2e-claude-");
let app: App | undefined;
const page = () => app!.page;
type Json = Record<string, any>;

async function until<T>(what: string, f: () => T | undefined | Promise<T | undefined>, ms = 30_000): Promise<T> {
  const end = Date.now() + ms;
  for (;;) {
    const v = await f();
    if (v !== undefined && v !== null && v !== false) return v as T;
    if (Date.now() > end) throw new Error(`timed out waiting for ${what}`);
    await new Promise((r) => setTimeout(r, 250));
  }
}

async function sendText(text: string): Promise<void> {
  await typeMessage(page(), text);
  await page().locator(tid("send")).dispatchEvent("click");
}

const gateRows = async (filter: string): Promise<Json[]> => ((await api(page(), "gate_list", { filter })) as { rows: Json[] }).rows;
const needsYou = async (chatId: string): Promise<boolean> =>
  ((await api(page(), "attention_get", {})) as { needsYou: { chatId: string }[] }).needsYou.some((r) => r.chatId === chatId);
const questionCard = () => page().locator(tid("interaction-card", '[data-attention="question"]'));

describe("The question gate: decided, escalated and timed-out cards", { timeout: 10 * 60_000 }, () => {
  before(async () => {
    app = await launchApp(profile, {
      env: {
        HELM_TEST_CLAUDE: JSON.stringify({ command: process.execPath, args: [FAKE_CLAUDE, path.join(FIX, "gatekeeper.json")] }),
        CLAUDE_CONFIG_DIR: claudeDir,
        FAKE_CLAUDE_LOG: path.join(profile, "fake-claude.log"),
      },
    });
    const s = (await api(page(), "gate_settings_get", {})) as Json;
    assert.equal(s.enabled, true, "the gate is on by default");
  });
  after(async () => {
    await dispose(app);
  });

  test("K1: a safe question is answered for the user, reviewed, and its answer changed", async () => {
    try {
      const chat = await newChat(page(), work);
      await sendText("gate flow one");
      // The gate answers the card; the chat carries on and the transcript says who answered.
      await waitText(page(), "Renamed, carrying on.", 60_000);
      await waitText(page(), "Rename it", 10_000);
      await page().waitForSelector(tid("gate-review-link"), { timeout: 10_000 });
      assert.equal(await questionCard().count(), 0, "no card is left for the user");
      const decided = await until("a decided record", async () => (await gateRows("review")).find((r) => r.chatId === chat));
      assert.equal(decided.verdict, "decided");
      assert.equal(decided.choice, "Rename it");
      assert.equal(await needsYou(chat), false, "a decided card never counts in attention");

      // Needs attention → Decided for you → Change answer.
      await page().click(tid("nav-attention"));
      const row = page().locator(tid("gate-decided-row", `[data-ask-id="${decided.id}"]`));
      await row.waitFor({ timeout: 20_000 });
      await row.locator(tid("gate-change-open")).click();
      await row.locator(tid("gate-change-input")).fill("Keep the old name");
      await row.locator(tid("gate-change-send")).click();
      await row.waitFor({ state: "detached", timeout: 20_000 });
      await page().keyboard.press("Escape");
      await waitText(page(), "You said: Correction from the user", 30_000);
      await waitText(page(), "Keep the old name", 10_000);
      const vetoed = await until("the vetoed record", async () => (await gateRows("decided")).find((r) => r.id === decided.id && r.state === "vetoed"));
      assert.equal(vetoed.answer, "Keep the old name");
    } finally {
      await shot(page(), OUT, "gatekeeper-decided");
    }
  });

  test("K2: an unclear, hard-to-undo question goes to the user with a brief, and History shows both", async () => {
    try {
      const chat = await newChat(page(), work);
      await sendText("gate flow two");
      await page().waitForSelector(tid("gate-brief"), { timeout: 60_000 });
      await page().waitForSelector(tid("gate-brief-option", '[data-label="Keep them"]'), { timeout: 5_000 });
      assert.equal(await questionCard().count(), 1, "the card stays");
      await until("the escalated card counted in attention", () => needsYou(chat), 20_000);
      const asked = await until("an escalated record", async () => (await gateRows("asked")).find((r) => r.chatId === chat));
      assert.equal(asked.verdict, "escalated");

      await page().click(tid("question-button", '[data-option="Keep them"]'));
      await waitText(page(), "Kept the backups.", 30_000);
      await until("attention cleared", async () => !(await needsYou(chat)), 20_000);

      // Settings → Gatekeeper → History lists the decided one and the escalated one.
      await page().keyboard.press("Control+Comma");
      await page().getByRole("tab", { name: "Gatekeeper", exact: true }).click();
      await page().waitForSelector(tid("settings-gatekeeper"), { timeout: 20_000 });
      await page().waitForSelector(tid("gate-history-row", '[data-verdict="decided"]'), { timeout: 20_000 });
      await page().waitForSelector(tid("gate-history-row", `[data-ask-id="${asked.id}"][data-verdict="escalated"]`), { timeout: 20_000 });
      await page().keyboard.press("Escape");
    } finally {
      await shot(page(), OUT, "gatekeeper-escalated");
    }
  });

  test("K3: a helper that never answers lets the card through as a normal one", async () => {
    try {
      const chat = await newChat(page(), work);
      await sendText("gate flow three");
      // While the gate holds it, the card says so quietly and is not counted.
      await page().waitForSelector(tid("gate-line", '[data-state="checking"]'), { timeout: 30_000 });
      assert.equal(await needsYou(chat), false, "a held card is not counted");
      // The helper's own time runs out well inside the hold; the card is then a normal one.
      await page().waitForSelector(tid("gate-line", '[data-state="failed"]'), { timeout: 100_000 });
      await until("the released card counted in attention", () => needsYou(chat), 20_000);
      const failed = await until("a failed record", async () => (await gateRows("failed")).find((r) => r.chatId === chat));
      assert.equal(failed.verdict, "failed");
      await page().click(tid("question-button", '[data-option="Reports"]'));
      await waitText(page(), "Exported.", 30_000);
    } finally {
      await shot(page(), OUT, "gatekeeper-hang");
    }
  });
});
