// The question gate end to end in the real debug app with the fake claude
// (fixtures/gatekeeper.json, which also scripts the gatekeeper helper's answers):
//   - decided: a question card is answered for the user, the transcript gets an "Answered for
//     you" row, the Needs attention dialog lists it under "Decided for you", and Change answer
//     lands a correction in the chat;
//   - escalated: the card stays with the gate's brief and counts in attention; the user answers
//     it, and Settings → Gatekeeper → History shows both records;
//   - hang: a helper that never answers lets the card through as a normal one once the helper's
//     time runs out. The run shortens the hold with HELM_TEST_GATE_HOLD_MS, so this check takes
//     seconds rather than the normal minute and a half; and it turns the gate on with
//     HELM_TEST_GATE, since a run on the fake claude has it off by default.
//
// The working mode end to end (the second scenario set): the top-bar chip reads "check in" by
// default; switching it to run puts the run text in the next chat's launch, a turn that ends by
// asking is held with a quiet checking line and answered with an App nudge instead of a finished
// notice, the nudge shows up under "Decided for you" where Change answer still works, a real
// blocker ends on a Blocker row that counts in attention, and a chat that keeps asking ends on a
// "Stopped to ask" row after three nudges.
//
// Run: `npm run build:debug` at the root, then `npm run e2e -w checks`.

import assert from "node:assert/strict";
import fs from "node:fs";
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
        HELM_TEST_GATE: "on",
        HELM_TEST_GATE_HOLD_MS: "9000",
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
      // The helper's own time runs out well inside the shortened hold; the card is then a
      // normal one.
      await page().waitForSelector(tid("gate-line", '[data-state="failed"]'), { timeout: 20_000 });
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

const profile2 = scratchDir("helm-e2e-profile-");
const work2 = scratchDir("helm-e2e-work-");
const claudeDir2 = scratchDir("helm-e2e-claude-");

describe("The working mode: the chip, the nudged stop, the counted blocker and the asked limit", { timeout: 12 * 60_000 }, () => {
  let app2: App | undefined;
  const page2 = () => app2!.page;

  before(async () => {
    app2 = await launchApp(profile2, {
      env: {
        HELM_TEST_CLAUDE: JSON.stringify({ command: process.execPath, args: [FAKE_CLAUDE, path.join(FIX, "gatekeeper.json")] }),
        CLAUDE_CONFIG_DIR: claudeDir2,
        FAKE_CLAUDE_LOG: path.join(profile2, "fake-claude.log"),
      },
    });
  });
  after(async () => {
    await dispose(app2);
  });

  const gateRows2 = async (filter: string): Promise<Json[]> =>
    ((await api(page2(), "gate_list", { filter })) as { rows: Json[] }).rows;
  const needsYou2 = async (chatId: string): Promise<boolean> =>
    ((await api(page2(), "attention_get", {})) as { needsYou: { chatId: string }[] }).needsYou.some((r) => r.chatId === chatId);
  // The working mode has no top-bar control any more (it lives in Settings → Chat & agents);
  // the test switches it through the same op the settings page uses.
  const modeView = async () => (await api(page2(), "workmode_get", {})) as { app: string; effective: string };
  const sendText2 = async (text: string): Promise<void> => {
    await typeMessage(page2(), text);
    await page2().locator(tid("send")).dispatchEvent("click");
  };

  test("W1: run mode turns an asked stop into a nudge, records it, and Change answer reaches the chat", async () => {
    try {
      // The mode is check-in by default; switch the app to run through the settings' own op.
      assert.equal((await modeView()).app, "check_in", "the app starts on check in");
      await api(page2(), "workmode_set", { scope: "app", mode: "run" });
      await until("the app reads run", async () => (await modeView()).app === "run");

      const chat = await newChat(page2(), work2);
      // The checking line lives only while the stop is being judged, which is a moment, and
      // a backgrounded window renders too slowly for a polling selector to catch a mount that
      // quick. So watch the DOM itself: the observer sees the line the instant it appears.
      const lineSeen = page2().evaluate(
        (id) =>
          new Promise<boolean>((resolve) => {
            const hit = () => document.querySelector(`[data-testid="${id}"]`);
            if (hit()) return resolve(true);
            const mo = new MutationObserver(() => {
              if (hit()) {
                mo.disconnect();
                resolve(true);
              }
            });
            mo.observe(document.body, { subtree: true, childList: true });
            setTimeout(() => {
              mo.disconnect();
              resolve(false);
            }, 90_000);
          }),
        "stop-check-line",
      );
      await sendText2("stop flow one");

      // Every chat is launched with the mode: the run text is in this chat's launch record.
      const launchFile = path.join(profile2, "workers", chat, "launch.json");
      const launched = await until("the launch record", () => (fs.existsSync(launchFile) ? fs.readFileSync(launchFile, "utf8") : undefined));
      assert.match(launched, /Working mode: run to completion/, "the launch says the mode");

      // The turn ends by asking: the quiet checking line showed (no finished notice is given).
      await waitText(page2(), "Shall I go ahead with the next step?", 60_000);
      assert.ok(await lineSeen, "the checking line shows while the stop is judged");

      // The answer is a nudge from Helm, and the chat carries on with the default.
      await waitText(page2(), "Carry on without asking", 60_000);
      await waitText(page2(), "All finished. Done.", 60_000);
      assert.equal(await needsYou2(chat), false, "a nudged stop is not counted against the chat");
      assert.equal(await page2().locator(tid("stop-row")).count(), 0, "a nudge writes no stop row");

      const decided = await until("the decided stop record", async () => (await gateRows2("review")).find((r) => r.chatId === chat));
      assert.equal(decided.verdict, "decided");
      assert.equal(decided.channel, "stop");
      assert.equal(decided.choice, "Take the next step");
      assert.match(decided.question, /next step/);

      // It joins "Decided for you", and Change answer lands a correction in the chat.
      await page2().click(tid("nav-attention"));
      const row = page2().locator(tid("gate-decided-row", `[data-ask-id="${decided.id}"]`));
      await row.waitFor({ timeout: 20_000 });
      await row.locator(tid("gate-change-open")).click();
      await row.locator(tid("gate-change-input")).fill("Only after I have reviewed it");
      await row.locator(tid("gate-change-send")).click();
      await row.waitFor({ state: "detached", timeout: 20_000 });
      await page2().keyboard.press("Escape");
      await waitText(page2(), "You said: Correction", 30_000);
    } finally {
      await shot(page2(), OUT, "workmode-nudged");
    }
  });

  test("W2: a real blocker ends on a Blocker row and counts in attention", async () => {
    try {
      const chat = await newChat(page2(), work2);
      await sendText2("stop flow blocker");
      await waitText(page2(), "sign in to the package registry", 60_000);
      await page2().waitForSelector(`${tid("stop-row")}[data-class="blocker"]`, { timeout: 30_000 });
      await until("the stop counted in attention", () => needsYou2(chat), 20_000);
      const asked = await until("the escalated stop record", async () => (await gateRows2("asked")).find((r) => r.chatId === chat));
      assert.equal(asked.verdict, "escalated");
      assert.equal(asked.channel, "stop");
      assert.equal(asked.stopClass, "blocker");
    } finally {
      await shot(page2(), OUT, "workmode-blocker");
    }
  });

  test("W3: a chat that keeps asking stops on its own after three nudges", async () => {
    try {
      const chat = await newChat(page2(), work2);
      await sendText2("loop chat");
      await page2().waitForSelector(`${tid("stop-row")}[data-class="asked"]`, { timeout: 120_000 });
      await waitText(page2(), "Still asking after 3 nudges", 10_000);
      assert.ok(chat, "the loop ran in one chat");
    } finally {
      await shot(page2(), OUT, "workmode-asked");
    }
  });
});
