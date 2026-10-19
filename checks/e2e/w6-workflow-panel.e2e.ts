// The workflow panel end to end in the real debug app with the fake claude
// (fixtures/w6-workflow.json): the workflow panel is live and its buttons are the user's own steps. The orchestrator adopts a
// plan through `app_workflow`; the panel appears from the `workflow_run` event, shows the plan
// sign-off the user owes, and Sign off plan, Mark visual, Park, Reopen and End task each go through
// the host and come back on the panel. Merge, Decline, Retry land and Ready for review need a real
// GitHub PR, so the land step's scripted-gh tests cover them (`cargo test -p helm-workflow`).
//
// Run: `npm run build:debug` at the root, then `npm run e2e -w checks`.

import assert from "node:assert/strict";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { after, before, describe, test } from "node:test";
import { api, dispose, FAKE_CLAUDE, launchApp, newChat, scratchDir, send, shot, tid, waitStatus, type App } from "./harness.ts";

const FIXTURE = path.join(path.dirname(fileURLToPath(import.meta.url)), "fixtures", "w6-workflow.json");
const OUT = process.env.HELM_E2E_OUT ?? scratchDir("helm-e2e-out-");
const profile = scratchDir("helm-e2e-profile-");
const work = scratchDir("helm-e2e-work-");
const claudeDir = scratchDir("helm-e2e-claude-");
let app: App | undefined;
const page = () => app!.page;
type Json = Record<string, unknown>;

const PANEL = tid("workflow-panel");

async function run(chatId: string): Promise<Json> {
  return (await api(page(), "workflow_run_get", { chatId })) as Json;
}

/** Open the task's ⋯ menu and choose one item. */
async function menu(item: string): Promise<void> {
  await page().click(tid("workflow-menu"));
  await page().waitForSelector(tid("workflow-task-menu"), { timeout: 10_000 });
  await page().click(`${tid("workflow-task-menu")} [data-item="${item}"]`);
  await page().waitForSelector(tid("workflow-task-menu"), { state: "detached", timeout: 10_000 });
}

describe("the workflow panel's live read and the user's buttons", { timeout: 10 * 60_000 }, () => {
  let chat = "";
  before(async () => {
    app = await launchApp(profile, {
      env: {
        HELM_TEST_CLAUDE: JSON.stringify({ command: process.execPath, args: [FAKE_CLAUDE, FIXTURE] }),
        CLAUDE_CONFIG_DIR: claudeDir,
      },
    });
  });
  after(async () => {
    await dispose(app);
  });

  test("P1: the orchestrator's plan shows on the panel, and the user signs it off", async () => {
    try {
      // The user signs every plan off; plan review is off so the plan waits only for them.
      await api(page(), "workflow_settings_save", {
        settings: { enabled: true, newChats: "always", planSignOff: "me", stages: { planReview: { use: "off" } } },
      });
      chat = await newChat(page(), work);
      assert.equal(((await run(chat)) as Json).task, null, "no task before the agent starts one");
      await send(page(), "[WF-PLAN] plan the login page");
      await waitStatus(page(), "idle", 60_000);
      await page().waitForSelector(`${PANEL}[data-status="active"]`, { timeout: 20_000 });
      assert.match(await page().locator(tid("workflow-needs")).innerText(), /plan sign-off/);
      const unsigned = (await run(chat)).plan as Json;
      assert.equal(unsigned.signedOff, false);
      assert.match(String(unsigned.missing), /sign-off/);
      await page().click(tid("workflow-signoff"));
      await page().waitForSelector(tid("workflow-signoff"), { state: "detached", timeout: 15_000 });
      const signed = (await run(chat)).plan as Json;
      assert.equal(signed.signedOff, true);
      assert.equal(signed.missing, null, "nothing stands between the plan and a build now");
      await page().waitForSelector(`${PANEL}[data-needs="0"]`, { timeout: 10_000 });
      await shot(page(), OUT, "w6-panel-signed-off");
    } catch (e) {
      await shot(page(), OUT, "w6-panel-p1-fail");
      throw e;
    }
  });

  test("P2: the task menu marks it visual, parks, reopens and ends it", async () => {
    try {
      await menu("visual");
      await page().waitForFunction((sel) => document.querySelector(sel)?.textContent?.includes("· visual"), tid("workflow-toggle"), { timeout: 10_000 });
      assert.equal(((await run(chat)).task as Json).visual, true);
      await menu("park");
      await page().waitForSelector(`${PANEL}[data-status="parked"]`, { timeout: 10_000 });
      await menu("reopen");
      await page().waitForSelector(`${PANEL}[data-status="active"]`, { timeout: 10_000 });
      const t = (await run(chat)).task as Json;
      assert.equal(t.attempt, 2, "a Reopen starts the next attempt");
      assert.match(await page().locator(tid("workflow-toggle")).innerText(), /attempt 2/);
      await menu("end");
      await page().waitForSelector(PANEL, { state: "detached", timeout: 10_000 });
      assert.equal((await run(chat)).task, null, "an ended task is no longer open");
      await shot(page(), OUT, "w6-panel-ended");
    } catch (e) {
      await shot(page(), OUT, "w6-panel-p2-fail");
      throw e;
    }
  });

  test("P3: the host refuses a task action on a task that does not exist", async () => {
    const e = await page().evaluate(async (c) => {
      try {
        await (window as unknown as { __TAURI_INTERNALS__: { invoke: (c: string, p: unknown) => Promise<unknown> } }).__TAURI_INTERNALS__.invoke("api", {
          op: "workflow_task_action",
          args: { runId: c, action: "reopen" },
        });
        return null;
      } catch (err) {
        return typeof err === "string" ? err : JSON.stringify(err);
      }
    }, "no-such-run");
    assert.match(String(e), /No such task/);
  });
});
