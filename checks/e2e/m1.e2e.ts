// Core scenarios end to end with the fake claude (E1–E8), plus the Markdown renderer under the
// production CSP. Every chat runs the same fake script (fakes/claude/fixtures/e2e.json), which
// branches on a tag in each prompt, so all scenarios share one app run; E8 relaunches the app on the same profile.
//
// Run: `npm run build:debug` at the root, then `npm run e2e -w checks`.

import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { after, before, describe, test } from "node:test";
import {
  api,
  assertShown,
  countRowsWith,
  dispose,
  killHard,
  launchApp,
  newChat,
  openChat,
  quitGracefully,
  rows,
  scratchDir,
  send,
  shot,
  tid,
  typeMessage,
  unfoldAll,
  waitRowText,
  waitStatus,
  waitText,
  type App,
} from "./harness.ts";

const OUT = process.env.HELM_E2E_OUT ?? scratchDir("helm-e2e-out-");
const profile = scratchDir("helm-e2e-profile-");
const folder = () => scratchDir("helm-e2e-chat-");
let app: App | undefined;

const page = () => app!.page;

async function failShot(name: string, fn: () => Promise<void>): Promise<void> {
  try {
    await fn();
    await shot(page(), OUT, name);
  } catch (e) {
    await shot(page(), OUT, `${name}-FAILED`);
    throw new Error(`${(e as Error).message}\n--- app log tail ---\n${app?.log().slice(-3000)}`);
  }
}

describe("core scenarios end to end with the fake claude", { timeout: 15 * 60_000 }, () => {
  before(async () => {
    process.stderr.write(`e2e: profile ${profile}\ne2e: screenshots ${OUT}\n`);
    app = await launchApp(profile);
  });

  after(async () => {
    await dispose(app);
  });

  test("E1: create a chat, send, see streaming text, then a complete reply", async () => {
    await failShot("E1", async () => {
      await newChat(page(), folder());
      await send(page(), "[E1] say hello");
      await page().waitForSelector(tid("transcript-row", '[data-role="assistant"][data-status="streaming"]'), { timeout: 20_000 });
      await waitText(page(), "in pieces.");
      await page().waitForSelector(tid("transcript-row", '[data-role="assistant"][data-status="complete"]'), { timeout: 20_000 });
      await waitStatus(page(), "idle");
      assert.equal(await rows(page(), "user").count(), 1);
    });
  });

  test("E2: a tool card and an approval card; Allow once, then Deny", async () => {
    await failShot("E2", async () => {
      await newChat(page(), folder());
      await send(page(), "[E2] tidy the folder");
      const card = page().locator(tid("interaction-card", '[data-attention="approval"]'));
      await card.waitFor({ timeout: 20_000 });
      assert.ok((await page().locator(tid("tool-card")).count()) >= 1, "tool card shown");
      await page().click(tid("approval-button", '[data-option="Allow once"]'));
      // The second call's card is shown. (A live turn folds its earlier work as it goes, so the first
      // call may already be behind "Worked for": wait for the second by its command, not a count.)
      await page().waitForFunction(
        () => [...document.querySelectorAll('[data-testid="tool-card"]')].some((c) => (c.textContent ?? "").includes("rm b.txt")),
        null,
        { timeout: 20_000 },
      );
      await card.waitFor({ timeout: 20_000 });
      await page().click(tid("approval-button", '[data-option="Deny"]'));
      await waitText(page(), "second call denied");
      await waitStatus(page(), "idle");
    });
  });

  test("E3: question steps, then plan approval", async () => {
    await failShot("E3", async () => {
      await newChat(page(), folder());
      await send(page(), "[E3] plan it");
      await page().locator(tid("interaction-card", '[data-attention="question"]')).waitFor({ timeout: 20_000 });
      await page().click(tid("question-button", '[data-option="Red"]'));
      await page().waitForSelector(tid("question-button", '[data-option="Large"]'), { timeout: 20_000 });
      await page().click(tid("question-button", '[data-option="Large"]'));
      // The reply streams in paced; wait for its last answer before reading the row.
      const answers = await waitRowText(page(), "Large");
      assert.match(answers, /Answers:/);
      await page().locator(tid("interaction-card", '[data-attention="plan"]')).waitFor({ timeout: 20_000 });
      await page().click(tid("plan-button", '[data-option="Approve plan"]'));
      await waitText(page(), "Plan approved; starting.");
      assert.match(answers, /Red/);
      assert.match(answers, /Large/);
    });
  });

  test("E4: a mid-turn message is Pending, read at the next tool boundary, and the same turn acts on it", async () => {
    await failShot("E4", async () => {
      await newChat(page(), folder());
      await send(page(), "[E4] look around");
      await page().waitForSelector(tid("tool-card", '[data-status="running"]'), { timeout: 20_000 });
      // asap is the home default: Send while busy steers.
      await send(page(), "also mention bananas");
      await page().waitForSelector(tid("queue-item", '[data-state="pending"]'), { timeout: 10_000 });
      await page().waitForSelector(tid("queue-pending"), { timeout: 5_000 });
      await shot(page(), OUT, "E4-pending");
      await waitText(page(), "E4 reply after reading: also mention bananas", 30_000);
      await page().waitForFunction(() => !document.querySelector('[data-testid="queue-item"][data-state="pending"]'), null, { timeout: 10_000 });
      // Order from the host's stored transcript: the DOM folds the settled turn's tool rows.
      const chatId = await page().locator(tid("chat-view")).getAttribute("data-chat-id");
      const stored = (await api(page(), "transcript_page", { chatId })) as { messages: { role: string; text: string }[] };
      const all = stored.messages;
      const order = {
        tool: all.findIndex((r) => r.role === "toolResult"),
        steer: all.findIndex((r) => r.role === "user" && r.text.includes("also mention bananas")),
        reply: all.findIndex((r) => r.text.includes("E4 reply after reading")),
      };
      assert.ok(order.tool !== -1 && order.tool < order.steer, `steer row after the tool result (read at the boundary): ${JSON.stringify(order)}`);
      assert.ok(order.steer < order.reply, `steer row before the reply: ${JSON.stringify(order)}`);
      await waitStatus(page(), "idle");
    });
  });

  test("E5: a classified Bash call is denied once, re-run in the background, and the chip shows 1", async () => {
    await failShot("E5", async () => {
      await newChat(page(), folder());
      await send(page(), "[E5] run the tests");
      await waitText(page(), "E5: the tests are running in the background.", 30_000);
      await page().waitForSelector(tid("background-chip", '[data-count="1"]'), { timeout: 10_000 });
      await waitStatus(page(), "idle");
      await unfoldAll(page());
      const failed = await page().locator(tid("tool-card", '[data-tool="Bash"]')).count();
      assert.ok(failed >= 2, "the denied call and the background re-run both show");
    });
  });

  test("E7: Stop leaves the chat stopped and the queued message uncertain; Esc lets the queue continue", async () => {
    await failShot("E7", async () => {
      await newChat(page(), folder());
      await send(page(), "[E7] wait a while");
      await page().waitForSelector(tid("tool-card", '[data-status="running"]'), { timeout: 20_000 });
      // Queue explicitly (Alt+Enter): under asap a plain Send would steer instead.
      await typeMessage(page(), "queued after stop");
      await page().keyboard.press("Alt+Enter");
      await page().waitForSelector(tid("queue-item", '[data-state="queued"]'), { timeout: 10_000 });
      await page().click(tid("stop"));
      await waitStatus(page(), "stopped", 30_000);
      await page().waitForSelector(tid("queue-item", '[data-state="uncertain"]'), { timeout: 10_000 });
      await shot(page(), OUT, "E7-stopped");

      // Esc: Stop that continues the queue.
      await send(page(), "[E7] wait again");
      await page().waitForSelector(tid("tool-card", '[data-status="running"]'), { timeout: 20_000 });
      await typeMessage(page(), "after esc");
      await page().keyboard.press("Alt+Enter");
      await page().waitForSelector(tid("queue-item", '[data-state="queued"]'), { timeout: 10_000 });
      await page().locator(tid("composer")).click();
      await page().keyboard.press("Escape");
      await waitText(page(), "You said: after esc", 30_000);
    });
  });

  test("an abnormal end (error, not Stop) holds the queue with Resume; Resume sends it", async () => {
    await failShot("held-queue", async () => {
      await newChat(page(), folder());
      await send(page(), "[ERR] fail this turn");
      await page().waitForSelector(tid("tool-card", '[data-status="running"]'), { timeout: 20_000 });
      await typeMessage(page(), "held follow-up");
      await page().keyboard.press("Alt+Enter");
      await page().waitForSelector(tid("queue-item", '[data-state="queued"]'), { timeout: 10_000 });
      await waitStatus(page(), "interrupted", 30_000);
      await page().waitForSelector(tid("queue-resume"), { timeout: 10_000 });
      await page().waitForTimeout(1_000);
      assert.equal(await countRowsWith(page(), "You said: held follow-up"), 0, "the queue did not advance by itself");
      await page().click(tid("queue-resume"));
      await waitText(page(), "You said: held follow-up", 30_000);
    });
  });

  test("E6: wait limit 5 s: a blocking unclassified call moves to the background and the steer is delivered in the same turn", async () => {
    await failShot("E6", async () => {
      // Set before creating the chat: the setting applies to new and idle chats.
      await api(page(), "preference", { key: "sendBehavior", value: { waitLimitSeconds: 5 } });
      await newChat(page(), folder());
      await send(page(), "[E6] start the server");
      await page().waitForSelector(tid("tool-card", '[data-status="running"]'), { timeout: 20_000 });
      const t0 = Date.now();
      await send(page(), "steer six");
      await page().waitForSelector(tid("queue-item", '[data-state="pending"]'), { timeout: 10_000 });
      await waitText(page(), "E6 reply: the server keeps running in the background; noted: steer six", 30_000);
      const took = Date.now() - t0;
      assert.ok(took >= 3_000, `moved only after the wait limit (${took} ms)`);
      await page().waitForSelector(tid("background-chip", '[data-count="1"]'), { timeout: 10_000 });
      const stopped = await page().locator(tid("transcript-row", '[data-status="interrupted"]')).count();
      assert.equal(stopped, 0, "the turn did not end");
      await waitStatus(page(), "idle");
      await api(page(), "preference", { key: "sendBehavior", value: null });
    });
  });

  test("Markdown: a Mermaid diagram and a callout render under the production CSP", async () => {
    await failShot("markdown", async () => {
      await newChat(page(), folder());
      await send(page(), "[MD] show markdown");
      await page().waitForSelector(".md-callout", { timeout: 20_000 });
      await page().waitForSelector(".md-mermaid svg", { timeout: 30_000 });
      const dom = await page().evaluate(() => {
        const svg = document.querySelector(".md-mermaid svg")!;
        const node = svg.querySelector("g.node rect, g.node polygon, rect.basic, .node rect") as SVGElement | null;
        const callout = document.querySelector(".md-callout")!;
        return {
          styleEl: svg.querySelectorAll("style").length,
          styleText: (svg.querySelector("style")?.textContent ?? "").length,
          nodeFill: node ? getComputedStyle(node).fill : null,
          nodeStroke: node ? getComputedStyle(node).stroke : null,
          labels: svg.textContent ?? "",
          calloutTitle: callout.querySelector(".md-callout-title")?.textContent ?? "",
          calloutKind: callout.getAttribute("data-callout"),
        };
      });
      process.stderr.write(`e2e markdown: ${JSON.stringify(dom)}\n`);
      assert.ok(dom.styleEl >= 1 && dom.styleText > 100, "Mermaid's <style> element is present and filled");
      assert.match(dom.labels, /Start/);
      assert.match(dom.labels, /Done/);
      assert.ok(dom.nodeFill && dom.nodeFill !== "rgb(0, 0, 0)", `node fill comes from Mermaid's styles (${dom.nodeFill})`);
      assert.ok(dom.nodeStroke && dom.nodeStroke !== "none", `node stroke applied (${dom.nodeStroke})`);
      assert.equal(dom.calloutKind, "note");
      assert.match(dom.calloutTitle, /Heads up/);
      assert.deepEqual(await app!.cspViolations(), [], "no CSP violations");
    });
  });

  test("CSP: the Markdown worker parses a long reply, and data: and blob: images load", async () => {
    await failShot("CSP-worker-images", async () => {
      // The resource-timing buffer holds 250 entries by default, and the app frame loads more
      // than that before this test: make room so the worker's fetch is recorded.
      await page().evaluate(() => {
        performance.setResourceTimingBufferSize(10_000);
        performance.clearResourceTimings();
      });
      const longChat = await newChat(page(), folder());
      await send(page(), "[MDLONG] a long reply");
      await waitText(page(), "End of long text", 60_000);
      // A reply that arrives streaming renders block by block on the main thread; a
      // long reply read from history goes through the worker. Leave the chat and come back.
      await waitStatus(page(), "idle");
      await newChat(page(), folder());
      await openChat(page(), longChat);
      await page().waitForFunction(
        () => [...document.querySelectorAll('[data-testid="transcript-row"] h2')].some((h) => (h.textContent ?? "").includes("End of long text")),
        null,
        { timeout: 20_000 },
      );
      const dom = await page().evaluate(async () => {
        const img = document.querySelector('[data-testid="transcript-row"] img[src^="data:image/png"]') as HTMLImageElement | null;
        if (img && !img.complete) await new Promise((r) => img.addEventListener("load", r, { once: true }) ?? setTimeout(r, 2000));
        // A blob: image, the form a resolved image embed uses.
        const png = Uint8Array.from(atob("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg=="), (c) => c.charCodeAt(0));
        const url = URL.createObjectURL(new Blob([png], { type: "image/png" }));
        const blobOk = await new Promise<boolean>((resolve) => {
          const b = new Image();
          b.onload = () => resolve(b.naturalWidth === 1);
          b.onerror = () => resolve(false);
          b.src = url;
        });
        // The renderer's own module worker, loaded from the bundle the way engine.ts does it.
        const workerUrl = performance.getEntriesByType("resource").map((e) => e.name).find((n) => n.includes("markdown.worker"));
        return {
          dataImg: img ? { complete: img.complete, w: img.naturalWidth } : null,
          blobOk,
          workerUrl: workerUrl ?? null,
          heading: [...document.querySelectorAll('[data-testid="transcript-row"] h2')].some((h) => (h.textContent ?? "").includes("End of long text")),
        };
      });
      process.stderr.write(`e2e CSP: ${JSON.stringify(dom)}
`);
      assert.ok(dom.heading, "the long reply rendered as Markdown");
      assert.ok(dom.workerUrl, "the Markdown worker script was fetched (the long reply went through it)");
      assert.deepEqual(dom.dataImg, { complete: true, w: 1 }, "data: image loaded");
      assert.equal(dom.blobOk, true, "blob: image loaded");
      assert.deepEqual(await app!.cspViolations(), [], "no CSP violations");
    });
  });

  test("U11: transcript, composer, approval card and queue are laid out and readable in the real exe", async () => {
    await failShot("U11", async () => {
      await newChat(page(), folder());
      await send(page(), "[E2] tidy the folder");
      await page().locator(tid("interaction-card", '[data-attention="approval"]')).waitFor({ timeout: 20_000 });
      const report: Record<string, unknown> = {};
      report.transcript = await assertShown(page(), tid("transcript"), 1);
      report.userRow = await assertShown(page(), tid("transcript-row", '[data-role="user"]'));
      report.composer = await assertShown(page(), tid("composer"));
      report.card = await assertShown(page(), tid("interaction-card"));
      report.allow = await assertShown(page(), tid("approval-button", '[data-option="Allow once"]'));
      await shot(page(), OUT, "U11-approval");
      await page().click(tid("approval-button", '[data-option="Allow once"]'));
      await page().locator(tid("interaction-card", '[data-attention="approval"]')).waitFor({ timeout: 20_000 });
      await page().click(tid("approval-button", '[data-option="Deny"]'));
      await waitStatus(page(), "idle");
      await send(page(), "[E4] look around");
      await page().waitForSelector(tid("tool-card", '[data-status="running"]'), { timeout: 20_000 });
      report.toolCard = await assertShown(page(), tid("tool-card", '[data-status="running"]'));
      await send(page(), "queued for layout");
      await page().waitForSelector(tid("queue-item", '[data-state="pending"]'), { timeout: 10_000 });
      report.queueItem = await assertShown(page(), tid("queue-item"));
      report.stop = await assertShown(page(), tid("stop"));
      await shot(page(), OUT, "U11-queue");
      await waitStatus(page(), "idle", 30_000);
      fs.writeFileSync(path.join(OUT, "U11-measurements.json"), JSON.stringify(report, null, 2));
      process.stderr.write(`e2e U11: ${Object.keys(report).length} elements measured
`);
    });
  });

  test("E8: graceful quit then hard kill, mid-turn: transcripts intact, the running chat continues once, the stopped one does not, the draft survives", async () => {
    // Run 1: one running chat, one user-stopped chat with a draft.
    const running = await newChat(page(), folder());
    await send(page(), "[E8-run] keep working");
    await page().waitForSelector(tid("tool-card", '[data-status="running"]'), { timeout: 20_000 });
    const stopped = await newChat(page(), folder());
    await send(page(), "[E8-stop] work then stop");
    await page().waitForSelector(tid("tool-card", '[data-status="running"]'), { timeout: 20_000 });
    await page().click(tid("stop"));
    await waitStatus(page(), "stopped", 30_000);
    await typeMessage(page(), "draft that survives");
    await page().waitForTimeout(1_000); // drafts save 250 ms after the last edit
    await openChat(page(), running);
    await waitStatus(page(), "running");
    await shot(page(), OUT, "E8-before-quit");
    await quitGracefully(app!);

    const check = async (round: number) => {
      await openChat(page(), running);
      await waitText(page(), "[E8-run] keep working");
      await page().waitForFunction(
        (n) => [...document.querySelectorAll('[data-testid="transcript-row"]')].filter((r) => (r.textContent ?? "").includes("Continued after restart.")).length >= n,
        round,
        { timeout: 60_000 },
      );
      await waitStatus(page(), ["idle", "stopped", "interrupted"], 30_000);
      await page().waitForTimeout(2_000); // a second continuation would land by now
      await unfoldAll(page());
      assert.equal(await countRowsWith(page(), "Continued after restart."), round, `exactly ${round} continuation(s) after restart ${round}`);
      await openChat(page(), stopped);
      await waitText(page(), "[E8-stop] work then stop");
      await page().waitForTimeout(1_000);
      await unfoldAll(page());
      assert.equal(await countRowsWith(page(), "Continued after restart."), 0, "the user-stopped chat is not continued");
      const draft = (await page().locator(tid("composer")).textContent()) ?? "";
      assert.match(draft, /draft that survives/);
    };

    // Run 2: after the graceful quit.
    app = await launchApp(profile);
    await failShot("E8-after-graceful", () => check(1));

    // Make the chat run again, then crash the app.
    await openChat(page(), running);
    await send(page(), "[E8-run] keep working again");
    await page().waitForSelector(tid("tool-card", '[data-status="running"]'), { timeout: 20_000 });
    await killHard(app!);

    // Run 3: after the hard kill.
    app = await launchApp(profile);
    await failShot("E8-after-kill", () => check(2));
  });
});

// Keep the scratch profile only when asked (it holds the fake transcripts for inspection).
process.on("exit", () => {
  try {
    if (process.env.HELM_E2E_KEEP !== "1") fs.rmSync(profile, { recursive: true, force: true });
  } catch {
    // a file still held open (an app that failed to quit): leave the scratch profile behind
  }
  void os;
  void path;
});
