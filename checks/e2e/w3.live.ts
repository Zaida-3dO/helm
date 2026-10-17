// Live check L1: one real, read-only sync from Agent Standup with a token from the environment.
// Runs only with HELM_LIVE_STANDUP=1 and the board's URL in HELM_LIVE_STANDUP_URL (or
// AGENT_STANDUP_URL); no host name lives in the repo. The token is read here, inside Node, from the environment
// (AGENT_STANDUP_TOKEN, or HELM_LIVE_STANDUP_TOKEN) and handed to the debug app through its
// environment; it is never printed, logged or written to disk. Only GET requests can leave the
// client: its transport has a single `get` method (helm-standup's no-write test proves it).
//
// Run: `HELM_LIVE_STANDUP=1 HELM_LIVE_STANDUP_URL=<board url> npm run live -w checks` (after
// `npm run build:debug`).

import assert from "node:assert/strict";
import { after, before, describe, test } from "node:test";
import { dispose, launchApp, scratchDir, tid, type App } from "./harness.ts";

const enabled = process.env.HELM_LIVE_STANDUP === "1";
const token = process.env.HELM_LIVE_STANDUP_TOKEN ?? process.env.AGENT_STANDUP_TOKEN ?? "";
const url = process.env.HELM_LIVE_STANDUP_URL ?? process.env.AGENT_STANDUP_URL ?? "";
let app: App | undefined;

describe("live: Agent Standup read sync (L1)", { skip: !enabled || !token || !url ? "set HELM_LIVE_STANDUP=1, the board URL (HELM_LIVE_STANDUP_URL) and a token in the environment" : false, timeout: 5 * 60_000 }, () => {
  before(async () => {
    app = await launchApp(scratchDir("helm-w3live-profile-"), { env: { HELM_TEST_STANDUP_URL: url, HELM_TEST_STANDUP_TOKEN: token } });
  });
  after(async () => {
    await dispose(app);
  });

  test("L1: the board's real projects appear as groups after one sync", async () => {
    const page = app!.page;
    await page.waitForSelector(tid("board-sync", '[data-state="ok"]'), { timeout: 60_000 }).catch(async (e) => {
      const state = await page.getAttribute(tid("board-sync"), "data-state").catch(() => "?");
      const text = await page.textContent(tid("board-sync")).catch(() => "");
      throw new Error(`sync did not reach ok (state ${state}: ${text}): ${(e as Error).message.split("\n")[0]}`);
    });
    const groups = await page.$$eval(tid("sidebar-group", '[data-kind="board"]'), (els) => els.map((e) => (e.querySelector(".sb-name")?.textContent ?? "").trim()));
    assert.ok(groups.length > 0, "at least one board project");
    // Names only, never the token: they are the user's own project titles.
    process.stderr.write(`w3-live L1: ${groups.length} board groups synced\n`);
    const log = app!.log();
    assert.ok(!token || !log.includes(token), "the token never appears in the app's output");
  });
});
