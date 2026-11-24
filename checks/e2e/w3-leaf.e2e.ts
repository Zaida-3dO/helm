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

      // The bar starts empty: Helm no longer seeds any visible button — the common requests
      // (Report progress, Ship it and the rest) are skills now, named in the composer.
      assert.equal(await page().locator(tid("action-button")).count(), 0, "no seeded top-bar buttons");
      assert.equal(await page().locator(tid("action-overflow")).count(), 0, "and no overflow trigger");

      // A button made in Settings still lands in the bar: add one and read it back.
      await page().keyboard.press("Control+Comma");
      await page().waitForSelector(tid("settings"), { timeout: 10_000 });
      await page().click('.settings-group-tab[data-group="Chat & agents"]');
      await page().click('.settings-tab[data-tab="actions"]');
      await page().waitForSelector(tid("action-add"), { timeout: 10_000 });
      await page().click(tid("action-add"));
      const btnId = (await page().locator(tid("action-edit")).last().getAttribute("data-action"))!;
      assert.ok(btnId && !btnId.startsWith("seed-"), "the new button is a user button");
      await page().fill(`#label-${btnId}`, "Status please");
      await page().fill(`#text-${btnId}`, "Please give me a brief plain-language status: what is finished, what is under way, what is stuck, and what you need from me.");
      await page().click(tid("actions-save"));
      await page().waitForSelector(`${tid("settings-pane")} [role="status"]:has-text("Saved.")`, { timeout: 10_000 });
      await page().keyboard.press("Escape");
      // A bar too narrow for it folds it into one "Actions" menu, so use whichever form shows.
      const folded = (await page().locator(tid("action-button", `[data-action="${btnId}"]`)).count()) === 0;
      if (folded) {
        await page().click(tid("action-overflow"));
        assert.equal(await page().locator(tid("action-menu")).locator(`[role="menuitem"][data-item="${btnId}"]`).count(), 1, "the user button is in the menu");
        await page().keyboard.press("Escape");
      }

      // A reply that ends in a question offers Yes and No (Continue only follows a stopped turn).
      await send(page(), "[LEAF] shall we carry on?");
      await waitStatus(page(), "idle");
      await page().waitForSelector(tid("quick-reply"), { timeout: 20_000 });
      const chips = await page().$$eval(tid("quick-reply"), (els) => els.map((e) => e.getAttribute("data-reply")));
      assert.deepEqual(chips, ["yes", "no"]);

      // The tooltip and the accessible name reach the DOM, and start with the visible label.
      for (const id of ["yes", "no"]) {
        const chip = page().locator(tid("quick-reply", `[data-reply="${id}"]`));
        const [label, title, name] = await chip.evaluate((el) => [el.textContent ?? "", el.getAttribute("title") ?? "", el.getAttribute("aria-label") ?? ""]);
        assert.ok(title.startsWith(label) && title.length > label.length, `${id} has a tooltip that explains it: ${title}`);
        assert.equal(name, title, `${id}'s accessible name is the tooltip text`);
      }

      // A chip and a top-bar button each send their text and leave the typed draft alone.
      await typeMessage(page(), "keep this draft");
      await page().click(tid("quick-reply", '[data-reply="yes"]'));
      await waitRowText(page(), "yes");
      await waitStatus(page(), "idle");
      assert.match(await page().locator(tid("composer")).innerText(), /keep this draft/);
      if (folded) {
        await page().click(tid("action-overflow"));
        await page().click(`${tid("action-menu")} [role="menuitem"][data-item="${btnId}"]`);
      } else {
        await page().click(tid("action-button", `[data-action="${btnId}"]`));
      }
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

  test("L2: after Stop the Continue chip appears with its tooltip and accessible name", async () => {
    try {
      await newChat(page(), scratchDir("helm-e2e-chat-"));
      await send(page(), "[E7] wait a while");
      await page().waitForSelector(tid("tool-card", '[data-status="running"]'), { timeout: 20_000 });
      await page().click(tid("stop"));
      await waitStatus(page(), "stopped", 30_000);
      await page().waitForSelector(tid("quick-reply", '[data-reply="continue"]'), { timeout: 20_000 });
      const chip = page().locator(tid("quick-reply", '[data-reply="continue"]'));
      const [label, title, name] = await chip.evaluate((el) => [el.textContent ?? "", el.getAttribute("title") ?? "", el.getAttribute("aria-label") ?? ""]);
      assert.equal(label, "Continue");
      assert.match(title, /^Continue: sends 'continue'/);
      assert.equal(name, title, "the accessible name is the tooltip text");
    } catch (e) {
      await shot(page(), OUT, "L2-FAILED");
      throw new Error(`${(e as Error).message}\n--- app log tail ---\n${app?.log().slice(-3000)}`);
    }
  });
});
