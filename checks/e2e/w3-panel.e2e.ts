// The side panel end to end in the real debug app: the Changes slot on a real temporary git
// repository (sections, a stage click), the path gate and private roots on the host, a vault note
// through HELM_TEST_OBSIDIAN_CONFIG, the file viewer at a line, quick-open on Ctrl+P, and no CSP
// violation.
//
// Run: `npm run build:debug` at the root, then `npm run e2e -w checks`. Needs git on PATH.

import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import fs from "node:fs";
import path from "node:path";
import { after, before, describe, test } from "node:test";
import { api, dispose, launchApp, newChat, scratchDir, shot, tid, type App } from "./harness.ts";

/** The `{code, message}` an op refuses with, or null when it succeeds. */
async function apiError(op: string, args: Record<string, unknown>): Promise<{ code?: string; message?: string } | null> {
  return page().evaluate(
    async ([o, a]) => {
      try {
        await (window as unknown as { __TAURI_INTERNALS__: { invoke: (c: string, p: unknown) => Promise<unknown> } }).__TAURI_INTERNALS__.invoke("api", { op: o, args: a });
        return null;
      } catch (e) {
        return e as { code?: string; message?: string };
      }
    },
    [op, args] as const,
  );
}

const OUT = process.env.HELM_E2E_OUT ?? scratchDir("helm-e2e-out-");
const profile = scratchDir("helm-e2e-profile-");
const repo = scratchDir("helm-e2e-repo-");
const vaultRoot = scratchDir("helm-e2e-vaults-");
const vault = path.join(vaultRoot, "Test Vault");
let app: App | undefined;
let chatId = "";
const page = () => app!.page;

function git(...args: string[]) {
  execFileSync("git", args, { cwd: repo, stdio: "pipe" });
}

describe("panel: Changes, the gate, vault notes, the viewer and quick-open", { timeout: 10 * 60_000 }, () => {
  before(async () => {
    git("init", "-q", "-b", "main");
    git("config", "user.email", "e2e@example.invalid");
    git("config", "user.name", "E2E");
    fs.writeFileSync(path.join(repo, "staged.txt"), "one\n");
    fs.writeFileSync(path.join(repo, "edited.txt"), Array.from({ length: 60 }, (_, i) => `line ${i + 1}`).join("\n") + "\n");
    git("add", ".");
    git("commit", "-q", "-m", "init");
    fs.writeFileSync(path.join(repo, "staged.txt"), "one\ntwo\n");
    git("add", "staged.txt");
    fs.writeFileSync(path.join(repo, "edited.txt"), Array.from({ length: 60 }, (_, i) => (i === 40 ? "CHANGED" : `line ${i + 1}`)).join("\n") + "\n");
    fs.writeFileSync(path.join(repo, "new.txt"), "fresh\n");

    fs.mkdirSync(path.join(vault, "Notes"), { recursive: true });
    fs.writeFileSync(path.join(vault, "Notes", "Plan.md"), "# The plan heading\n\nBody text.\n");
    const config = path.join(vaultRoot, "obsidian.json");
    fs.writeFileSync(config, JSON.stringify({ vaults: { v1: { path: vault, ts: 1 } } }));

    app = await launchApp(profile, { env: { HELM_TEST_OBSIDIAN_CONFIG: config } });
    chatId = await newChat(page(), repo);
  });
  after(async () => {
    await dispose(app);
  });

  test("P1: the Changes slot shows the sections and a stage click moves a file", async () => {
    try {
      await page().click(tid("panel-toggle"));
      await page().waitForSelector(tid("changes-view"), { timeout: 20_000 });
      await page().waitForSelector(tid("changes-section", '[data-section="staged"]'), { timeout: 20_000 });
      const count = (section: string) => page().locator(tid("changes-section", `[data-section="${section}"]`) + " " + tid("change-row")).count();
      assert.equal(await count("staged"), 1);
      assert.equal(await count("unstaged"), 2, "the edit and the untracked file");
      const row = tid("change-row", '[data-path="new.txt"][data-section="unstaged"]');
      await page().click(`${row} ${tid("stage-button")}`);
      await page().waitForSelector(tid("change-row", '[data-path="new.txt"][data-section="staged"]'), { timeout: 20_000 });
      assert.equal(await count("staged"), 2);
      // Unfold a diff: a long unchanged run is folded into a gap.
      await page().click(`${tid("change-row", '[data-path="edited.txt"]')} .ch-chevron`);
      await page().waitForFunction(
        () => {
          const row = document.querySelector('[data-testid="change-row"][data-path="edited.txt"]');
          return Boolean(row?.querySelector('[data-testid="diff-view"]')) || [...(row?.querySelectorAll(".ch-hint") ?? [])].some((h) => !(h.textContent ?? "").startsWith("Loading"));
        },
        undefined,
        { timeout: 20_000 },
      );
      const diffText = await page().$eval(tid("change-row", '[data-path="edited.txt"]'), (e) => (e.textContent ?? "").slice(0, 300));
      assert.ok(await page().locator(tid("diff-gap")).count() > 0, `a folded gap in the diff: ${diffText}`);
      const h = await page().$eval(`${tid("change-row", '[data-path="edited.txt"]')} .ch-file-row`, (e) => Math.round(e.getBoundingClientRect().height));
      assert.equal(h, 40, "file rows are 40 px");
    } finally {
      await shot(page(), OUT, "P1");
    }
  });

  test("P2: the host's path gate and private roots refuse before any read", async () => {
    for (const p of ["\\\\host\\share\\x.txt", "//host/share/x.txt", "\\\\?\\C:\\x", "C:x", "C:\\NUL"]) {
      assert.equal((await apiError("file_read", { path: p }))?.code, "refused", p);
    }
    assert.equal((await apiError("file_read", { path: path.join(profile, "helm.sqlite3") }))?.code, "refused", "the profile is private");
    const view = (await api(page(), "file_read", { path: path.join(repo, "edited.txt") })) as { kind: string; text: string };
    assert.equal(view.kind, "text");
    assert.ok(view.text.includes("CHANGED"));
  });

  test("P3: a vault from the Obsidian registry resolves and reads", async () => {
    const vaults = (await api(page(), "vault_list", {})) as Array<{ name: string; source: string }>;
    assert.deepEqual(vaults.map((v) => [v.name, v.source]), [["Test Vault", "obsidian"]]);
    const r = (await api(page(), "vault_resolve", { note: "Plan" })) as { vault: string; path: string };
    assert.deepEqual(r, { vault: "Test Vault", path: "Notes/Plan.md" });
    const doc = (await api(page(), "vault_read", { vault: "Test Vault", path: "Notes/Plan.md" })) as { text: string };
    assert.ok(doc.text.includes("The plan heading"));
    assert.equal((await apiError("vault_read", { vault: "Test Vault", path: "../obsidian.json" }))?.code, "refused");
  });

  test("P4: quick-open on Ctrl+P lists the project's files; no CSP violation", async () => {
    try {
      await page().locator(tid("chat-view")).click({ position: { x: 5, y: 5 } }).catch(() => undefined);
      await page().keyboard.press("Control+P");
      await page().waitForSelector(tid("quick-open"), { timeout: 10_000 });
      await page().keyboard.type("edited");
      await page().waitForSelector(tid("quick-open-row"), { timeout: 15_000 });
      const labels = await page().$$eval(tid("quick-open-row"), (els) => els.map((e) => e.textContent ?? ""));
      assert.ok(labels.some((l) => l.includes("edited.txt")), labels.join(" | "));
      await page().keyboard.press("Enter");
      await page().waitForSelector(tid("panel-tab", '[data-kind="file"]'), { timeout: 15_000 });
      await page().waitForSelector(tid("code-view"), { timeout: 15_000 });
      assert.deepEqual(await app!.cspViolations(), []);
      void chatId;
    } finally {
      await shot(page(), OUT, "P4");
    }
  });
});
