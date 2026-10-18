// The Orchestrator UI end to end in the real debug app with the fake claude: the Orchestrator
// switch in the composer and Settings → Orchestrator. With the master switch off (the default) no
// chat shows the switch; turning the workflow on in the pane and saving makes new chats start in
// the state "Orchestrator for new chats" names, the switch shows it, and a click changes it through
// the host (the transcript line included).
//
// Run: `npm run build:debug` at the root, then `npm run e2e -w checks`.

import assert from "node:assert/strict";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { after, before, describe, test } from "node:test";
import { api, dispose, FAKE_CLAUDE, launchApp, newChat, scratchDir, shot, tid, type App } from "./harness.ts";

const FIX = path.join(path.dirname(fileURLToPath(import.meta.url)), "fixtures");
const OUT = process.env.HELM_E2E_OUT ?? scratchDir("helm-e2e-out-");
const profile = scratchDir("helm-e2e-profile-");
const work = scratchDir("helm-e2e-work-");
const claudeDir = scratchDir("helm-e2e-claude-");
let app: App | undefined;
const page = () => app!.page;

type View = { mode: string; setBy: string; inEffect: boolean; unavailable?: string };

const SWITCH = tid("orchestrator-switch");

async function openPane(): Promise<void> {
  if ((await page().locator(tid("orchestrator-pane")).count()) > 0) return;
  await page().keyboard.press("Control+Comma");
  await page().getByRole("tab", { name: "Orchestrator", exact: true }).click();
  await page().waitForSelector(tid("orchestrator-pane"), { timeout: 20_000 });
}

async function save(): Promise<void> {
  await page().click(tid("orch-save"));
  await page().waitForSelector(tid("orch-saved"), { timeout: 15_000 });
}

async function closeSettings(): Promise<void> {
  await page().keyboard.press("Escape");
  await page().waitForSelector(tid("orchestrator-pane"), { state: "detached", timeout: 10_000 });
}

const view = async (chatId: string): Promise<View> => (await api(page(), "workflow_chat_mode_get", { chatId })) as View;

describe("the Orchestrator switch and Settings → Orchestrator", { timeout: 10 * 60_000 }, () => {
  before(async () => {
    app = await launchApp(profile, {
      env: {
        HELM_TEST_CLAUDE: JSON.stringify({ command: process.execPath, args: [FAKE_CLAUDE, path.join(FIX, "w4-usage.json")] }),
        CLAUDE_CONFIG_DIR: claudeDir,
      },
    });
  });
  after(async () => {
    await dispose(app);
  });

  test("O1: the workflow is off by default: no switch in the composer, and the host says why", async () => {
    try {
      const chat = await newChat(page(), work);
      await page().waitForSelector(tid("composer"), { timeout: 20_000 });
      assert.equal(await page().locator(SWITCH).count(), 0);
      const v = await view(chat);
      assert.equal(v.inEffect, false);
      assert.ok(v.unavailable, "the host names why the switch is unavailable");
    } finally {
      await shot(page(), OUT, "w6-orch-off");
    }
  });

  test("O2: turning the workflow on in the pane (Save) shows the switch, on, for a new chat", async () => {
    try {
      await openPane();
      assert.equal(await page().getAttribute(`${tid("orch-newchats")} [data-value="always"]`, "aria-checked"), "true", "Always is the default for new chats");
      assert.equal(await page().locator(`${tid("orch-save")}:disabled`).count(), 1, "nothing to save yet");
      await page().check(`${tid("orch-enabled")}`);
      await page().locator(`${tid("orch-save")}:enabled`).waitFor();
      await save();
      const s = (await api(page(), "workflow_settings_get", {})) as { enabled: boolean; newChats: string };
      assert.deepEqual([s.enabled, s.newChats], [true, "always"]);
      await closeSettings();

      const chat = await newChat(page(), work);
      await page().waitForSelector(SWITCH, { timeout: 20_000 });
      assert.equal(await page().getAttribute(SWITCH, "data-mode"), "on");
      assert.equal(await page().getAttribute(SWITCH, "data-look"), "on-user");
      assert.match((await page().getAttribute(SWITCH, "title")) ?? "", /Orchestrator: on for this chat\. Click to turn it off\./);
      const v = await view(chat);
      assert.deepEqual([v.mode, v.setBy, v.inEffect], ["on", "default", true]);
    } finally {
      await shot(page(), OUT, "w6-orch-on");
    }
  });

  test("O3: a click turns it off through the host, and again on", async () => {
    try {
      const chat = await page().getAttribute(tid("chat-view"), "data-chat-id");
      assert.ok(chat);
      await page().click(SWITCH);
      await page().waitForSelector(`${SWITCH}[data-mode="off"]`, { timeout: 15_000 });
      assert.equal(await page().getAttribute(SWITCH, "data-look"), "off");
      assert.match((await page().getAttribute(SWITCH, "title")) ?? "", /Orchestrator: off for this chat\. Click to turn it on; it applies from the next turn\./);
      assert.deepEqual(await view(chat).then((v) => [v.mode, v.setBy]), ["off", "user"]);
      await page().click(SWITCH);
      await page().waitForSelector(`${SWITCH}[data-mode="on"]`, { timeout: 15_000 });
      assert.deepEqual(await view(chat).then((v) => [v.mode, v.setBy]), ["on", "user"]);
    } finally {
      await shot(page(), OUT, "w6-orch-toggle");
    }
  });

  test("O4: Orchestrator for new chats: Never starts a chat off, Auto starts it in Auto; used chats keep theirs", async () => {
    try {
      const used = await page().getAttribute(tid("chat-view"), "data-chat-id");
      assert.ok(used);
      await openPane();
      await page().click(`${tid("orch-newchats")} [data-value="never"]`);
      await save();
      await closeSettings();
      const never = await newChat(page(), work);
      await page().waitForSelector(`${SWITCH}[data-mode="off"]`, { timeout: 20_000 });
      assert.deepEqual(await view(never).then((v) => [v.mode, v.setBy]), ["off", "default"]);

      await openPane();
      await page().click(`${tid("orch-newchats")} [data-value="auto"]`);
      await save();
      await closeSettings();
      const auto = await newChat(page(), work);
      await page().waitForSelector(`${SWITCH}[data-look="auto"]`, { timeout: 20_000 });
      assert.match((await page().getAttribute(SWITCH, "title")) ?? "", /The agent may turn it on for a task/);
      assert.deepEqual(await view(auto).then((v) => [v.mode, v.setBy]), ["auto", "default"]);
      assert.equal((await view(used)).mode, "on", "a chat already used keeps its state");
    } finally {
      await shot(page(), OUT, "w6-orch-newchats");
    }
  });

  test("O5: Move fast is a user-only setting in the pane and survives Save", async () => {
    try {
      await openPane();
      await page().check(tid("orch-movefast"));
      await save();
      const s = (await api(page(), "workflow_settings_get", {})) as { moveFast: boolean };
      assert.equal(s.moveFast, true);
      await closeSettings();
    } finally {
      await shot(page(), OUT, "w6-orch-movefast");
    }
  });

  test("O6: no CSP violation across the switch and the pane", async () => {
    assert.deepEqual(await app!.cspViolations(), []);
  });
});
