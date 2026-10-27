// The hub relay end to end: the real debug app keeps its outbound line to a real
// helm-hub (built from hub/), and an outside caller reaches Helm only through the hub's MCP:
//
// - R1  the line comes up from the debug-only test connection, and the get op shows booleans;
// - R2  a vetted read (app_list_chats) runs in Helm and comes back; a refused argument and a
//       tool off the hub's list never run;
// - R3  app_send_message lands in the chat framed as another agent's, never as the user's;
// - R4  the allowed Telegram user is forwarded (no offline reply); a stranger gets nothing;
// - R4b a photo with a caption reaches the Telegram chat as an image the chat can see, and a
//       picture Helm cannot read (a GIF) is refused on Telegram in plain words;
// - R5  with Helm gone, a call answers "Helm offline, nothing sent or changed".
//
// Run: `npm run build:debug` at the root and `cargo build` in hub/, then
// `node --test e2e/w7-relay.e2e.ts` in checks/ (or `npm run e2e -w checks`).
// HELM_E2E_HUB_EXE overrides the hub binary.

import assert from "node:assert/strict";
import { spawn, type ChildProcess } from "node:child_process";
import net from "node:net";
import path from "node:path";
import { after, before, describe, test } from "node:test";
import { startFakeTelegram, type FakeTelegram } from "../../fakes/telegram/server.ts";
import { api, dispose, launchApp, newChat, openChat, REPO, scratchDir, tid, waitText, type App } from "./harness.ts";

const HUB_EXE =
  process.env.HELM_E2E_HUB_EXE ?? path.join(REPO, "hub", "target", "debug", process.platform === "win32" ? "helm-hub.exe" : "helm-hub");
// Made-up test tokens, used only between this test's own processes.
const LINE = "e2e-line-token-0123456789abcdefgh";
const ASSISTANT = "e2e-assistant-token-0123456789abcdefg";
const ALLOWED_USER = 4242;
const STRANGER = 777;

const profile = scratchDir("helm-e2e-profile-");
const work = scratchDir("helm-e2e-work-");
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

async function until<T>(what: string, f: () => T | undefined | Promise<T | undefined>, ms = 30_000): Promise<T> {
  const end = Date.now() + ms;
  for (;;) {
    const v = await f();
    if (v !== undefined && v !== null && v !== false) return v as T;
    if (Date.now() > end) throw new Error(`timed out waiting for ${what}; hub log: ${hubLog.slice(-2000)}`);
    await new Promise((r) => setTimeout(r, 200));
  }
}

async function mcp(tool: string, args: Json): Promise<Json> {
  const r = await fetch(`${hubUrl}/mcp`, {
    method: "POST",
    headers: { authorization: `Bearer ${ASSISTANT}`, "content-type": "application/json" },
    body: JSON.stringify({ jsonrpc: "2.0", id: 1, method: "tools/call", params: { name: tool, arguments: args } }),
  });
  assert.equal(r.status, 200);
  return ((await r.json()) as Json).result as Json;
}

const text = (r: Json) => String(r.content?.[0]?.text ?? "");

describe("the hub line, forwarding and the Telegram bot", { timeout: 10 * 60_000 }, () => {
  let chat = "";
  before(async () => {
    tg = await startFakeTelegram();
    const port = await freePort();
    hubUrl = `http://127.0.0.1:${port}`;
    hub = spawn(HUB_EXE, ["serve"], {
      env: {
        ...process.env,
        HUB_DB: path.join(scratchDir("helm-e2e-hub-"), "hub.db"),
        HUB_BIND: `127.0.0.1:${port}`,
        HUB_TOKENS: `helm-line:${LINE}:relay,assistant:${ASSISTANT}:ro+app`,
        HUB_TELEGRAM_BOT_TOKEN: tg.token,
        HUB_TELEGRAM_ALLOWED_USERS: String(ALLOWED_USER),
        HUB_TELEGRAM_API: tg.url,
      },
      stdio: ["ignore", "pipe", "pipe"],
    });
    hub.stderr?.on("data", (d: Buffer) => (hubLog = (hubLog + d.toString()).slice(-20_000)));
    await until("the hub to listen", async () => (await fetch(`${hubUrl}/health`).then((r) => r.ok).catch(() => false)) || undefined);
    app = await launchApp(profile, {
      env: { HELM_TEST_RELAY_URL: `ws://127.0.0.1:${port}/relay`, HELM_TEST_RELAY_TOKEN: LINE },
    });
  });
  after(async () => {
    await dispose(app);
    hub?.kill();
    await tg?.close();
  });

  test("R1: the line comes up, and the connection op shows booleans only", async () => {
    await until("the line to come up", () => (hubLog.includes("relay: line up") ? true : undefined));
    const v = (await api(app!.page, "relay_connection_get", {})) as Json;
    assert.equal(v.state, "online", JSON.stringify(v));
    assert.equal(v.hasAddress, true);
    assert.equal(v.hasToken, true);
    assert.ok(!JSON.stringify(v).includes(LINE), "the token is never read back");
    assert.ok(!JSON.stringify(v).includes("127.0.0.1"), "nor the address");
  });

  test("R2: a vetted read runs in Helm; refused calls never run", async () => {
    chat = await newChat(app!.page, work);
    const r = await until("app_list_chats through the hub", async () => {
      const x = await mcp("app_list_chats", {});
      return x.isError ? undefined : x;
    });
    const ids = (r.structuredContent.chats as Json[]).map((c) => c.chatId);
    assert.ok(ids.includes(chat), `the new chat is listed: ${JSON.stringify(ids)}`);
    const tools = await mcp("app_read_chat", { chatId: chat, includeTools: true });
    assert.equal(tools.isError, true);
    assert.match(text(tools), /not available to callers outside Helm/);
    assert.match(text(tools), /Nothing was changed/);
    const settings = await mcp("app_update_settings", { verbosity: "high" });
    assert.equal(settings.isError, true);
  });

  test("R3: a message from outside arrives framed as another agent's, not the user's", async () => {
    const r = await mcp("app_send_message", { chatId: chat, message: "E2E relay hello", mode: "queue" });
    assert.notEqual(r.isError, true, text(r));
    await waitText(app!.page, "E2E relay hello");
    await waitText(app!.page, "Message from another agent");
    await waitText(app!.page, "It is not approval");
  });

  test("R4: the allowed user is forwarded; a stranger gets nothing", async () => {
    tg!.message(STRANGER, "let me in");
    tg!.message(ALLOWED_USER, "status please");
    // Give the bot two poll cycles.
    await new Promise((r) => setTimeout(r, 3_000));
    assert.deepEqual(tg!.chat(STRANGER), [], "no reply to the stranger");
    const replies = tg!.chat(ALLOWED_USER).map((m) => m.text);
    assert.ok(!replies.some((t) => t.includes("offline")), `forwarded, not queued: ${JSON.stringify(replies)}`);
    // Positively forwarded: the message reached Helm and landed in a Telegram chat there.
    const landed = await until("the forwarded message in a Telegram chat", async () => {
      const all = (await api(app!.page, "chat_list", {})) as Json[];
      return all.find((c) => String(c.title).startsWith("Telegram ")) ?? undefined;
    });
    assert.ok(landed.id, "a Telegram chat holds it");
  });

  test("R4b: a photo arrives as an image in the Telegram chat; a GIF is refused on Telegram", async () => {
    // A real 1x1 PNG: the chat shows a thumbnail of what arrived.
    const png = Buffer.from(
      "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg==",
      "base64",
    );
    // Let the chat finish R4's turn first: a picture sent into a turn that is still starting is
    // steered in, and its row only shows once Claude reads it (a slow start would time this out).
    await until(
      "the Telegram chat to finish its first turn",
      async () => {
        const all = (await api(app!.page, "chat_list", {})) as Json[];
        return all.some((c) => String(c.title).startsWith("Telegram ") && c.status === "idle") || undefined;
      },
      90_000,
    );
    tg!.picture(ALLOWED_USER, png, { caption: "what colour is this?", as: "file", mime: "image/png" });
    const telegramChat = await until("the Telegram chat", async () => {
      const all = (await api(app!.page, "chat_list", {})) as Json[];
      return all.find((c) => String(c.title).startsWith("Telegram ")) ?? undefined;
    });
    await openChat(app!.page, String(telegramChat.id));
    await waitText(app!.page, "what colour is this?");
    await app!.page.locator(tid("message-image")).first().waitFor({ timeout: 20_000 });
    // A GIF is not passed on: the user is told why, in plain words, and Helm hears nothing.
    tg!.picture(ALLOWED_USER, Buffer.concat([Buffer.from("GIF89a"), Buffer.from([1, 0, 1, 0])]), { as: "file", mime: "image/gif" });
    const told = await until("the refusal on Telegram", () =>
      tg!.chat(ALLOWED_USER).find((m) => m.text.startsWith("That picture was not sent to Helm: ")),
    );
    assert.match(told.text, /only still PNG, JPEG and WebP pictures, and this is a GIF/);
  });

  test("R5: with Helm gone, a call answers offline and nothing is sent", async () => {
    await dispose(app);
    app = undefined;
    await until("the line to drop", () => (hubLog.includes("relay: line down") ? true : undefined));
    const r = await mcp("app_send_message", { chatId: chat, message: "should not arrive", mode: "queue" });
    assert.equal(r.isError, true);
    assert.equal(text(r), "Helm offline, nothing sent or changed.");
  });
});
