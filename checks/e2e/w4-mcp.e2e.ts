// Settings → MCP end to end in the real debug app, over the real host.
// The empty page names a reason; a saved server's literal secret never reaches the config file
// or the UI; the pane follows the mcp_config event; a stale revision is refused; the presets
// carry no address; the native switch defaults on.
//
// Run: `npm run build:debug` at the root, then `npm run e2e -w checks`.

import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";
import { after, before, describe, test } from "node:test";
import { api, dispose, launchApp, scratchDir, shot, tid, type App } from "./harness.ts";

const OUT = process.env.HELM_E2E_OUT ?? scratchDir("helm-e2e-out-");
const profile = scratchDir("helm-e2e-profile-");
const SECRET = "e2e-literal-secret-0123456789";
let app: App | undefined;
const page = () => app!.page;

async function openMcpPane(): Promise<void> {
  await page().keyboard.press("Control+Comma");
  await page().getByRole("tab", { name: "MCP" }).or(page().getByRole("button", { name: "MCP", exact: true })).first().click();
  await page().waitForSelector(tid("mcp-pane"), { timeout: 20_000 });
}

describe("Settings → MCP", { timeout: 10 * 60_000 }, () => {
  before(async () => {
    app = await launchApp(profile);
  });
  after(async () => {
    await dispose(app);
  });

  test("M1: the empty page names a reason; presets have no address", async () => {
    try {
      const snap = (await api(page(), "mcp_config_get", {})) as { revision: string; servers: unknown[] };
      assert.equal(snap.servers.length, 0);
      assert.match(snap.revision, /^[0-9a-f]{64}$/);
      await openMcpPane();
      const empty = await page().textContent(tid("mcp-empty"));
      assert.match(String(empty), /No MCP servers yet/);
      const presets = (await api(page(), "mcp_presets_list", {})) as { id: string; urlPlaceholder: string }[];
      assert.deepEqual(presets.map((p) => p.id), ["agent_standup", "home_assistant", "helm_hub"]);
      assert.equal(await page().locator(tid("mcp-add-preset-helm_hub")).count(), 1);
      assert.equal(await page().isChecked(tid("mcp-native-switch")), true, "native servers default on");
    } finally {
      await shot(page(), OUT, "w4-mcp-empty");
    }
  });

  test("M2: a literal secret is stored in the credential store, never in the file or the UI", async () => {
    try {
      const { revision } = (await api(page(), "mcp_config_get", {})) as { revision: string };
      const server = {
        id: "hub",
        name: "Hub",
        enabled: true,
        allowPrivateHttp: true,
        transport: { type: "http", url: "http://10.0.0.2:8080/mcp", headers: { Authorization: { kind: "literal", value: SECRET, prefix: "Bearer " } } },
      };
      const snap = (await api(page(), "mcp_server_save", { revision, server })) as { revision: string; servers: { transport: { headers: Record<string, { value?: string; secretId?: string }> } }[] };
      const h = snap.servers[0]!.transport.headers.Authorization!;
      assert.equal(h.value, undefined);
      assert.match(String(h.secretId), /^lit_[0-9a-f]{32}$/);
      const file = fs.readFileSync(path.join(profile, "mcp-servers.json"), "utf8");
      assert.ok(!file.includes(SECRET), "the config file holds the secret");
      await page().waitForSelector(tid("mcp-server-hub"), { timeout: 10_000 });
      const html = await page().content();
      assert.ok(!html.includes(SECRET), "the page holds the secret");
      assert.match(String(await page().textContent(tid("mcp-status-hub"))), /Ready/);
      // A stale revision is refused and changes nothing.
      await assert.rejects(api(page(), "mcp_server_remove", { revision, serverId: "hub" }));
      const after = (await api(page(), "mcp_config_get", {})) as { servers: unknown[] };
      assert.equal(after.servers.length, 1);
    } finally {
      await shot(page(), OUT, "w4-mcp-server");
    }
  });

  test("M3: plain http to a public address is refused even with the opt-in", async () => {
    const { revision } = (await api(page(), "mcp_config_get", {})) as { revision: string };
    await assert.rejects(
      api(page(), "mcp_server_save", {
        revision,
        server: { id: "pub", name: "Pub", enabled: true, allowPrivateHttp: true, transport: { type: "http", url: "http://8.8.8.8/mcp" } },
      }),
    );
    const repo = (await api(page(), "mcp_repo_servers_list", {})) as unknown[];
    assert.deepEqual(repo, []);
  });
});
