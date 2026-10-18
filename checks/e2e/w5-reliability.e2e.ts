// Failure modes and unattended reliability in the real debug app with the fake claude.
//
// - R1: the main window's content process dies (a CDP Page.crash): the host notices the page has
//   stopped answering, writes a diagnostic line and reloads it; the saved draft is still there.
// - R2: a hard kill with one chat running and a follow-up queued behind it, and a draft in another
//   chat: after relaunch the running chat gets exactly one restart continuation (crash
//   and quit alike), its queued follow-up is sent once after that (a continued chat's queue is not
//   held), the interrupted prompt is never resent, and the draft is intact.
//
// Run: `npm run build:debug` at the root, then `npm run e2e -w checks`.

import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import fs from "node:fs";
import path from "node:path";
import { after, before, describe, test } from "node:test";
import { chromium, type Page } from "playwright-core";
import { dispose, killHard, launchApp, newChat, openChat, scratchDir, send, tid, typeMessage, unfoldAll, waitStatus, waitText, type App } from "./harness.ts";

const profile = scratchDir("helm-e2e-profile-");
const work = scratchDir("helm-e2e-work-");
let app: App | undefined;
let port = 0;
const page = () => app!.page;

/** The DevTools port of the WebView2 browser process running on `profile` (from its command line). */
function devToolsPort(profileDir: string): number {
  const ps =
    "Get-CimInstance Win32_Process -Filter \"Name='msedgewebview2.exe'\" | ForEach-Object { $_.CommandLine }";
  const r = spawnSync("powershell", ["-NoProfile", "-Command", ps], { encoding: "utf8", timeout: 30_000 });
  const want = path.join(profileDir, "webview").toLowerCase();
  for (const line of (r.stdout ?? "").split(/\r?\n/)) {
    const m = line.match(/--remote-debugging-port=(\d+)/);
    if (m && line.toLowerCase().includes(want)) return Number(m[1]);
  }
  throw new Error(`no WebView2 DevTools port found for ${profileDir}`);
}

async function until<T>(what: string, f: () => T | undefined | Promise<T | undefined>, ms = 30_000): Promise<T> {
  const end = Date.now() + ms;
  for (;;) {
    // A call into a crashed page can hang rather than fail: each attempt gets 2 s.
    const v = await Promise.race([
      Promise.resolve(f()).catch(() => undefined),
      new Promise<undefined>((r) => setTimeout(() => r(undefined), 2_000)),
    ]);
    if (v !== undefined && v !== null && v !== false) return v as T;
    if (Date.now() > end) throw new Error(`timed out waiting for ${what}`);
    await new Promise((r) => setTimeout(r, 250));
  }
}

const userRows = () =>
  page().evaluate(() => [...document.querySelectorAll('[data-testid="transcript-row"][data-role="user"]')].map((r) => r.textContent ?? ""));

const composerText = () =>
  page().evaluate(() => {
    const el = document.querySelector('[data-testid="composer"]') as HTMLTextAreaElement | HTMLElement | null;
    if (!el) return null;
    return "value" in el ? (el as HTMLTextAreaElement).value : el.textContent;
  });

describe("failure modes in the running app", { timeout: 10 * 60_000 }, () => {
  before(async () => {
    app = await launchApp(profile);
    // The DevTools port, so the test can reconnect after the renderer is replaced.
    port = devToolsPort(profile);
  });
  after(async () => {
    await dispose(app);
  });

  test("R1: a dead content process is reloaded with a diagnostic, and the draft survives", async () => {
    const chat = await newChat(page(), work);
    await typeMessage(page(), "draft kept across a renderer crash");
    await until("the draft typed", async () => (await composerText())?.includes("renderer crash"));
    // The watchdog only judges a page that has answered a probe since it loaded (probes run every
    // 10 s from launch), so let it see this one alive first; the draft saves meanwhile.
    await new Promise((r) => setTimeout(r, 22_000));

    const cdp = await page().context().newCDPSession(page());
    void cdp.send("Page.crash").catch(() => undefined);

    // The watchdog probes every 10 s and gives up after 30 s unanswered; allow for both.
    const log = path.join(profile, "diagnostics", "ui-process.log");
    await until("the diagnostic line", () => fs.existsSync(log) && fs.readFileSync(log, "utf8").includes("reloaded it"), 90_000).catch(async (e) => {
      const alive = await Promise.race([page().evaluate(() => "alive").catch((x) => `dead: ${x}`), new Promise((r) => setTimeout(() => r("no answer"), 3_000))]);
      throw new Error(`${(e as Error).message}; page after crash: ${alive}; app log: ${app!.log().slice(-1500)}`);
    });
    assert.match(fs.readFileSync(log, "utf8"), /did not answer for \d+ s; reloaded it/);

    // Reconnect: the crashed page object is dead to Playwright; find the reloaded app page.
    await app!.browser.close().catch(() => undefined);
    const live = await until<Page>(
      "the reloaded page",
      async () => {
        // Reconnect on every attempt: a connection made while the page was being replaced can
        // miss the new page.
        await app!.browser.close().catch(() => undefined);
        app!.browser = await chromium.connectOverCDP(`http://127.0.0.1:${port}`);
        for (const p of app!.browser.contexts().flatMap((c) => c.pages())) {
          if (p.url().startsWith("devtools:")) continue;
          const ok = await p.evaluate(() => !!document.querySelector('[data-testid="new-chat"]')).catch(() => false);
          if (ok) return p;
        }
        return undefined;
      },
      120_000,
    );
    app!.page = live;
    if ((await live.locator(tid("chat-view", `[data-chat-id="${chat}"]`)).count()) === 0) {
      await live.click(tid("chat-item", `[data-chat-id="${chat}"]`));
    }
    await until("the draft back in the composer", async () => (await composerText())?.includes("draft kept across a renderer crash"), 20_000);
  });

  test("R2: after a hard kill, the running chat continues once, then its queued follow-up; the draft survives", async () => {
    // A fresh launch, so R2 never depends on how R1 left the page.
    await killHard(app!);
    app = await launchApp(profile);
    const running = await newChat(page(), work);
    await send(page(), "[E8-run] keep working");
    await page().waitForSelector(tid("tool-card", '[data-status="running"]'), { timeout: 20_000 });
    // Queue explicitly (Alt+Enter): under asap a plain Send would steer instead.
    await typeMessage(page(), "queued across a crash");
    await page().keyboard.press("Alt+Enter");
    await page().waitForSelector(tid("queue-item", '[data-state="queued"]'), { timeout: 10_000 });
    const other = await newChat(page(), work);
    await typeMessage(page(), "draft across a crash");
    await page().waitForTimeout(1_000); // drafts save 250 ms after the last edit
    await openChat(page(), running);
    await waitStatus(page(), "running");
    await killHard(app!);

    app = await launchApp(profile);
    await openChat(page(), running);
    // The continuation prompt is a user-side row; the queued follow-up's row appears once it is sent.
    await until("the continuation, then the queued follow-up", async () => (await userRows()).some((t) => t.includes("queued across a crash")), 60_000);
    await waitStatus(page(), ["idle", "interrupted", "stopped"], 30_000);
    await page().waitForTimeout(2_000); // a second continuation or resend would land by now
    await unfoldAll(page());
    const users = await userRows();
    assert.equal(users.filter((t) => t.includes("Helm was restarted")).length, 1, `exactly one continuation: ${JSON.stringify(users)}`);
    assert.equal(users.filter((t) => t.includes("queued across a crash")).length, 1, "the queued follow-up is sent once");
    assert.equal(users.filter((t) => t.includes("[E8-run] keep working")).length, 1, "the interrupted prompt is not resent");
    assert.ok(
      users.findIndex((t) => t.includes("Helm was restarted")) < users.findIndex((t) => t.includes("queued across a crash")),
      "the continuation goes first",
    );
    await openChat(page(), other);
    await until("the draft in the other chat", async () => (await composerText())?.includes("draft across a crash"), 20_000);
  });
});
