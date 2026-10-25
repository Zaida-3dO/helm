// Telegram into chats end to end. The real debug app keeps its line to a
// real helm-hub (built from hub/), whose bot talks to the fake Telegram API:
//
// - T1  the first message starts "Telegram <bot> <date>"; the turn's answer comes back as a NEW
//       message (not silent, so the phone notifies) and no status message is left behind;
// - T2  the next message attaches to the same chat; no second Telegram chat appears;
// - T3  while a turn runs, ONE status message is edited in place and names the running tool; a
//       message sent meanwhile steers in without aborting the turn; /stop stops it, and the
//       status goes once the final note arrives;
// - T4  Settings → Hub shows the line connected and the Telegram chat; "Start a new chat" drops
//       the tag, so the next message, posted by a forwarder to the hub's inbound endpoint (a bot
//       shared with another service), starts a new chat, and its bubble shows the Telegram badge
//       without the note for the agent;
// - T5  while the user is away, a chat that finishes a reply sends one "While you're away" line
//       to the phone (Helm speaking first), and nothing is sent once they are back.
//
// Origin `telegram` and "never consent" are pinned in Rust (helm-relay sink tests) through the
// real send path. Run: `npm run build:debug` at the root and `cargo build` in hub/, then
// `node --test e2e/w7-telegram-seam.e2e.ts` in checks/. HELM_E2E_HUB_EXE overrides the hub binary.

import assert from "node:assert/strict";
import { spawn, type ChildProcess } from "node:child_process";
import net from "node:net";
import path from "node:path";
import { after, before, describe, test } from "node:test";
import { BOT_USERNAME, startFakeTelegram, type FakeTelegram } from "../../fakes/telegram/server.ts";
import { api, dispose, launchApp, REPO, rows, scratchDir, tid, type App } from "./harness.ts";

const HUB_EXE =
  process.env.HELM_E2E_HUB_EXE ?? path.join(REPO, "hub", "target", "debug", process.platform === "win32" ? "helm-hub.exe" : "helm-hub");
// Made-up test tokens, used only between this test's own processes.
const LINE = "e2e-line-token-0123456789abcdefgh";
const FORWARDER = "e2e-forwarder-token-0123456789abcd";
const ALLOWED_USER = 4242;

const profile = scratchDir("helm-e2e-profile-");
let app: App | undefined;
let hub: ChildProcess | undefined;
let hubLog = "";
let tg: FakeTelegram | undefined;
let hubUrl = "";
type Json = Record<string, any>;

async function freePort(): Promise<number> {
  return new Promise((resolve) => {
    const s = net.createServer();
    s.listen(0, "127.0.0.1", () => {
      const p = (s.address() as net.AddressInfo).port;
      s.close(() => resolve(p));
    });
  });
}

async function until<T>(what: string, f: () => T | undefined | Promise<T | undefined>, ms = 60_000): Promise<T> {
  const end = Date.now() + ms;
  for (;;) {
    const v = await f();
    if (v !== undefined && v !== null && v !== false) return v as T;
    if (Date.now() > end) {
      throw new Error(`timed out waiting for ${what}; telegram: ${JSON.stringify(tg?.sent.slice(-10))}; hub log: ${hubLog.slice(-1500)}; app log: ${app?.log().slice(-1500)}`);
    }
    await new Promise((r) => setTimeout(r, 200));
  }
}

async function telegramChats(): Promise<Json[]> {
  const all = (await api(app!.page, "chat_list", {})) as Json[];
  return all.filter((c) => String(c.title).startsWith(`Telegram ${BOT_USERNAME} `));
}

const phone = () => tg!.chat(ALLOWED_USER).map((m) => m.text);

describe("Telegram messages into chats, and the turn mirrored back", { timeout: 10 * 60_000 }, () => {
  let chatId = "";
  before(async () => {
    tg = await startFakeTelegram();
    const port = await freePort();
    hub = spawn(HUB_EXE, ["serve"], {
      env: {
        ...process.env,
        HUB_DB: path.join(scratchDir("helm-e2e-hub-"), "hub.db"),
        HUB_BIND: `127.0.0.1:${port}`,
        HUB_TOKENS: `helm-line:${LINE}:relay,forwarder:${FORWARDER}:telegram`,
        HUB_TELEGRAM_BOT_TOKEN: tg.token,
        HUB_TELEGRAM_ALLOWED_USERS: String(ALLOWED_USER),
        HUB_TELEGRAM_API: tg.url,
      },
      stdio: ["ignore", "pipe", "pipe"],
    });
    hub.stderr?.on("data", (d: Buffer) => (hubLog = (hubLog + d.toString()).slice(-20_000)));
    hubUrl = `http://127.0.0.1:${port}`;
    await until("the hub to listen", async () => (await fetch(`http://127.0.0.1:${port}/health`).then((r) => r.ok).catch(() => false)) || undefined);
    app = await launchApp(profile, {
      env: { HELM_TEST_RELAY_URL: `ws://127.0.0.1:${port}/relay`, HELM_TEST_RELAY_TOKEN: LINE },
    });
    await until("the line to come up", () => (hubLog.includes("relay: line up") ? true : undefined));
  });
  after(async () => {
    await dispose(app);
    hub?.kill();
    await tg?.close();
  });

  test("T1: the first message starts the Telegram chat and its answer arrives as a new message", async () => {
    const before = tg!.sent.length;
    tg!.message(ALLOWED_USER, "hello from the phone");
    const chats = await until("the Telegram chat", async () => {
      const c = await telegramChats();
      return c.length ? c : undefined;
    });
    assert.equal(chats.length, 1);
    assert.match(String(chats[0].title), new RegExp(`^Telegram ${BOT_USERNAME} \\d{4}-\\d{2}-\\d{2}$`));
    chatId = chats[0].id;
    await until("the answer on the phone", () => (phone().some((t) => t.includes("You said:") && t.includes("hello from the phone")) ? true : undefined));
    const answer = tg!.sent.slice(before).find((c) => c.method === "sendMessage" && (c.text ?? "").includes("You said:"));
    assert.ok(answer, "sent as a new message");
    assert.notEqual(answer!.silent, true, "the answer notifies");
    // Only the final state stays: no "Working…" message left on the phone.
    await until("the status to go", () => (phone().every((t) => !t.startsWith("Working")) ? true : undefined), 10_000);
  });

  test("T2: the next message attaches to the same chat", async () => {
    tg!.message(ALLOWED_USER, "second message");
    await until("the second answer", () => (phone().some((t) => t.includes("second message")) ? true : undefined));
    const chats = await telegramChats();
    assert.equal(chats.length, 1, "no second Telegram chat");
    assert.equal(chats[0].id, chatId);
  });

  test("T3: one live status while running; a steer does not abort; /stop stops", async () => {
    const mark = tg!.sent.length;
    tg!.message(ALLOWED_USER, "[E8-run] start the long one");
    await until("a status naming the tool", () => (phone().some((t) => t.startsWith("Working") && t.includes("Bash")) ? true : undefined));
    const statuses = () => phone().filter((t) => t.startsWith("Working"));
    assert.equal(statuses().length, 1, "one status message");
    tg!.message(ALLOWED_USER, "a steer while it runs");
    // The steer is stored and the turn keeps running: never an abort.
    await new Promise((r) => setTimeout(r, 3_000));
    const c = (await telegramChats())[0];
    assert.equal(c.status, "running", `still running after the steer: ${c.status}`);
    assert.equal(statuses().length, 1, "still one status message");
    tg!.message(ALLOWED_USER, "/stop");
    await until("the chat to stop", async () => {
      const s = (await telegramChats())[0]?.status;
      return s && s !== "running" && s !== "starting" && s !== "stopping" ? true : undefined;
    });
    await until("the status to go after the final", () => (statuses().length === 0 ? true : undefined), 20_000);
    const edits = tg!.sent.slice(mark).filter((x) => x.method === "editMessageText").length;
    const sends = tg!.sent.slice(mark).filter((x) => x.method === "sendMessage" && (x.text ?? "").startsWith("Working")).length;
    assert.ok(sends <= 1, `the status is edited in place, not re-sent (${sends} sends, ${edits} edits)`);
  });

  test("T4: Settings → Hub, a fresh chat, a forwarded message and the Telegram badge", async () => {
    const page = app!.page;
    await page.keyboard.press("Control+Comma");
    await page.click('.settings-tab[data-tab="hub"]');
    await page.waitForSelector(tid("hub-state", '[data-state="online"]'), { timeout: 15_000 });
    await page.waitForSelector(tid("hub-telegram-reset"), { timeout: 10_000 });
    await page.click(tid("hub-telegram-reset"));
    await until("the tag to drop", async () => ((await api(page, "relay_telegram_get", {})) as Json).chat === null || undefined);
    await page.keyboard.press("Escape");
    // A forwarder on a shared bot posts the update it received; "/helm" routes it to Helm.
    const update = {
      update_id: 9_000_001,
      message: { message_id: 9_001, date: Math.floor(Date.now() / 1000), text: "/helm from the shared bot", from: { id: ALLOWED_USER }, chat: { id: ALLOWED_USER, type: "private" } },
    };
    const post = (token: string) =>
      fetch(`${hubUrl}/telegram/update`, { method: "POST", headers: { authorization: `Bearer ${token}`, "content-type": "application/json" }, body: JSON.stringify(update) });
    assert.equal((await post(LINE)).status, 403, "the line token cannot post updates");
    assert.equal((await post(FORWARDER)).status, 200);
    await until("the answer on the phone", () => (phone().some((t) => t.includes("You said:") && t.includes("from the shared bot")) ? true : undefined));
    const chats = await telegramChats();
    assert.equal(chats.length, 2, "a new Telegram chat after the reset");
    const fresh = chats.find((c) => c.id !== chatId)!;
    // Settings → Hub now names the new chat, and Open goes to it.
    await page.keyboard.press("Control+Comma");
    await page.click('.settings-tab[data-tab="hub"]');
    await page.locator(tid("hub-telegram-chat")).getByRole("button", { name: /^Open / }).click();
    await page.waitForSelector(tid("chat-view", `[data-chat-id="${fresh.id}"]`), { timeout: 15_000 });
    const user = rows(page, "user").first();
    await user.locator(tid("row-origin", '[data-origin="telegram"]')).waitFor({ timeout: 15_000 });
    const shown = String(await user.locator(".bubble").textContent());
    assert.match(shown, /from the shared bot/);
    assert.doesNotMatch(shown, /Sent from Telegram|\/helm/, "the note for the agent and the routing command stay hidden");
  });

  test("T5: away alerts reach the phone while the user is away", async () => {
    const page = app!.page;
    await page.keyboard.press("Escape");
    await api(page, "afk_settings", { settings: { enabled: true } });
    await api(page, "afk_set", { on: true });
    // The first look of an away span only records what already waits (it runs every 20 s).
    await new Promise((r) => setTimeout(r, 25_000));
    const mark = phone().length;
    const chat = (await api(page, "chat_create", { cwd: scratchDir("helm-e2e-away-") })) as Json;
    await api(page, "chat_send", { chatId: chat.id, submissionId: crypto.randomUUID(), text: "done while you were away", action: "default", draftRevision: 0 });
    const alert = await until("the away alert", () => phone().slice(mark).find((t) => t.startsWith("While you're away:")), 90_000);
    assert.equal(alert.split("\n").length, 2, `one line, for the one chat that needs you: ${alert}`);
    await api(page, "afk_set", { on: false });
  });
});
