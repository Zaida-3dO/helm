// Orchestrator state end to end in the real debug app: Settings > Orchestrator is off by
// default and refuses unknown keys; with the master switch on, a chat follows "Orchestrator for
// new chats" (Auto here), the user's switch flips it with its transcript line, and Off on a chat
// with no running turn is accepted; the user's change actions refuse a task that does not exist.
// The engine, the guard, the Agent Standup outbox and the land step are covered by
// `cargo test -p helm-workflow` (scripted gh, fake board); this check proves the ops are wired.
//
// Run: `npm run build:debug` at the root, then `npm run e2e -w checks`.

import assert from "node:assert/strict";
import { after, before, describe, test } from "node:test";
import { api, dispose, launchApp, newChat, scratchDir, type App } from "./harness.ts";

const profile = scratchDir("helm-e2e-profile-");
const work = scratchDir("helm-e2e-work-");
let app: App | undefined;
const page = () => app!.page;
type Json = Record<string, unknown>;

/** The refusal an op answers, as text (caught in the page, so its shape survives). */
async function rejects(op: string, args: Json): Promise<string> {
  const r = await page().evaluate(
    async ([o, a]) => {
      try {
        await (window as unknown as { __TAURI_INTERNALS__: { invoke: (c: string, p: unknown) => Promise<unknown> } }).__TAURI_INTERNALS__.invoke("api", { op: o, args: a });
        return null;
      } catch (e) {
        return typeof e === "string" ? e : JSON.stringify(e);
      }
    },
    [op, args] as const,
  );
  if (r === null) throw new Error(`${op} should have been refused`);
  return r;
}

describe("Settings > Orchestrator and the per-chat Orchestrator state", { timeout: 5 * 60_000 }, () => {
  before(async () => {
    app = await launchApp(profile);
  });
  after(async () => {
    await dispose(app);
  });

  test("W1: the workflow is off by default and the settings refuse unknown keys", async () => {
    const s = (await api(page(), "workflow_settings_get", {})) as Json;
    assert.equal(s.enabled, false);
    assert.equal(s.newChats, "always");
    assert.equal(s.moveFast, false);
    assert.equal(((s.stages as Json).review as Json).maxRounds, 3);
    const e = await rejects("workflow_settings_save", { settings: { enabled: true, landing: { fromBoxes: true } } });
    assert.match(e, /landing\.fromBoxes/);
    assert.equal(((await api(page(), "workflow_settings_get", {})) as Json).enabled, false, "the refused save wrote nothing");
  });

  test("W2: with the master switch on, a new chat follows newChats and the user's switch flips it", async () => {
    const chat = await newChat(page(), work);
    const offView = (await api(page(), "workflow_chat_mode_get", { chatId: chat })) as Json;
    assert.equal(offView.inEffect, false);
    assert.match(String(offView.unavailable), /off in Settings/);
    const saved = (await api(page(), "workflow_settings_save", { settings: { enabled: true, newChats: "auto", moveFast: true } })) as Json;
    assert.equal(saved.newChats, "auto");
    assert.equal(saved.moveFast, true);
    const v = (await api(page(), "workflow_chat_mode_get", { chatId: chat })) as Json;
    assert.equal(v.mode, "auto");
    assert.equal(v.inEffect, false, "Auto is off until the agent turns it on");
    const on = (await api(page(), "workflow_chat_mode_set", { chatId: chat, mode: "on" })) as { view: Json; row: string };
    assert.equal(on.view.mode, "on");
    assert.equal(on.view.setBy, "user");
    assert.equal(on.view.inEffect, true);
    assert.match(on.row, /You turned Orchestrator on/);
    const same = (await api(page(), "workflow_chat_mode_set", { chatId: chat, mode: "on" })) as Json;
    assert.equal(same.row, undefined, "a matching request writes nothing");
    const off = (await api(page(), "workflow_chat_mode_set", { chatId: chat, mode: "off" })) as { view: Json; row: string };
    assert.equal(off.view.mode, "off");
    assert.match(off.row, /turned Orchestrator off/);
  });

  test("W3: the user's change actions refuse a task that does not exist", async () => {
    const e = await rejects("workflow_change_action", { runId: "nope", repo: "github.com/o/r", branch: "b", action: "merge" });
    assert.match(e, /No such task/);
  });
});
