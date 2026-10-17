// Chats end to end with the fake claude: fork from a reply, edit and resend, mute, unread marks
// and the attention summary, in the real debug app.
//
// Run: `npm run build:debug` at the root, then `npm run e2e -w checks`.

import assert from "node:assert/strict";
import { after, before, describe, test } from "node:test";
import { api, dispose, launchApp, newChat, scratchDir, send, shot, tid, waitRowText, waitStatus, type App } from "./harness.ts";

const OUT = process.env.HELM_E2E_OUT ?? scratchDir("helm-e2e-out-");
const profile = scratchDir("helm-e2e-profile-");
let app: App | undefined;
const page = () => app!.page;

interface Chat {
  id: string;
  title: string;
  muted: boolean;
  unread: { markToken: string | null; completionId: string | null };
}

describe("chats: fork, edit and resend, mute, unread", { timeout: 10 * 60_000 }, () => {
  before(async () => {
    app = await launchApp(profile);
  });
  after(async () => {
    await dispose(app);
  });

  test("C1: fork from a reply opens a fork carrying the history", async () => {
    try {
      const source = await newChat(page(), scratchDir("helm-e2e-chat-"));
      await send(page(), "first question for the fork");
      await waitStatus(page(), "idle");
      await page().waitForSelector(tid("transcript-row", '[data-role="assistant"]'), { timeout: 20_000 });
      // The fallback title is the first line.
      const listed = (await api(page(), "chat_list", {})) as Chat[];
      assert.equal(listed.find((c) => c.id === source)?.title, "first question for the fork");
      await page().locator(tid("transcript-row", '[data-role="assistant"]')).last().locator('[data-action="fork-from-here"]').click();
      await page().waitForFunction(
        (src) => {
          const v = document.querySelector('[data-testid="chat-view"]');
          return v && v.getAttribute("data-chat-id") !== src;
        },
        source,
        { timeout: 20_000 },
      );
      await waitRowText(page(), "first question for the fork");
      await waitRowText(page(), "Forked from");
      const forks = (await api(page(), "chat_list", {})) as Chat[];
      assert.ok(forks.some((c) => c.title === "Fork · first question for the fork"), "titled Fork · <source>");
      await shot(page(), OUT, "C1");
    } catch (e) {
      await shot(page(), OUT, "C1-FAILED");
      throw new Error(`${(e as Error).message}\n--- app log tail ---\n${app?.log().slice(-3000)}`);
    }
  });

  test("C2: edit and resend replaces the message and what came after it", async () => {
    try {
      await newChat(page(), scratchDir("helm-e2e-chat-"));
      await send(page(), "the original wording");
      await waitStatus(page(), "idle");
      await page().waitForSelector(tid("transcript-row", '[data-role="assistant"]'), { timeout: 20_000 });
      await page().locator(tid("transcript-row", '[data-role="user"]')).first().locator(tid("edit-message-button")).click();
      await page().waitForSelector(tid("edit-message"), { timeout: 10_000 });
      await page().fill(`${tid("edit-message")} textarea`, "the edited wording");
      await page().click(tid("edit-resend"));
      await waitRowText(page(), "the edited wording");
      await waitStatus(page(), "idle", 30_000);
      // The transcript re-reads its page after the edit is applied; the old branch then leaves it.
      await page().waitForFunction(
        () => ![...document.querySelectorAll('[data-testid="transcript-row"][data-role="user"]')].some((e) => (e.textContent ?? "").includes("the original wording")),
        undefined,
        { timeout: 20_000 },
      );
      await shot(page(), OUT, "C2");
    } catch (e) {
      await shot(page(), OUT, "C2-FAILED");
      throw new Error(`${(e as Error).message}\n--- app log tail ---\n${app?.log().slice(-3000)}`);
    }
  });

  test("C3: unread marks and mute feed the attention summary", async () => {
    try {
      const chat = await newChat(page(), scratchDir("helm-e2e-chat-"));
      const marked = (await api(page(), "unread_mark", { chatId: chat })) as Chat;
      assert.ok(marked.unread.markToken);
      let a = (await api(page(), "attention_get", {})) as { badge: number; unread: { chatId: string; reason: string }[] };
      assert.ok(a.unread.some((r) => r.chatId === chat && r.reason === "Marked unread"));
      const muted = (await api(page(), "chat_mute", { chatId: chat, muted: true })) as Chat;
      assert.equal(muted.muted, true);
      a = (await api(page(), "attention_get", {})) as { badge: number; unread: { chatId: string; reason: string }[] };
      assert.ok(!a.unread.some((r) => r.chatId === chat), "a muted chat leaves the groups");
      await api(page(), "chat_mute", { chatId: chat, muted: false });
      // A stale token never clears the newer mark.
      await api(page(), "unread_mark", { chatId: chat });
      const after = (await api(page(), "unread_clear", { chatId: chat, token: marked.unread.markToken })) as Chat;
      assert.ok(after.unread.markToken && after.unread.markToken !== marked.unread.markToken);
    } catch (e) {
      throw new Error(`${(e as Error).message}\n--- app log tail ---\n${app?.log().slice(-3000)}`);
    }
  });
});
