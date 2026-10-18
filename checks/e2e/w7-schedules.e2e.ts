// Schedules end to end in the real debug app with the fake claude
// (fixtures/w7-schedules.json): Settings > Schedules lists the two ready-made schedules off, turns
// one on for a chat, creates, pauses, resumes and cancels one through the editor; a schedule that
// comes due while its chat is busy waits and then fires once, for the slot that was waiting; and
// after a restart with many missed slots the chat gets one prompt, stored as a schedule-origin
// submission (not the user's own send), with the schedule moved on past now.
//
// Run: `npm run build:debug` at the root, then `npm run e2e -w checks`.

import assert from "node:assert/strict";
import path from "node:path";
import { DatabaseSync } from "node:sqlite";
import { fileURLToPath } from "node:url";
import { after, before, describe, test } from "node:test";
import { api, dispose, FAKE_CLAUDE, launchApp, newChat, openChat, quitGracefully, scratchDir, shot, tid, typeMessage, type App } from "./harness.ts";

const FIX = path.join(path.dirname(fileURLToPath(import.meta.url)), "fixtures");
const OUT = process.env.HELM_E2E_OUT ?? scratchDir("helm-e2e-out-");
const profile = scratchDir("helm-e2e-profile-");
const work = scratchDir("helm-e2e-work-");
let app: App | undefined;
const page = () => app!.page;

const env = {
  HELM_TEST_CLAUDE: JSON.stringify({ command: process.execPath, args: [FAKE_CLAUDE, path.join(FIX, "w7-schedules.json")] }),
};

interface Schedule {
  id: string;
  state: string;
  nextRunAt: number | null;
  revision: number;
  chatTitle: string;
  definition: { title: string; message: string; chatId: string; template?: string; timing: { kind: string; seconds?: number; startAt?: number } };
}
interface Run {
  scheduledAt: number;
  status: string;
}

async function until<T>(what: string, f: () => T | undefined | Promise<T | undefined>, ms = 40_000): Promise<T> {
  const end = Date.now() + ms;
  for (;;) {
    const v = await f();
    if (v !== undefined && v !== null && v !== false) return v as T;
    if (Date.now() > end) throw new Error(`timed out waiting for ${what}`);
    await new Promise((r) => setTimeout(r, 200));
  }
}

const list = async (): Promise<{ schedules: Schedule[]; templates: { id: string; title: string }[] }> =>
  (await api(page(), "schedule_list", { includeInactive: true })) as never;
const runsOf = async (id: string): Promise<Run[]> => ((await api(page(), "schedule_runs", { scheduleId: id })) as { runs: Run[] }).runs;

async function openSchedulesPane(): Promise<void> {
  if ((await page().locator(tid("schedules-pane")).count()) > 0) return;
  await page().keyboard.press("Control+Comma");
  await page().getByRole("tab", { name: "Schedules", exact: true }).click();
  await page().waitForSelector(tid("schedules-pane"), { timeout: 20_000 });
}

/** Sends with a DOM click: a completion toast can sit over the Send button. */
async function sendMessage(text: string): Promise<void> {
  await typeMessage(page(), text);
  await page().locator(tid("send")).dispatchEvent("click");
}

/** Counts the user's bubbles in the open chat that carry the scheduled-prompt header. */
const scheduledBubbles = () =>
  page().evaluate(
    () => [...document.querySelectorAll('[data-testid="transcript-row"][data-role="user"]')].filter((r) => (r.textContent ?? "").includes("[Scheduled prompt]")).length,
  );

let chatA = "";
let chatB = "";
let chatC = "";
let checkIn: Schedule | undefined;
let waiting: Schedule | undefined;

describe("schedules: prompts into an existing chat", { timeout: 10 * 60_000 }, () => {
  before(async () => {
    app = await launchApp(profile, { env });
  });
  after(async () => {
    await dispose(app);
  });

  test("S1: Settings > Schedules offers the two ready-made schedules, off, and turns one on for a chat", async () => {
    try {
      chatA = await newChat(page(), work);
      const start = await list();
      assert.deepEqual(start.templates.map((t) => t.id), ["check-in", "noon-digest"]);
      assert.equal(start.schedules.length, 0, "nothing exists, so nothing can fire, until one is turned on");

      await openSchedulesPane();
      await page().waitForSelector(tid("template-check-in"), { timeout: 15_000 });
      assert.equal(await page().locator(tid("template-noon-digest")).count(), 1);
      assert.equal(await page().locator(tid("schedule-empty")).count(), 1);

      await page().click(tid("template-check-in-on"));
      await page().waitForSelector(tid("schedule-row"), { timeout: 15_000 });
      const after = await list();
      assert.equal(after.schedules.length, 1);
      checkIn = after.schedules[0]!;
      assert.equal(checkIn.definition.template, "check-in");
      assert.equal(checkIn.definition.chatId, chatA);
      assert.deepEqual(checkIn.definition.timing, { kind: "interval", seconds: 1800 });
      assert.equal(checkIn.state, "active");
      assert.ok(checkIn.nextRunAt! > Date.now() + 25 * 60_000, "the first run is about 30 minutes out");
      assert.match((await page().textContent(tid("schedule-row"))) ?? "", /Every 30 minutes/);
      assert.match((await page().textContent(tid(`template-check-in-enabled`))) ?? "", /On for/);
    } finally {
      await shot(page(), OUT, "w7-schedules-pane");
    }
  });

  test("S2: create through the editor, pause, resume and cancel", async () => {
    try {
      await page().keyboard.press("Escape");
      chatC = await newChat(page(), work);
      await openSchedulesPane();
      await page().click(tid("schedule-new"));
      await page().waitForSelector(tid("schedule-editor"), { timeout: 10_000 });

      // A bad entry is named, and nothing is created.
      await page().click(tid("schedule-save"));
      assert.match((await page().textContent(tid("schedule-error"))) ?? "", /name/);
      await page().fill(tid("schedule-title"), "Editor made");
      await page().fill(tid("schedule-message"), "Say hello.");
      await page().fill(tid("schedule-every"), "0.5");
      await page().click(tid("schedule-save"));
      assert.match((await page().textContent(tid("schedule-error"))) ?? "", /1 minute/);
      assert.equal((await list()).schedules.length, 1);

      await page().fill(tid("schedule-every"), "2");
      await page().click(tid("schedule-save"));
      await page().waitForSelector(tid("schedule-editor"), { state: "detached", timeout: 10_000 });
      const made = (await list()).schedules.find((s) => s.definition.title === "Editor made")!;
      assert.ok(made);
      assert.deepEqual(made.definition.timing, { kind: "interval", seconds: 120 });
      assert.equal(made.definition.chatId, chatC, "the open chat is the default destination");

      await page().click(tid(`schedule-${made.id}-toggle`));
      await until("paused", async () => ((await list()).schedules.find((s) => s.id === made.id)?.state === "paused" ? true : undefined));
      await page().waitForFunction((id) => document.querySelector(`[data-testid="schedule-${id}-status"]`)?.textContent === "Paused", made.id, { timeout: 10_000 });
      await page().click(tid(`schedule-${made.id}-toggle`));
      await until("resumed", async () => ((await list()).schedules.find((s) => s.id === made.id)?.state === "active" ? true : undefined));

      // Cancel takes a second click to confirm.
      await page().click(tid(`schedule-${made.id}-cancel`));
      assert.equal((await list()).schedules.find((s) => s.id === made.id)?.state, "active", "one click does not cancel");
      await page().click(tid(`schedule-${made.id}-cancel`));
      await until("cancelled", async () => ((await list()).schedules.find((s) => s.id === made.id)?.state === "cancelled" ? true : undefined));
      await page().waitForSelector(tid("schedule-row", `[data-schedule-id="${made.id}"]`), { state: "detached", timeout: 10_000 });
      assert.equal((await runsOf(made.id)).length, 0, "it never ran");
      // A stale revision is refused by the host.
      await assert.rejects(api(page(), "schedule_update", { scheduleId: checkIn!.id, revision: 999, paused: true }));
    assert.equal((await list()).schedules.find((s) => s.id === checkIn!.id)?.state, "active", "the refused update changed nothing");
      await page().keyboard.press("Escape");
    } finally {
      await shot(page(), OUT, "w7-schedules-editor");
    }
  });

  test("S3: a schedule that comes due while its chat is busy waits, then fires once for the slot that was waiting", async () => {
    chatB = await newChat(page(), work);
    const startAt = Date.now() + 8_000;
    waiting = (await api(page(), "schedule_create", {
      requestId: "e2e-waiting",
      title: "Waits for B",
      message: "Report back.",
      timing: { kind: "interval", seconds: 60, startAt },
      chatId: chatB,
    })) as Schedule;
    // The same request id again is the same schedule.
    const again = (await api(page(), "schedule_create", {
      requestId: "e2e-waiting",
      title: "Waits for B",
      message: "Report back.",
      timing: { kind: "interval", seconds: 60, startAt },
      chatId: chatB,
    })) as Schedule;
    assert.equal(again.id, waiting.id);

    await sendMessage("[HOLD] keep busy");
    // Past the due time and two ticks on, the chat still busy (its turn lasts 40 s): nothing claimed.
    await until("the slot to be due", () => (Date.now() > startAt + 10_000 ? true : undefined), 30_000);
    assert.deepEqual(await runsOf(waiting.id), [], "a busy chat is not sent a prompt");
    const mid = (await list()).schedules.find((s) => s.id === waiting!.id)!;
    assert.equal(mid.nextRunAt, startAt, "the slot is still waiting");

    // The turn ends (about 40 s in): one run, for the slot that was waiting.
    const runs = await until("the waiting run", async () => {
      const r = await runsOf(waiting!.id);
      // Claimed first (dispatching), then recorded as sent.
      return r.length > 0 && r[0]!.status !== "dispatching" ? r : undefined;
    }, 45_000);
    assert.equal(runs.length, 1);
    assert.equal(runs[0]!.scheduledAt, startAt);
    assert.equal(runs[0]!.status, "started");
    const now = (await list()).schedules.find((s) => s.id === waiting!.id)!;
    assert.ok(now.nextRunAt! > Date.now(), "the schedule moved on past now");
    await openChat(page(), chatB);
    await until("the prompt in the chat", async () => ((await scheduledBubbles()) >= 1 ? true : undefined), 20_000);
    assert.equal(await scheduledBubbles(), 1);
    // Stop it repeating while the rest of the run goes on.
    await api(page(), "schedule_cancel", { scheduleId: waiting.id, revision: now.revision });
  });

  test("S4: no CSP violation across the pane and the editor", async () => {
    assert.deepEqual(await app!.cspViolations(), []);
  });

  test("S5: after a restart with many missed slots the chat gets one prompt, as a schedule send", async () => {
    await quitGracefully(app!);
    app = undefined;
    // The app was closed for three hours: wind the check-in back past six of its slots.
    const db = new DatabaseSync(path.join(profile, "helm.sqlite3"));
    const wound = Date.now() - 3 * 3_600_000;
    db.prepare("UPDATE schedules SET next_run_at = ? WHERE id = ?").run(wound, checkIn!.id);
    db.close();

    app = await launchApp(profile, { env });
    const runs = await until("the catch-up run", async () => {
      const r = await runsOf(checkIn!.id).catch(() => []);
      return r.length > 0 && r[0]!.status !== "dispatching" ? r : undefined;
    }, 60_000);
    assert.equal(runs.length, 1, "six missed slots are one run");
    assert.equal(runs[0]!.scheduledAt, wound);
    assert.equal(runs[0]!.status, "started");
    const moved = (await list()).schedules.find((s) => s.id === checkIn!.id)!;
    assert.ok(moved.nextRunAt! > Date.now() && moved.nextRunAt! <= Date.now() + 30 * 60_000, "the next run is the first slot after now");

    await openChat(page(), chatA);
    await until("the prompt in chat A", async () => ((await scheduledBubbles()) >= 1 ? true : undefined), 20_000);
    // Two more ticks go by: still one.
    await new Promise((r) => setTimeout(r, 11_000));
    assert.equal(await scheduledBubbles(), 1);
    assert.equal((await runsOf(checkIn!.id)).length, 1);
    assert.match((await page().textContent(tid("transcript-row", '[data-role="user"]:has-text("[Scheduled prompt]")'))) ?? "", /30-minute check-in/);
    assert.deepEqual(await app.cspViolations(), []);

    // What was stored is a schedule-origin submission under the slot's own id, never the user's.
    await quitGracefully(app);
    app = undefined;
    const check = new DatabaseSync(path.join(profile, "helm.sqlite3"), { readOnly: true });
    const rows = check
      .prepare("SELECT s.id AS id, m.origin AS origin, s.chat_id AS chat FROM submissions s JOIN submission_meta m ON m.submission_id = s.id WHERE s.text LIKE '[Scheduled prompt]%' ORDER BY s.created_at")
      .all() as { id: string; origin: string; chat: string }[];
    check.close();
    assert.equal(rows.length, 2, "one for the waiting chat and one for the catch-up");
    assert.deepEqual([...new Set(rows.map((r) => r.origin))], ["schedule"]);
    assert.deepEqual(rows.map((r) => r.chat).sort(), [chatA, chatB].sort());
  });
});
