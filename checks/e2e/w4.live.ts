// Live checks with the real claude and real MCP servers.
// Runs only with HELM_LIVE_CLAUDE=1, HELM_LIVE_MCP_JSON pointing at a .mcp.json that defines
// `agent-standup` and `home-assistant`, and HELM_LIVE_STANDUP_ITEM naming an item on that board.
// Nothing about those servers lives in the repo: the file is read here, its two entries are copied
// into a scratch folder outside the repo, and no address, header or token is printed, logged or
// asserted on.
//
// - L1: Agent Standup is used exactly once, natively. Helm's own client is also given an Agent
//   Standup server; native de-duplication must skip it (status "native"), so the one call is the
//   CLI's own. Read only: one get_item.
// - L2: the Home Assistant MCP is reachable: one read-only list call.
// - L3 (only with HELM_LIVE_HUB_URL and HELM_LIVE_HUB_TOKEN): the hub's memory_search through
//   Helm's own client (app_mcp_call), with the private-network opt-in.
// - L5 (light): the same chat calls app_list_chats through Helm's host tools.
//
// Run: `npm run build:debug`, then
// `HELM_LIVE_CLAUDE=1 HELM_LIVE_MCP_JSON=<path to .mcp.json> npm run live -w checks`.

import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";
import { after, before, describe, test } from "node:test";
import { api, dispose, launchApp, newChat, scratchDir, send, type App } from "./harness.ts";

type Json = Record<string, unknown>;
const enabled = process.env.HELM_LIVE_CLAUDE === "1" && !!process.env.HELM_LIVE_MCP_JSON && !!process.env.HELM_LIVE_STANDUP_ITEM;
const hubUrl = process.env.HELM_LIVE_HUB_URL ?? "";
const hubToken = process.env.HELM_LIVE_HUB_TOKEN ?? "";
/** An existing item on the board the Agent Standup entry points at; get_item reads it once. */
const ITEM = process.env.HELM_LIVE_STANDUP_ITEM ?? "";
let app: App | undefined;
let work = "";
const page = () => app!.page;

function nativeEntries(): Json {
  const all = JSON.parse(fs.readFileSync(process.env.HELM_LIVE_MCP_JSON!, "utf8")) as { mcpServers?: Json };
  const pick: Json = {};
  for (const name of ["agent-standup", "home-assistant"]) {
    const e = all.mcpServers?.[name];
    assert.ok(e, `${name} is defined in HELM_LIVE_MCP_JSON`);
    pick[name] = e;
  }
  return pick;
}

/** Helm's own server entry for the native Agent Standup one (same URL and headers, as literals). */
function helmCopy(id: string, name: string, e: Json): Json {
  const headers: Json = {};
  for (const [k, v] of Object.entries((e.headers ?? {}) as Record<string, string>)) headers[k] = { kind: "literal", value: v };
  return { id, name, enabled: true, allowPrivateHttp: false, transport: { type: "http", url: e.url, headers } };
}

async function saveServer(server: Json): Promise<void> {
  const { revision } = (await api(page(), "mcp_config_get", {})) as { revision: string };
  await api(page(), "mcp_server_save", { revision, server });
}

async function waitIdle(chatId: string, ms: number): Promise<void> {
  // First wait for the turn to start (a send returns before the worker is up).
  const started = Date.now() + 60_000;
  for (;;) {
    const c = (await api(page(), "chat_get", { chatId })) as { chat?: { status?: string } };
    if (c.chat?.status && c.chat.status !== "idle") break;
    if (Date.now() > started) break;
    await new Promise((r) => setTimeout(r, 500));
  }
  const end = Date.now() + ms;
  for (;;) {
    const c = (await api(page(), "chat_get", { chatId })) as { chat?: { status?: string }; status?: string };
    const s = c.chat?.status ?? c.status;
    if (s && !["starting", "running", "stopping"].includes(s)) return;
    if (Date.now() > end) throw new Error(`chat still ${s} after ${ms} ms`);
    await new Promise((r) => setTimeout(r, 2_000));
  }
}

async function toolRows(chatId: string): Promise<{ name: string; status: string; text: string; source?: Json }[]> {
  const p = (await api(page(), "transcript_page", { chatId })) as { messages: Json[] };
  return p.messages
    .filter((m) => m.role === "toolResult")
    .map((m) => {
      let source: Json | undefined;
      try {
        source = (JSON.parse(String(m.details ?? "{}")) as Json).toolSource as Json | undefined;
      } catch {
        source = (m.details as Json | undefined)?.toolSource as Json | undefined;
      }
      return { name: String(m.toolName ?? ""), status: String(m.status ?? ""), text: String(m.text ?? ""), source };
    });
}

describe("live: MCP through a real chat (L1, L2, L3, L5)", {
  skip: enabled ? false : "set HELM_LIVE_CLAUDE=1, HELM_LIVE_MCP_JSON (a .mcp.json with agent-standup and home-assistant) and HELM_LIVE_STANDUP_ITEM (an item id on that board)",
  timeout: 15 * 60_000,
}, () => {
  let chat = "";
  before(async () => {
    work = scratchDir("helm-w4live-work-");
    fs.writeFileSync(path.join(work, ".mcp.json"), JSON.stringify({ mcpServers: nativeEntries() }));
    // The folder's own Claude Code approval for its two servers, which Helm's gate seeds from.
    fs.mkdirSync(path.join(work, ".claude"), { recursive: true });
    fs.writeFileSync(path.join(work, ".claude", "settings.local.json"), JSON.stringify({ enabledMcpjsonServers: ["agent-standup", "home-assistant"] }));
    app = await launchApp(scratchDir("helm-w4live-profile-"), { fake: false, skipPermissions: true });
    // Helm's own client gets Agent Standup too: de-duplication must leave it to the CLI.
    await saveServer(helmCopy("standup", "Agent Standup", nativeEntries()["agent-standup"] as Json));
    if (hubUrl && hubToken) {
      await saveServer({
        id: "hub",
        name: "Helm hub",
        enabled: true,
        allowPrivateHttp: true,
        transport: { type: "http", url: hubUrl, headers: { Authorization: { kind: "literal", value: hubToken, prefix: "Bearer " } } },
      });
    }
  });
  after(async () => {
    await dispose(app);
  });

  test("one real chat: Agent Standup once and natively, Home Assistant reachable, host tools", async () => {
    chat = await newChat(page(), work);
    const hub = hubUrl && hubToken ? " Then call app_mcp_tools with server \"hub\" and use app_mcp_call to run its memory_search tool with query \"notes\" and limit 1." : "";
    await send(
      page(),
      `This is an automated read-only check. Make exactly these tool calls, each once, and change nothing: ` +
        `(1) the agent-standup MCP server's get_item tool with id "${ITEM}"; ` +
        `(2) the home-assistant MCP server's tool that lists entities, with the smallest result it allows; ` +
        `(3) app_list_chats.${hub} Then reply with one short line per call saying whether it worked.`,
    );
    await waitIdle(chat, 8 * 60_000);
    const rows = await toolRows(chat);
    const names = rows.map((r) => r.name);
    // L1: exactly one Agent Standup call, and it is the CLI's own server.
    const standup = rows.filter(
      (r) => r.name.startsWith("mcp__agent-standup__") || (r.name.startsWith("mcp__helm__app_mcp") && /standup/i.test(String(r.source?.server ?? ""))),
    );
    assert.equal(standup.length, 1, `one Agent Standup call; tools: ${names.join(", ")}`);
    assert.ok(standup[0].name.startsWith("mcp__agent-standup__"), `native call, got ${standup[0].name}`);
    assert.equal(standup[0].source?.kind, "mcp");
    const status = (await api(page(), "mcp_servers_status", {})) as Json[] | { servers?: Json[] };
    const list = Array.isArray(status) ? status : (status.servers ?? []);
    const helmStandup = list.find((s) => s.serverId === "standup");
    assert.equal(helmStandup?.state, "native", `Helm's own Agent Standup entry is skipped: ${JSON.stringify(helmStandup)}`);
    // L2: Home Assistant answered.
    const ha = rows.filter((r) => r.name.startsWith("mcp__home-assistant__"));
    assert.ok(ha.length >= 1, `a Home Assistant call; tools: ${names.join(", ")}`);
    assert.notEqual(ha[0].status, "error", "Home Assistant answered");
    // L5: Helm's own host tool.
    const host = rows.find((r) => r.name === "mcp__helm__app_list_chats");
    assert.ok(host && host.status !== "error", `app_list_chats through the helm server; tools: ${names.join(", ")}`);
    assert.equal(host?.source?.kind, "host");
    // L3: the hub through Helm's own client, when configured.
    if (hubUrl && hubToken) {
      const viaHelm = rows.find((r) => r.name === "mcp__helm__app_mcp_call" && r.status !== "error");
      assert.ok(viaHelm, `memory_search through app_mcp_call; tools: ${names.join(", ")}`);
      assert.equal(viaHelm?.source?.kind, "mcp");
    }
    // Report the tool sequence for the PR (names only; no arguments, results or secrets).
    console.log(`LIVE TOOLS: ${names.join(" | ")}`);
    console.log(`LIVE HELM STANDUP STATE: ${String(helmStandup?.state)}`);
  });
});
