// AFK and the usage governor end to end in the real debug app with the fake claude
// (fixtures/w7-afk.json): Settings → AFK saves strictly, the AFK switch opens and closes a span,
// a chat that worked while away gets one "While you were out" digest from Helm, and Settings →
// Usage governor bands the scripted plan usage against the user's thresholds.
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

async function openPane(title: string, testId: string): Promise<void> {
  if ((await page().locator(tid(testId)).count()) > 0) return;
  if ((await page().locator('[role="tab"]').count()) === 0) await page().keyboard.press("Control+Comma");
  await page().getByRole("tab", { name: title, exact: true }).click();
  await page().waitForSelector(tid(testId), { timeout: 20_000 });
}

/** The host's refusal of an op, as text (the invoke rejects with an object). */
async function refusal(op: string, args: Record<string, unknown>): Promise<string> {
  return page().evaluate(
    async ([o, a]) => {
      try {
        await (window as unknown as { __TAURI_INTERNALS__: { invoke: (c: string, p: unknown) => Promise<unknown> } }).__TAURI_INTERNALS__.invoke("api", { op: o, args: a });
        return "accepted";
      } catch (e) {
        return JSON.stringify(e);
      }
    },
    [op, args] as const,
  );
}

const afk = async (): Promise<Json> => (await api(page(), "afk_state", {})) as Json;

describe("AFK spans, the digest and the usage governor", { timeout: 10 * 60_000 }, () => {
  before(async () => {
    app = await launchApp(profile, {
      env: {
        HELM_TEST_CLAUDE: JSON.stringify({ command: process.execPath, args: [FAKE_CLAUDE, path.join(FIX, "w7-afk.json")] }),
        CLAUDE_CONFIG_DIR: claudeDir,
      },
    });
  });
  after(async () => {
    await dispose(app);
  });

  test("A1: Settings → AFK turns AFK mode on, and a bad save is refused with its reason", async () => {
    try {
      const first = await afk();
      assert.equal(first.settings.enabled, false, "off by default");
      assert.equal(first.open, null);
      await openPane("AFK", "afk-pane");
      await page().locator(`${tid("afk-enabled-box")}`).check();
      await page().click(tid("afk-save"));
      await until("AFK enabled", async () => ((await afk()).settings.enabled ? true : undefined));
      assert.match(await refusal("afk_settings", { settings: { enabled: true, auto: { idleMinutes: 2 } } }), /5 to 240/);
      assert.match(await refusal("afk_settings", { settings: { enabled: true, appliesTo: "chat" } }), /not available/);
      assert.equal((await afk()).settings.auto.idleMinutes, 30, "a refused save changes nothing");
    } finally {
      await shot(page(), OUT, "w7-afk-settings");
    }
  });

  test("A2: a chat that worked while away gets one While-you-were-out digest from Helm", async () => {
    try {
      await page().keyboard.press("Escape");
      const on = (await api(page(), "afk_set", { on: true })) as Json;
      assert.equal(on.open.source, "manual");
      const chat = await newChat(page(), work);
      await typeMessage(page(), "working while you are away");
      await page().locator(tid("send")).dispatchEvent("click");
      await waitText(page(), "You said: working while you are away", 30_000);
      // A manual span under a minute is accidental and gets no digest.
      await new Promise((r) => setTimeout(r, 61_000));
      const off = (await api(page(), "afk_set", { on: false })) as Json;
      assert.equal(off.open, null);
      assert.equal(off.last.digests.length, 1);
      assert.equal(off.last.digests[0].chatId, chat);
      await waitText(page(), "Back from AFK", 30_000);
      await waitText(page(), "You said: [Helm · Back from AFK", 30_000);
      const sent = await until("digest sent", async () => {
        const v = await afk();
        return v.last?.digests[0]?.state === "sent" ? v : undefined;
      }, 90_000);
      assert.equal(sent.failures, null);
    } finally {
      await shot(page(), OUT, "w7-afk-digest");
    }
  });

  test("G1: Settings → Usage governor bands the plan usage against the user's thresholds", async () => {
    try {
      const off = (await api(page(), "governor_state", {})) as Json;
      assert.equal(off.settings.enabled, false, "off by default");
      assert.equal(off.band, "unknown");
      await until("a usage reading", async () => {
        const u = (await api(page(), "usage_get", {})) as Json;
        return u.reading?.weekly?.pct === 56 ? true : undefined;
      }, 40_000);
      assert.match(await refusal("governor_settings", { settings: { enabled: true, windDownPct: 99 } }), /rise/);
      const v = (await api(page(), "governor_settings", {
        settings: { enabled: true, selectivePct: 10, windDownPct: 60, stopPct: 90, atStop: "wrapUp" },
      })) as Json;
      assert.equal(v.usedPct, 56, "the higher of the 5-hour and weekly windows");
      assert.equal(v.band, "selective");
      await page().keyboard.press("Control+Comma");
      await openPane("Usage governor", "governor-pane");
      await page().waitForSelector(tid("governor-band", '[data-band="selective"]'), { timeout: 10_000 });
      // Off again, so nothing else in the run is governed.
      await api(page(), "governor_settings", { settings: { enabled: false } });
    } finally {
      await shot(page(), OUT, "w7-governor");
    }
  });
});
