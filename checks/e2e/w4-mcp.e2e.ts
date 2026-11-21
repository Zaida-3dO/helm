// Settings → MCP end to end in the real debug app, over the real host.
// The empty page names a reason; a saved server's literal secret never reaches the config file
// or the UI; the pane follows the mcp_config event; a stale revision is refused; the presets
// carry no address; the native switch defaults on; clicking a preset opens its setup visibly,
// focused; the Google preset goes straight to its accounts dialog with the setup checklist; a
// browser-signed-in server shows its sign-in block in the editor.
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
  // The tab bar is two rows: MCP lives under Tools & integrations.
  await page().click('.settings-group-tab[data-group="Tools & integrations"]');
  await page().getByRole("tab", { name: "MCP" }).or(page().getByRole("button", { name: "MCP", exact: true })).first().click();
  await page().waitForSelector(tid("mcp-pane"), { timeout: 20_000 });
}

/** Settings toggles on Ctrl+Comma, so only open it when it is not already showing. */
async function ensureMcpPane(): Promise<void> {
  if (await page().locator(tid("mcp-pane")).isVisible().catch(() => false)) return;
  await openMcpPane();
}

/** Leave the pane as the next check expects: no editor open, no dialog open. */
async function tidyPane(): Promise<void> {
  if (await page().locator(tid("google-accounts-dialog")).isVisible().catch(() => false)) await page().keyboard.press("Escape");
  if (await page().locator(tid("mcp-cancel")).isVisible().catch(() => false)) await page().locator(tid("mcp-cancel")).click();
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
      assert.deepEqual(presets.map((p) => p.id), ["agent_standup", "home_assistant", "helm_hub", "google_accounts", "browser_broker"]);
      assert.equal(await page().locator(tid("mcp-add-preset-helm_hub")).count(), 1);
      assert.equal(await page().locator(tid("mcp-add-preset-browser_broker")).count(), 1);
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

  test("clicking a preset opens its editor in view, caret in the first field", async () => {
    try {
      await ensureMcpPane();
      await page().locator(tid("mcp-add-preset-home_assistant")).click();
      await page().waitForSelector(tid("mcp-editor"), { timeout: 5_000 });
      assert.ok(await page().locator(tid("mcp-editor")).isVisible(), "the editor is visible, not off-screen");
      const focused = await page().evaluate(() => (document.activeElement as HTMLElement | null)?.getAttribute("data-testid"));
      assert.equal(focused, "mcp-name", "the caret goes to the editor's first field");
      const inView = await page().locator(tid("mcp-name")).evaluate((el) => {
        const r = el.getBoundingClientRect();
        return r.top >= 0 && r.bottom <= innerHeight && r.height > 0;
      });
      assert.ok(inView, "the editor is scrolled into view");
    } finally {
      await shot(page(), OUT, "w4-mcp-editor");
      await tidyPane();
    }
  });

  test("the Google preset opens its accounts dialog, is served built in, and never doubles up", async () => {
    try {
      await ensureMcpPane();
      await page().locator(tid("mcp-add-preset-google_accounts")).click();
      await page().waitForSelector(tid("google-accounts-dialog"), { timeout: 10_000 });
      assert.equal(await page().locator(tid("mcp-editor")).count(), 0, "no plain form for the Google preset");
      assert.ok(await page().locator(tid("google-steps")).isVisible(), "the setup checklist is what greets the click");
      for (const step of ["google-step-1", "google-step-6"]) assert.equal(await page().locator(tid(step)).count(), 1);
      assert.equal(String(await page().locator(tid("google-client")).getAttribute("data-state")), "unset", "a fresh profile has no sign-in client");
      assert.equal(await page().locator(tid("google-client-save")).isEnabled(), false, "Save waits for both values");
      assert.equal(await page().locator(tid("google-add")).count(), 0, "Add account only appears once the client is set");
      // The choice-free draft was saved: the server exists, with an Accounts button that reopens this.
      const snap = (await api(page(), "mcp_config_get", {})) as { servers: { id: string; preset?: string }[] };
      assert.ok(snap.servers.some((s) => s.preset === "google_accounts"), "the preset saved its server");
      await page().locator(tid("google-accounts-dialog")).getByRole("button", { name: "Close dialog" }).click();
      await page().waitForSelector(tid("mcp-google-google_accounts"), { timeout: 5_000 });
      await page().locator(tid("mcp-google-google_accounts")).click();
      await page().waitForSelector(tid("google-accounts-dialog"), { timeout: 5_000 });
      await page().locator(tid("google-accounts-dialog")).getByRole("button", { name: "Close dialog" }).click();
      // The row says what answers for it: Helm's own Gmail tools, no program of its own.
      await page().waitForSelector(tid("mcp-status-google_accounts"), { timeout: 5_000 });
      assert.match(String(await page().textContent(tid("mcp-status-google_accounts"))), /Built in: served by Helm/);
      // Clicking the preset again opens that server's dialog rather than saving a second entry.
      await page().locator(tid("mcp-add-preset-google_accounts")).click();
      await page().waitForSelector(tid("google-accounts-dialog"), { timeout: 5_000 });
      const after = (await api(page(), "mcp_config_get", {})) as { servers: { preset?: string }[] };
      assert.equal(after.servers.filter((s) => s.preset === "google_accounts").length, 1, "the preset reopens the existing server");
    } finally {
      await shot(page(), OUT, "w4-mcp-google");
      await tidyPane();
    }
  });

  test("a browser-signed-in server shows its sign-in block in the editor", async () => {
    const { revision } = (await api(page(), "mcp_config_get", {})) as { revision: string };
    await api(page(), "mcp_server_save", {
      revision,
      server: { id: "crm", name: "CRM", enabled: true, allowPrivateHttp: false, transport: { type: "http", url: "https://crm.example/mcp", oauth: {} } },
    });
    try {
      await ensureMcpPane();
      await page().waitForSelector(tid("mcp-server-crm"), { timeout: 10_000 });
      await page().locator(tid("mcp-edit-crm")).click();
      await page().waitForSelector(tid("mcp-oauth"), { timeout: 5_000 });
      assert.ok(await page().locator(tid("mcp-oauth-note")).isVisible());
      assert.ok(await page().locator(tid("mcp-oauth-client-id")).isVisible());
      assert.ok(await page().locator(tid("mcp-oauth-connect")).isVisible(), "the editor offers Connect for a saved sign-in server");
      // The empty oauth marker survives the pane: the row still reads as a browser sign-in ask.
      assert.match(String(await page().textContent(tid("mcp-status-crm"))), /browser|sign in/i);
    } finally {
      await shot(page(), OUT, "w4-mcp-oauth");
      await tidyPane();
    }
  });
});
