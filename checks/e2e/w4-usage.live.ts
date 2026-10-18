// Live check L4: the top-bar usage meter shows real 5-hour and weekly
// numbers read from the real claude binary, in the debug app. No chat is started and no prompt is
// sent: the numbers come from the binary's usage control request alone. Reads no credential file
// and prints only the two percentages (never a token).
//
// Runs only with HELM_LIVE_CLAUDE=1. Run: `npm run build:debug`, then
// `HELM_LIVE_CLAUDE=1 node --test e2e/w4-usage.live.ts` in checks/.
//
// The numbers are compared with a second, direct run of the same helper (`helpers/usage`) in the
// same minute.

import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import path from "node:path";
import { after, before, describe, test } from "node:test";
import { api, dispose, launchApp, REPO, scratchDir, shot, tid, type App } from "./harness.ts";

const enabled = process.env.HELM_LIVE_CLAUDE === "1";
const OUT = process.env.HELM_E2E_OUT ?? scratchDir("helm-e2e-out-");
let app: App | undefined;
const page = () => app!.page;

describe("live: the plan-usage meter with the real claude binary", { skip: enabled ? false : "set HELM_LIVE_CLAUDE=1", timeout: 10 * 60_000 }, () => {
  before(async () => {
    app = await launchApp(scratchDir("helm-live-usage-"), { fake: false });
  });
  after(async () => {
    await dispose(app);
  });

  test("L4: the meter shows the binary's own 5-hour and weekly numbers, and no prompt was sent", async () => {
    try {
      await page().waitForSelector(tid("usage-meter", '[data-state="live"]'), { timeout: 120_000 });
      const five = Number.parseFloat((await page().textContent(tid("usage-pct-five_hour"))) ?? "");
      const weekly = Number.parseFloat((await page().textContent(tid("usage-pct-weekly"))) ?? "");
      assert.ok(Number.isFinite(five) && Number.isFinite(weekly), "both windows show a number");

      // The same helper, run directly: it must agree within a point (usage moves slowly).
      const r = spawnSync(process.execPath, [path.join(REPO, "helpers", "usage", "src", "main.ts")], {
        input: JSON.stringify({ kind: "plan" }),
        encoding: "utf8",
        timeout: 90_000,
      });
      const direct = JSON.parse(r.stdout.trim().split("\n").pop() ?? "{}") as { ok?: boolean; reading?: { fiveHour?: { pct: number }; weekly?: { pct: number } } };
      assert.equal(direct.ok, true, "the helper reads the binary's usage directly too");
      assert.ok(Math.abs(Math.round(direct.reading!.fiveHour!.pct) - five) <= 1, `5-hour: meter ${five}, direct ${direct.reading!.fiveHour!.pct}`);
      assert.ok(Math.abs(Math.round(direct.reading!.weekly!.pct) - weekly) <= 1, `weekly: meter ${weekly}, direct ${direct.reading!.weekly!.pct}`);

      const snap = (await api(page(), "usage_get", {})) as { reading: { source: string }; readiness: { state: string } };
      assert.equal(snap.reading.source, "control");
      assert.equal(snap.readiness.state, "ready");
      // No chat exists: nothing in this run could have sent a prompt.
      const chats = (await api(page(), "chat_list", {})) as unknown[];
      assert.equal(chats.length, 0);
      console.log(`live usage meter: 5-hour ${five}%, weekly ${weekly}% (direct helper: ${Math.round(direct.reading!.fiveHour!.pct)}% / ${Math.round(direct.reading!.weekly!.pct)}%)`);
      assert.deepEqual(await app!.cspViolations(), []);
    } finally {
      await shot(page(), OUT, "w4-usage-live");
    }
  });
});
