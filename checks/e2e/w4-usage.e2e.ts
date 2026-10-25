// The usage meter end to end in the real debug app with the fake claude (fixtures/w4-usage.json):
// the top-bar meter shows the numbers the binary's usage answer scripted; Settings → Usage shows
// the cards, Request modes counted from scratch transcripts of a chat Helm started, and the Agent
// feedback tab (note text is plain text); chats link themselves to an item, so the sidebar shows an
// item chip, then an item sub-folder, and "open item" offers a picker with the main chat first;
// and a relaunch against a binary that refuses the usage request shows the stale reading with the
// readiness reason.
//
// Run: `npm run build:debug` at the root, then `npm run e2e -w checks`.

import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";
import { DatabaseSync } from "node:sqlite";
import { fileURLToPath } from "node:url";
import { after, before, describe, test } from "node:test";
import { api, dispose, FAKE_CLAUDE, launchApp, newChat, quitGracefully, scratchDir, shot, tid, typeMessage, type App } from "./harness.ts";

const FIX = path.join(path.dirname(fileURLToPath(import.meta.url)), "fixtures");
const OUT = process.env.HELM_E2E_OUT ?? scratchDir("helm-e2e-out-");
const profile = scratchDir("helm-e2e-profile-");
const work = scratchDir("helm-e2e-work-");
const claudeDir = scratchDir("helm-e2e-claude-");
const ITEM = "0badcafe";
let app: App | undefined;
const page = () => app!.page;
type Json = Record<string, unknown>;

const fixtureEnv = (name: string): Record<string, string> => ({
  HELM_TEST_CLAUDE: JSON.stringify({ command: process.execPath, args: [FAKE_CLAUDE, path.join(FIX, name)] }),
  CLAUDE_CONFIG_DIR: claudeDir,
});

async function until<T>(what: string, f: () => T | undefined | Promise<T | undefined>, ms = 30_000): Promise<T> {
  const end = Date.now() + ms;
  for (;;) {
    const v = await f();
    if (v !== undefined && v !== null && v !== false) return v as T;
    if (Date.now() > end) throw new Error(`timed out waiting for ${what}`);
    await new Promise((r) => setTimeout(r, 150));
  }
}

async function openUsagePane(): Promise<void> {
  if ((await page().locator(tid("usage-pane")).count()) > 0) return;
  await page().keyboard.press("Control+Comma");
  await page().getByRole("tab", { name: "Usage", exact: true }).click();
  await page().waitForSelector(tid("usage-pane"), { timeout: 20_000 });
}

/** Sends with a DOM click: the first chat's completion toast can sit over the Send button. */
async function sendMessage(text: string): Promise<void> {
  await typeMessage(page(), text);
  await page().locator(tid("send")).dispatchEvent("click");
}

const links = async (): Promise<Json[]> => ((await api(page(), "item_links_for_item", { itemId: ITEM })) as { links: Json[] }).links;

describe("the plan-usage meter, Settings → Usage and item links", { timeout: 10 * 60_000 }, () => {
  before(async () => {
    app = await launchApp(profile, { env: fixtureEnv("w4-usage.json") });
  });
  after(async () => {
    await dispose(app);
  });

  test("U1: the top bar shows the 5-hour and weekly numbers the binary answered", async () => {
    try {
      await page().waitForSelector(tid("usage-meter", '[data-state="live"]'), { timeout: 40_000 });
      assert.equal((await page().textContent(tid("usage-pct-five_hour")))?.trim(), "5%");
      assert.equal((await page().textContent(tid("usage-pct-weekly")))?.trim(), "56%");
      const title = (await page().getAttribute(tid("usage-meter"), "title")) ?? "";
      assert.match(title, /5-hour window: 5% used/);
      assert.match(title, /Claude Code usage report/);
      const snap = (await api(page(), "usage_get", {})) as { reading: { source: string; plan: string; perModel: { model: string }[] }; readiness: { state: string } };
      assert.equal(snap.reading.source, "control");
      assert.equal(snap.reading.plan, "max");
      assert.deepEqual(snap.reading.perModel.map((m) => m.model), ["Fable"]);
      assert.equal(snap.readiness.state, "ready");
    } finally {
      await shot(page(), OUT, "w4-usage-meter");
    }
  });

  test("U2: chats link themselves to an item: a chip, then a sub-folder, then a picker with the main chat first", async () => {
    try {
      const one = await newChat(page(), work);
      await sendMessage("link this chat");
      await until("the first link", async () => ((await links()).length === 1 ? true : undefined));
      await page().waitForSelector(tid("item-chip"), { timeout: 15_000 });
      assert.equal(await page().locator(tid("item-folder")).count(), 0, "one chat: a chip and no folder");
      const open1 = (await api(page(), "item_open", { itemId: ITEM })) as { chatId?: string };
      assert.equal(open1.chatId, one);

      const two = await newChat(page(), work);
      await sendMessage("link this chat too");
      await until("the second link", async () => ((await links()).length === 2 ? true : undefined));
      await page().waitForSelector(tid("item-folder"), { timeout: 15_000 });
      assert.match((await page().textContent(tid("item-folder"))) ?? "", /Linked item.*2/);
      assert.equal(await page().locator(tid("item-chip")).count(), 0, "inside a folder the folder names the item");
      assert.ok((await page().locator(tid("chat-item", `[data-chat-id="${one}"]`)).count()) === 1);
      assert.ok((await page().locator(tid("chat-item", `[data-chat-id="${two}"]`)).count()) === 1);

      await page().click(`${tid("item-folder")} .sb-item-open`);
      await page().waitForSelector(tid("item-picker"), { timeout: 10_000 });
      const rows = await page().locator(tid("item-picker-row")).evaluateAll((els) => els.map((e) => [e.getAttribute("data-chat-id"), e.getAttribute("data-main")]));
      assert.deepEqual(rows, [[one, "true"], [two, null]], "the first chat linked is main and listed first");
      // The user makes the second chat main: the picker order changes.
      await page().getByRole("button", { name: "Make main" }).click();
      await page().waitForSelector(tid("item-picker"), { state: "detached", timeout: 10_000 });
      const reopened = (await api(page(), "item_open", { itemId: ITEM })) as { candidates: { chatId: string; isMain: boolean }[] };
      assert.deepEqual(reopened.candidates.map((c) => [c.chatId, c.isMain]), [[two, true], [one, false]]);
      await page().click(`${tid("item-folder")} .sb-item-open`);
      await page().waitForSelector(tid("item-picker"));
      await page().locator(tid("item-picker-row")).first().click();
      await page().waitForFunction((id) => document.querySelector('[data-testid="chat-view"]')?.getAttribute("data-chat-id") === id, two, { timeout: 10_000 });
    } finally {
      await shot(page(), OUT, "w4-usage-itemlinks");
    }
  });

  test("U3: Settings → Usage: plan cards, Request modes from Helm's own transcripts, Agent feedback as plain text", async () => {
    try {
      // A chat Helm started, with a native transcript: fast, priority (fast), standard, no mode,
      // and one request whose message arrives twice (counted once).
      const sid = "11111111-2222-3333-4444-555555555555";
      fs.mkdirSync(path.join(profile, "sessions"), { recursive: true });
      fs.writeFileSync(path.join(profile, "sessions", "e2e-modes.claude.json"), JSON.stringify({ nativeSessionId: sid, cwd: work, updatedAt: 1 }));
      const dir = path.join(claudeDir, "projects", work.replace(/[^a-zA-Z0-9]/g, "-"));
      fs.mkdirSync(dir, { recursive: true });
      const at = new Date().toISOString();
      const rec = (id: string, usage: Json | null) => JSON.stringify({ type: "assistant", timestamp: at, requestId: `req_${id}`, uuid: `u_${id}`, message: { id: `msg_${id}`, ...(usage ? { usage } : {}) } });
      fs.writeFileSync(
        path.join(dir, `${sid}.jsonl`),
        [rec("1", { speed: "fast" }), rec("1", { speed: "fast" }), rec("2", { service_tier: "priority" }), rec("3", { speed: "standard" }), rec("4", { input_tokens: 1 })].join("\n") + "\n",
      );
      const modes = (await api(page(), "request_modes", { since: 0, until: Date.now() + 5_000 })) as { fast: number; standard: number; unknown: number; files: number };
      assert.deepEqual([modes.fast, modes.standard, modes.unknown, modes.files], [2, 1, 1, 1]);
      await assert.rejects(api(page(), "request_modes", { since: 10, until: 5 }));

      await openUsagePane();
      assert.equal((await page().textContent(tid("usage-card-five_hour-pct")))?.trim(), "5%");
      assert.equal((await page().textContent(tid("usage-card-weekly-pct")))?.trim(), "56%");
      assert.equal(await page().locator(tid("usage-card-model-Fable")).count(), 1);
      await page().waitForSelector(tid("request-mode-fast-count"), { timeout: 30_000 });
      assert.equal((await page().textContent(tid("request-mode-fast-count")))?.trim(), "2");
      assert.equal((await page().textContent(tid("request-mode-standard-count")))?.trim(), "1");
      assert.equal((await page().textContent(tid("request-mode-unknown-count")))?.trim(), "1");

      await page().getByRole("tab", { name: "Agent feedback", exact: true }).click();
      await page().waitForSelector(tid("feedback-item"), { timeout: 15_000 });
      const note = await page().textContent(tid("feedback-note"));
      assert.equal(note, "<b>plain</b> text only", "an agent's note is plain text, never markup");
      assert.equal(await page().locator(`${tid("agent-feedback")} b`).count(), 0);
    } finally {
      await shot(page(), OUT, "w4-usage-settings");
    }
  });

  test("U4: no CSP violation across the meter, the pane, the folder and the picker", async () => {
    assert.deepEqual(await app!.cspViolations(), []);
  });

  test("U5: a binary that refuses the usage request leaves the last reading, greyed, with the reason", async () => {
    await quitGracefully(app!);
    app = undefined;
    // Age the stored reading past 15 minutes while the app is closed.
    const db = new DatabaseSync(path.join(profile, "helm.sqlite3"));
    const row = db.prepare("SELECT body FROM plan_usage WHERE id = 1").get() as { body: string } | undefined;
    assert.ok(row, "the reading was stored");
    const old = JSON.parse(row.body) as { fiveHour: { at: number }; weekly: { at: number }; perModel: { at: number }[] };
    const ago = 40 * 60_000;
    for (const w of [old.fiveHour, old.weekly, ...old.perModel]) w.at -= ago;
    db.prepare("UPDATE plan_usage SET body = ? WHERE id = 1").run(JSON.stringify(old));
    db.close();
    app = await launchApp(profile, { env: fixtureEnv("w4-usage-unsupported.json") });
    try {
      await until("the refusal to be read", async () => {
        const s = (await api(page(), "usage_get", {})) as { readiness: { reason: string } };
        return /did not report plan usage/.test(s.readiness.reason) ? s : undefined;
      });
      await page().waitForSelector(tid("usage-meter", '[data-state="stale"]'), { timeout: 15_000 });
      assert.equal((await page().textContent(tid("usage-pct-five_hour")))?.trim(), "5%", "the last reading is still shown");
      await openUsagePane();
      const notice = (await page().textContent(tid("usage-readiness"))) ?? "";
      assert.match(notice, /This Claude Code version did not report plan usage; showing the last reading from a chat/);
      assert.match(notice, /Claude Code 0\.0\.0-fake/, "the version is named");
      assert.match(notice, /out of date/);
      assert.deepEqual(await app.cspViolations(), []);
    } finally {
      await shot(page(), OUT, "w4-usage-unsupported");
    }
  });
});
