// The hub relay end to end: the real debug app keeps its outbound line to a real
// helm-hub (built from hub/), and an outside caller reaches Helm only through the hub's MCP:
//
// - R1  the line comes up from the debug-only test connection, and the get op shows booleans;
// - R2  a vetted read (app_list_chats) runs in Helm and comes back; a refused argument and a
//       tool off the hub's list never run;
// - R3  app_send_message lands in the chat framed as another agent's, never as the user's;
// - R4  the allowed Telegram user is forwarded (no offline reply); a stranger gets nothing;
// - R4b a photo with a caption reaches the Telegram chat as an image the chat can see; an album
//       of other files (a PDF and a GIF) arrives as one message with both saved where the agent
//       can read them; a file over the Bot API's 20 MB download limit is refused in plain words;
// - R5  with Helm gone, a call answers "Helm offline, nothing sent or changed", and with no
//       launcher a Telegram message is queued with the steps to let the hub start Helm next time;
// - R6  (Windows) the always-on launcher, started against the test hub with the same scratch
//       profile, makes the hub start Helm for the queued message, which then arrives; Helm killed
//       outright comes back by itself and its line returns.
//
// Run: `npm run build:debug` and `cargo build -p helm-launcher` at the root and `cargo build` in
// hub/, then `node --test e2e/w7-relay.e2e.ts` in checks/ (or `npm run e2e -w checks`).
// HELM_E2E_HUB_EXE overrides the hub binary, HELM_E2E_LAUNCHER_EXE the launcher.

import assert from "node:assert/strict";
import { spawn, type ChildProcess } from "node:child_process";
import net from "node:net";
import path from "node:path";
import fs from "node:fs";
import { after, before, describe, test } from "node:test";
import { startFakeTelegram, type FakeTelegram } from "../../fakes/telegram/server.ts";
import { api, APP_BROWSER_ARGS, dispose, E2E_FIXTURE, FAKE_CLAUDE, launchApp, newChat, openChat, REPO, scratchDir, tid, waitText, type App } from "./harness.ts";

const HUB_EXE =
  process.env.HELM_E2E_HUB_EXE ?? path.join(REPO, "hub", "target", "debug", process.platform === "win32" ? "helm-hub.exe" : "helm-hub");
const LAUNCHER_EXE = process.env.HELM_E2E_LAUNCHER_EXE ?? path.join(REPO, "target", "debug", "helm-launcher.exe");
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
let hubPort = 0;
let launcher: ChildProcess | undefined;
type Json = Record<string, any>;

/** The process the profile's run record names, and whether it ended in an orderly way. */
function runRecord(): { pid?: number; exited: boolean } {
  try {
    const r = JSON.parse(fs.readFileSync(path.join(profile, "diagnostics", "run.json"), "utf8")) as Json;
    return { pid: typeof r.pid === "number" ? r.pid : undefined, exited: r.exit != null };
  } catch {
    return { exited: false };
  }
}

function alive(pid: number | undefined): boolean {
  if (!pid) return false;
  try {
    process.kill(pid, 0);
    return true;
  } catch {
    return false;
  }
}

/** How many times the hub has logged a PC line coming up (counted as it streams: the kept log is trimmed). */
let lineUpCount = 0;
const lineUps = () => lineUpCount;

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
    hubPort = port;
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
    hub.stderr?.on("data", (d: Buffer) => {
      lineUpCount += d.toString().split("relay: line up").length - 1;
      hubLog = (hubLog + d.toString()).slice(-20_000);
    });
    await until("the hub to listen", async () => (await fetch(`${hubUrl}/health`).then((r) => r.ok).catch(() => false)) || undefined);
    app = await launchApp(profile, {
      env: { HELM_TEST_RELAY_URL: `ws://127.0.0.1:${port}/relay`, HELM_TEST_RELAY_TOKEN: LINE },
    });
  });
  after(async () => {
    await dispose(app);
    // The launcher is our own child; a Helm it started is ended by the process id its record
    // names, never by name.
    launcher?.kill();
    const rec = runRecord();
    if (!rec.exited && alive(rec.pid)) process.kill(rec.pid!);
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

  test("R3: a message from outside arrives framed as an agent's, not the user's", async () => {
    const r = await mcp("app_send_message", { chatId: chat, message: "E2E relay hello", mode: "steer" });
    assert.notEqual(r.isError, true, text(r));
    await waitText(app!.page, "E2E relay hello");
    // This caller's token belongs to the hub's owner, and this PC now knows that person as its
    // home profile's, so Helm frames it as an agent acting for that person by name — still an
    // agent's message, still not the user's. It renders as a compact card; open it to read the
    // whole text the agent received, including the fixed no-approval paragraph.
    await waitText(app!.page, "Message from an agent acting for");
    await app!.page.locator('[data-testid="note-card"][data-kind="peer"] [data-testid="note-card-toggle"]').click();
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

  test("R4b: a photo arrives as an image, an album of files as one message; too large is refused", async () => {
    // A real 1x1 PNG: the chat shows a thumbnail of what arrived.
    const png = Buffer.from(
      "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg==",
      "base64",
    );
    // Let the chat finish its turn first: a message sent into a turn that is still starting is
    // steered in, and its row only shows once Claude reads it (a slow start would time this out).
    const idle = (what: string) =>
      until(
        what,
        async () => {
          const all = (await api(app!.page, "chat_list", {})) as Json[];
          return all.some((c) => String(c.title).startsWith("Telegram ") && c.status === "idle") || undefined;
        },
        90_000,
      );
    await idle("the Telegram chat to finish its first turn");
    tg!.picture(ALLOWED_USER, png, { caption: "what colour is this?", as: "file", mime: "image/png" });
    const telegramChat = await until("the Telegram chat", async () => {
      const all = (await api(app!.page, "chat_list", {})) as Json[];
      return all.find((c) => String(c.title).startsWith("Telegram ")) ?? undefined;
    });
    await openChat(app!.page, String(telegramChat.id));
    await waitText(app!.page, "what colour is this?");
    await app!.page.locator(tid("message-image")).first().waitFor({ timeout: 20_000 });
    // An album: a PDF and a GIF sent together arrive as one message, both saved under their own
    // names where the agent can read them.
    await idle("the Telegram chat to finish the picture's turn");
    const pdf = Buffer.from("%PDF-1.4 e2e");
    const gif = Buffer.concat([Buffer.from("GIF89a"), Buffer.from([1, 0, 1, 0])]);
    tg!.file(ALLOWED_USER, pdf, { name: "report.pdf", mime: "application/pdf", caption: "the e2e album", group: "e2e-album" });
    tg!.file(ALLOWED_USER, gif, { name: "dance.gif", mime: "image/gif", group: "e2e-album" });
    await waitText(app!.page, "the e2e album", 30_000);
    await waitText(app!.page, `[File] report.pdf, ${pdf.length} bytes`);
    await waitText(app!.page, `[File] dance.gif, ${gif.length} bytes`);
    const saved = (name: string) =>
      fs
        .readdirSync(path.join(profile, "attachments"))
        .map((d) => path.join(profile, "attachments", d, name))
        .find((f) => fs.existsSync(f));
    assert.deepEqual(fs.readFileSync(saved("report.pdf")!), pdf);
    assert.deepEqual(fs.readFileSync(saved("dance.gif")!), gif);
    // Over the Bot API's download limit: the user is told why, in plain words, and Helm hears
    // nothing.
    tg!.file(ALLOWED_USER, Buffer.alloc(21 * 1024 * 1024), { name: "big.zip", mime: "application/zip" });
    const told = await until("the refusal on Telegram", () =>
      tg!.chat(ALLOWED_USER).find((m) => m.text.startsWith("big.zip was not sent to Helm: ")),
    );
    assert.match(told.text, /larger than 20 MB, the most a Telegram bot can download/);
  });

  test("R5: with Helm gone, a call answers offline and nothing is sent", async () => {
    await dispose(app);
    app = undefined;
    await until("the line to drop", () => (hubLog.includes("relay: line down") ? true : undefined));
    const r = await mcp("app_send_message", { chatId: chat, message: "should not arrive", mode: "steer" });
    assert.equal(r.isError, true);
    assert.equal(text(r), "Helm offline, nothing sent or changed.");
    // No launcher on this PC: the message is queued, and the reply says how to let the hub start
    // Helm next time.
    const before = tg!.chat(ALLOWED_USER).length;
    tg!.message(ALLOWED_USER, "are you there after the restart");
    const queued = await until("the queued reply", () => tg!.chat(ALLOWED_USER).slice(before).find((m) => m.text.includes("queued")));
    assert.match(queued.text, /Helm is offline and the hub can.t start it/);
    assert.match(queued.text, /Start Helm when a message arrives/);
    assert.match(queued.text, /\/cancel/);
  });

  test("R6: the launcher has the hub start Helm for a queued message, and brings a killed Helm back", { skip: process.platform !== "win32" }, async () => {
    if (!fs.existsSync(LAUNCHER_EXE)) throw new Error(`${LAUNCHER_EXE} not found: run \`cargo build -p helm-launcher\` first`);
    // Helm wrote the launcher's handoff at its start: on, because the test hub counts as paired.
    const handoff = JSON.parse(fs.readFileSync(path.join(profile, "launcher", "launcher.json"), "utf8")) as Json;
    assert.equal(handoff.enabled, true, JSON.stringify(handoff));
    assert.match(String(handoff.helmExe), /helm\.exe$/i);
    // The environment launchApp gives Helm, so the Helm the launcher starts runs the same way
    // (off-screen, the fake claude, the test hub). The launcher passes its own environment on.
    const env: Record<string, string> = {};
    for (const [k, v] of Object.entries(process.env)) if (v !== undefined && k.toUpperCase() !== "CLAUDECODE") env[k] = v;
    Object.assign(env, {
      HELM_PROFILE_DIR: profile,
      WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS: APP_BROWSER_ARGS,
      HELM_TEST_CONFIRM: "ok",
      HELM_TEST_WINDOW: "background",
      HELM_TEST_LOCAL_SERVERS: "off",
      HELM_TEST_CLAUDE: JSON.stringify({ command: process.execPath, args: [FAKE_CLAUDE, E2E_FIXTURE] }),
      FAKE_CLAUDE_LOG: path.join(profile, "fake-claude.log"),
      HELM_TEST_RELAY_URL: `ws://127.0.0.1:${hubPort}/relay`,
      HELM_TEST_RELAY_TOKEN: LINE,
    });
    const ups = lineUps();
    launcher = spawn(LAUNCHER_EXE, ["--profile", profile], { env, stdio: "ignore" });
    // The launcher's hello says Helm is not running while a Telegram message waits: the hub asks
    // it to start Helm, Helm's line comes up, and the queued message is delivered.
    await until("Helm started by the launcher to reconnect", () => (lineUps() > ups ? true : undefined), 120_000);
    const claudeLog = path.join(profile, "fake-claude.log");
    await until(
      "the queued message to reach a chat",
      () => (fs.existsSync(claudeLog) && fs.readFileSync(claudeLog, "utf8").includes("are you there after the restart") ? true : undefined),
      120_000,
    );
    const started = runRecord();
    assert.ok(alive(started.pid) && !started.exited, `a running Helm: ${JSON.stringify(started)}`);
    // Killed outright (a crash): the launcher waits a little and starts it again.
    const ups2 = lineUps();
    process.kill(started.pid!);
    await until(
      "a new Helm after the crash",
      () => {
        const now = runRecord();
        return now.pid !== started.pid && alive(now.pid) ? true : undefined;
      },
      90_000,
    );
    await until("its line to come back", () => (lineUps() > ups2 ? true : undefined), 120_000);
    assert.equal(launcher.exitCode, null, "the launcher keeps running");
  });
});
