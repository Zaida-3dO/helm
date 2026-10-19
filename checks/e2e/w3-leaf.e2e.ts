// Top-bar buttons, the composer chips and Mod+K search end to end with the fake claude in the
// real debug app. Every send must leave a typed draft alone.
//
// Run: `npm run build:debug` at the root, then `npm run e2e -w checks`.

import assert from "node:assert/strict";
import { after, before, describe, test } from "node:test";
import { dispose, launchApp, newChat, scratchDir, send, shot, tid, typeMessage, waitRowText, waitStatus, type App } from "./harness.ts";

const OUT = process.env.HELM_E2E_OUT ?? scratchDir("helm-e2e-out-");
const profile = scratchDir("helm-e2e-profile-");
let app: App | undefined;
const page = () => app!.page;

describe("top-bar buttons, chips and search", { timeout: 10 * 60_000 }, () => {
  before(async () => {
    app = await launchApp(profile);
  });
  after(async () => {
    await dispose(app);
  });

  test("L1: top-bar buttons, chips and search, with a typed draft left alone", async () => {
    try {
      await newChat(page(), scratchDir("helm-e2e-chat-"));

      // The four visible top-bar buttons; the launcher is hidden and Git sync sits in the menu.
      const top = await page().$$eval(tid("action-button"), (els) => els.map((e) => e.getAttribute("data-action")));
      assert.deepEqual(top, ["update", "needs-me", "ship-it", "close-session"]);
      await page().click(tid("action-overflow"));
      assert.equal(await page().locator(tid("action-menu")).locator('[role="menuitem"][data-item="git-sync"]').count(), 1, "Git sync is in the menu");
      await page().keyboard.press("Escape");

      // A reply that ends in a question offers Yes and No (Continue only follows a stopped turn).
      await send(page(), "[LEAF] shall we carry on?");
      await waitStatus(page(), "idle");
      await page().waitForSelector(tid("quick-reply"), { timeout: 20_000 });
      const chips = await page().$$eval(tid("quick-reply"), (els) => els.map((e) => e.getAttribute("data-reply")));
      assert.deepEqual(chips, ["yes", "no"]);

      // A chip and a top-bar button each send their text and leave the typed draft alone.
      await typeMessage(page(), "keep this draft");
      await page().click(tid("quick-reply", '[data-reply="yes"]'));
      await waitRowText(page(), "yes");
      await waitStatus(page(), "idle");
      assert.match(await page().locator(tid("composer")).innerText(), /keep this draft/);
      await page().click(tid("action-button", '[data-action="update"]'));
      await waitRowText(page(), "brief plain-language status");
      await waitStatus(page(), "idle");
      assert.match(await page().locator(tid("composer")).innerText(), /keep this draft/);

      // Mod+K finds the chat by its text and Enter opens it.
      await page().keyboard.press("Control+K");
      await page().waitForSelector(tid("search-dialog"), { timeout: 10_000 });
      await page().fill(tid("search-input"), "carry on");
      await page().waitForSelector(tid("search-hit", '[data-group="chats"]'), { timeout: 10_000 });
      await page().keyboard.press("Enter");
      await page().waitForSelector(tid("search-dialog"), { state: "detached", timeout: 10_000 });
      assert.deepEqual(await app!.cspViolations(), []);
      await shot(page(), OUT, "L1");
    } catch (e) {
      await shot(page(), OUT, "L1-FAILED");
      throw new Error(`${(e as Error).message}\n--- app log tail ---\n${app?.log().slice(-3000)}`);
    }
  });
});
