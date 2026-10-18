// Host tools end to end in the real debug app with the fake claude
// (fixtures/w4-hosttools.json): a chat calls Helm's host tools through the in-process helm MCP
// server and the request channel; the settings toast offers Undo; the Allow card ignores a click
// before its settle window and decides after it; the decision reaches the agent once, as a note
// from Helm.
//
// Run: `npm run build:debug` at the root, then `npm run e2e -w checks`.

import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { after, before, describe, test } from "node:test";
import { api, dispose, FAKE_CLAUDE, launchApp, newChat, scratchDir, send, shot, tid, waitStatus, type App } from "./harness.ts";

const FIXTURE = path.join(path.dirname(fileURLToPath(import.meta.url)), "fixtures", "w4-hosttools.json");
const OUT = process.env.HELM_E2E_OUT ?? scratchDir("helm-e2e-out-");
const profile = scratchDir("helm-e2e-profile-");
const work = scratchDir("helm-e2e-work-");
let app: App | undefined;
const page = () => app!.page;
type Json = Record<string, unknown>;

function fakeIn(): Json[] {
  const p = path.join(profile, "fake-claude.log");
  if (!fs.existsSync(p)) return [];
  return fs
    .readFileSync(p, "utf8")
    .split("\n")
    .filter(Boolean)
    .map((l) => JSON.parse(l) as Json)
    .filter((e) => e.kind === "in")
    .map((e) => e.rec as Json);
}

/** The helm server's answer to one tools/call, by JSON-RPC id. */
function mcpAnswer(id: number): Json | undefined {
  for (const r of fakeIn()) {
    const resp = ((r.response as Json | undefined)?.response as Json | undefined)?.mcp_response as Json | undefined;
    if (r.type === "control_response" && resp?.id === id) return resp;
  }
  return undefined;
}

async function until<T>(what: string, f: () => T | undefined | Promise<T | undefined>, ms = 20_000): Promise<T> {
  const end = Date.now() + ms;
  for (;;) {
    const v = await f();
    if (v !== undefined && v !== null && v !== false) return v as T;
    if (Date.now() > end) throw new Error(`timed out waiting for ${what}`);
    await new Promise((r) => setTimeout(r, 100));
  }
}

describe("host tools, the settings toast and the Allow card", { timeout: 10 * 60_000 }, () => {
  let chat = "";
  before(async () => {
    app = await launchApp(profile, {
      env: { HELM_TEST_CLAUDE: JSON.stringify({ command: process.execPath, args: [FAKE_CLAUDE, FIXTURE] }) },
    });
  });
  after(async () => {
    await dispose(app);
  });

  test("H1: a host-tool round-trip through the helm MCP server", async () => {
    try {
      chat = await newChat(page(), work);
      await send(page(), "[HT-TOOLS] use Helm's tools");
      const list = await until("the app_list_chats answer", () => mcpAnswer(2));
      const res = list.result as Json;
      assert.notEqual(res.isError, true, JSON.stringify(res));
      const chats = ((res.structuredContent as Json).chats as Json[]).map((c) => c.chatId);
      assert.ok(chats.includes(chat), "the caller is listed with includeCurrent");
      const tools = (((mcpAnswer(1)?.result as Json).tools as Json[]) ?? []).map((t) => t.name);
      for (const t of ["app_list_chats", "app_update_settings", "app_request_allow", "app_list_tools", "app_mcp_call"]) assert.ok(tools.includes(t), t);
      // Native MCP stays on: no --strict-mcp-config, and the helm SDK server is on the CLI line.
      const start = fs
        .readFileSync(path.join(profile, "fake-claude.log"), "utf8")
        .split("\n")
        .filter(Boolean)
        .map((l) => JSON.parse(l) as Json)
        // The chat worker's claude, not a titles or autocomplete helper run.
        .find((e) => e.kind === "start" && ((e.argv ?? []) as string[]).includes("--permission-prompt-tool"));
      const argv = (start?.argv ?? []) as string[];
      assert.ok(argv.includes("--mcp-config") && !argv.includes("--strict-mcp-config"));
    } finally {
      await shot(page(), OUT, "w4-hosttools-roundtrip");
    }
  });

  test("H2: a chat's settings change shows the 8 s toast, and Undo restores the value", async () => {
    try {
      const set = await until("the app_update_settings answer", () => mcpAnswer(3));
      assert.equal(((set.result as Json).structuredContent as Json).applied && true, true, JSON.stringify(set));
      await page().waitForSelector(tid("settings-toast"), { timeout: 15_000 });
      assert.match(String(await page().textContent(tid("settings-toast"))), /compactSidebar/);
      assert.equal(await api(page(), "preference_get", { key: "compactSidebar" }), true);
      await page().locator(tid("settings-toast")).getByRole("button", { name: "Undo" }).click();
      await until("the value restored", async () => (await api(page(), "preference_get", { key: "compactSidebar" })) === null || undefined);
      assert.equal(await api(page(), "preference_get", { key: "compactSidebar" }), null);
    } finally {
      await shot(page(), OUT, "w4-hosttools-toast");
    }
  });

  test("H3: the Allow card ignores an early click, decides after the settle window, and the agent hears once", async () => {
    try {
      const req = await until("the app_request_allow answer", () => mcpAnswer(4));
      assert.equal(((req.result as Json).structuredContent as Json).state, "pending", JSON.stringify(req));
      await waitStatus(page(), ["Idle", "idle", "Done", "Ready"], 30_000).catch(() => undefined);
      // Re-mount the dock (switch away and back) so the card is fresh, then click at once.
      await page().reload();
      await page().waitForSelector(tid("chat-item", `[data-chat-id="${chat}"]`), { timeout: 30_000 });
      await page().click(tid("chat-item", `[data-chat-id="${chat}"]`));
      await page().waitForSelector(tid("allow-allow"), { timeout: 20_000 });
      const early = await page().evaluate(() => {
        const b = document.querySelector('[data-testid="allow-allow"]') as HTMLButtonElement;
        b.click();
        return true;
      });
      assert.ok(early);
      await new Promise((r) => setTimeout(r, 300));
      assert.equal(await page().locator(tid("allow-card")).count(), 1, "a click within 400 ms decides nothing");
      await new Promise((r) => setTimeout(r, 300));
      await page().click(tid("allow-allow"));
      await page().waitForSelector(tid("allow-card"), { state: "detached", timeout: 10_000 });
      const list = (await api(page(), "consent_list", { chatId: chat })) as { pending: unknown[]; decided: Json[] };
      assert.equal(list.pending.length, 0);
      assert.equal(list.decided[0].state, "allowed");
      // The decision reaches the agent as a note from Helm, exactly once.
      const notes = () => fakeIn().filter((r) => r.type === "user" && JSON.stringify(r).includes("[Helm] The user allowed"));
      await until("the decision note", () => (notes().length > 0 ? true : undefined), 90_000);
      await new Promise((r) => setTimeout(r, 2_000));
      assert.equal(notes().length, 1);
      assert.deepEqual(await app!.cspViolations(), []);
    } finally {
      await shot(page(), OUT, "w4-hosttools-allow");
    }
  });
});
