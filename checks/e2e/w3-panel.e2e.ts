// The side panel end to end in the real debug app: the Changes slot on a real temporary git
// repository (sections, a stage click), the path gate and private roots on the host, a vault note
// through HELM_TEST_OBSIDIAN_CONFIG, the file viewer at a line, quick-open on Ctrl+P, and no CSP
// violation. Also a hostile HTML page run with "Run page": it must stay sealed (no IPC, no parent
// page, no storage, no network while offline, no other page's state) and still keep its own state.
//
// Run: `npm run build:debug` at the root, then `npm run e2e -w checks`. Needs git on PATH.

import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import dgram from "node:dgram";
import fs from "node:fs";
import net from "node:net";
import path from "node:path";
import { after, before, describe, test } from "node:test";
import type { FrameLocator } from "playwright-core";
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

  /** What the hostile page wrote into #out. */
  interface Report {
    globals: Record<string, string>;
    probes: Record<string, { ok: boolean; value?: string; error?: string }>;
    notes: Record<string, string>;
    violations: string[];
    frames: number;
    framesReached: number;
    state: { before: unknown; saved: unknown; saveError: string | null; after: unknown; frozen: boolean };
  }

  test("P5: a hostile page run with Run page stays sealed, keeps its own state and sees no other page's", async () => {
    // Anything that reaches these two loopback listeners got out of the page.
    const leaks: string[] = [];
    const tcp = net.createServer((sock) => {
      leaks.push("a connection on the loopback port");
      sock.on("error", () => undefined);
      sock.destroy();
    });
    const udp = dgram.createSocket("udp4");
    udp.on("message", (m) => leaks.push(`a ${m.length}-byte packet on the loopback UDP port`));
    await new Promise<void>((done) => tcp.listen(0, "127.0.0.1", () => done()));
    await new Promise<void>((done) => udp.bind(0, "127.0.0.1", () => done()));
    const tcpPort = (tcp.address() as net.AddressInfo).port;
    const udpPort = udp.address().port;
    const wait = (ms: number) => new Promise((done) => setTimeout(done, ms));
    try {
      const dir = path.join(repo, "pages");
      fs.mkdirSync(dir, { recursive: true });
      const fixture = (name: string) => fs.readFileSync(path.join(import.meta.dirname, "fixtures", name), "utf8");
      fs.writeFileSync(path.join(dir, "hostile-page.html"), fixture("hostile-page.html").replaceAll("@TCP@", String(tcpPort)).replaceAll("@UDP@", String(udpPort)));
      fs.writeFileSync(path.join(dir, "other-page.html"), fixture("other-page.html"));
      // Quick open reads the folder again when a query finds nothing in a listing older than a
      // quarter second; P4's listing may be younger than that.
      await wait(500);

      // Quick-open the file, then "Run page" on its tab.
      const runPage = async (stem: string): Promise<FrameLocator> => {
        await page().locator(tid("chat-view")).click({ position: { x: 5, y: 5 } }).catch(() => undefined);
        await page().keyboard.press("Control+P");
        await page().waitForSelector(tid("quick-open"), { timeout: 10_000 });
        await page().keyboard.type(stem);
        await page().waitForSelector(tid("quick-open-row"), { timeout: 15_000 });
        await page().keyboard.press("Enter");
        await page().locator(`${tid("run-page")}:visible`).click({ timeout: 20_000 });
        await page().waitForSelector(`iframe[title="${stem}"]`, { timeout: 20_000 });
        return page().frameLocator(`iframe[title="${stem}"]`);
      };
      const finished = async (frame: FrameLocator, hasText?: string): Promise<string> => {
        const out = frame.locator("#out[data-done]", hasText ? { hasText } : undefined);
        await out.waitFor({ timeout: 90_000 });
        return (await out.textContent()) ?? "";
      };

      // The other page saves a secret in its own state.
      const other = await runPage("other-page");
      assert.deepEqual(JSON.parse(await finished(other)), { secret: "OTHER-SECRET" });

      const hostile = await runPage("hostile-page");
      assert.equal(await page().locator(`${tid("page-frame-root")}:visible`).getAttribute("data-network"), "off", "a page opens offline");
      const text = await finished(hostile);
      const r = JSON.parse(text) as Report;
      const why = (k: string) => `${k}: ${JSON.stringify(r.probes[k])} notes=${JSON.stringify(r.notes)} violations=${JSON.stringify(r.violations)}`;

      // No way to Helm's IPC. If the bridge object is there at all, a call through it must fail.
      for (const k of ["ipcInvoke", "ipcFetch", "ipcImage"]) assert.equal(r.probes[k]?.ok, false, why(k));
      // No way to the app's page.
      for (const k of ["parentDocument", "topDocument", "parentIpc", "parentStorage"]) {
        assert.match(r.probes[k]?.error ?? "", /^SecurityError/, why(k));
      }
      // No storage of its own: an opaque origin has none.
      for (const k of ["localStorage", "sessionStorage", "cookieRead", "cookieWrite"]) {
        assert.match(r.probes[k]?.error ?? "", /^SecurityError/, why(k));
      }
      for (const k of ["indexedDB", "caches"]) assert.equal(r.probes[k]?.ok, false, why(k));
      // No network while offline: every attempt is refused by the page's policy.
      for (const k of ["fetchOutside", "fetchLoopback", "xhrLoopback", "webSocket", "eventSource", "imageLoopback", "scriptLoopback", "styleLoopback"]) {
        assert.equal(r.probes[k]?.ok, false, why(k));
      }
      assert.ok(r.violations.some((v) => v.startsWith("connect-src") && v.includes("example.com")), `the outside fetch is refused by policy: ${JSON.stringify(r.violations)}`);
      // The other page's frame, if the app keeps it mounted, is not reachable from here.
      assert.equal(r.framesReached, 0, `frames reached through the parent: ${r.framesReached} of ${r.frames}`);
      // The page's own state works, starts empty and is its own.
      assert.equal(r.state.before, null, "a page starts with no state, not another page's");
      assert.equal(r.state.saveError, null);
      assert.equal(typeof r.state.saved, "number", "setState answers a revision");
      assert.deepEqual(r.state.after, { probe: "ok", n: 1 });
      assert.equal(r.state.frozen, true, "the helm object cannot be changed by the page");
      assert.ok(!text.includes("OTHER-SECRET"), "nothing of the other page's state reached the hostile page");

      // Reload: the page starts again and finds the state it saved.
      await page().locator(`${tid("page-reload")}:visible`).click();
      const again = JSON.parse(await finished(hostile, '"before":{"probe":"ok","n":1}')) as Report;
      assert.deepEqual(again.state.before, { probe: "ok", n: 1 }, "the saved state survives a reload");
      assert.ok(!JSON.stringify(again).includes("OTHER-SECRET"));

      // The hostile page leaves through a link last: the app's frame policy must stop that too.
      // Nothing may ever have reached the listeners, from any of the attempts above.
      await wait(4_000);
      assert.deepEqual(leaks, [], `reached the outside; what the page noted: ${JSON.stringify(r.notes)}`);

      // Each page's saved state is its own, as the host keeps it.
      const stateOf = async (file: string) => {
        const s = (await api(page(), "page_open", { path: path.join(dir, file), network: false })) as { session: string; state: unknown };
        await api(page(), "page_close", { session: s.session });
        return s.state;
      };
      assert.deepEqual(await stateOf("other-page.html"), { secret: "OTHER-SECRET" });
      assert.deepEqual(await stateOf("hostile-page.html"), { probe: "ok", n: 1 });
    } finally {
      tcp.close();
      udp.close();
      await shot(page(), OUT, "P5");
    }
  });
});
