// Visual check of the real debug exe. DOM checks over WebView2's DevTools protocol only: measured
// boxes, computed styles, attributes and text, never screenshots read back. It also reads the
// probe file for what the DOM cannot see (tray, taskbar badge, keep-awake, hotkey).
//
// Inputs: the fake Agent Standup board (fakes/board, pointed at with HELM_TEST_STANDUP_URL /
// HELM_TEST_STANDUP_TOKEN), the w3-visual fake-claude fixture, and HELM_TEST_OBSIDIAN_CONFIG for a
// temporary vault. A part whose input is missing is reported as skipped with the reason, never
// passed.
//
// Run: `npm run build:debug` at the root, then `npm run e2e -w checks`.

import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";
import { after, before, describe, test } from "node:test";
import { api, contrast, dispose, FAKE_CLAUDE, launchApp, measure, newChat, REPO, scratchDir, send, shot, tid, waitStatus, type App } from "./harness.ts";

const OUT = process.env.HELM_E2E_OUT ?? scratchDir("helm-e2e-out-");
const profile = scratchDir("helm-w3v-profile-");
const probeFile = path.join(scratchDir("helm-w3v-probe-"), "probe.jsonl");
const W3_FIXTURE = path.join(REPO, "fakes", "claude", "fixtures", "w3-visual.json");

let app: App | undefined;
let board: { url: string; token: string; violations: unknown[]; requests: unknown[]; close(): Promise<void> } | undefined;
let vault: string | undefined;
const page = () => app!.page;

type Probe = { probe: string } & Record<string, unknown>;
function probes(kind?: string): Probe[] {
  if (!fs.existsSync(probeFile)) return [];
  return fs
    .readFileSync(probeFile, "utf8")
    .split("\n")
    .filter(Boolean)
    .map((l) => JSON.parse(l) as Probe)
    .filter((p) => !kind || p.probe === kind);
}

async function step(name: string, fn: () => Promise<void>): Promise<void> {
  try {
    await fn();
  } catch (e) {
    await shot(page(), OUT, `${name}-FAILED`);
    throw new Error(`${(e as Error).message}\n--- app log tail ---\n${app?.log().slice(-3000)}`);
  }
}

async function setTheme(mode: "light" | "dark"): Promise<void> {
  await api(page(), "preference", { key: "appearance", value: mode });
  await page().waitForFunction((m) => document.documentElement.dataset.theme === m, mode, { timeout: 5_000 });
  // Buttons ease their colours over 110 ms; measure after they settle.
  await page().waitForTimeout(400);
}

/** Text/background contrast of the first visible element matching `sel` (luminance in the page). */
async function readable(sel: string, min = 4.5): Promise<number> {
  // The background is the nearest opaque one up the tree; light translucent tints over it (hover
  // and selected fills of a few percent) are ignored, which errs on the side of the page colour.
  const m = await page().evaluate((s) => {
    const el = document.querySelector(s) as HTMLElement | null;
    if (!el) return null;
    const r = el.getBoundingClientRect();
    const probe = document.createElement("canvas").getContext("2d")!;
    const rgba = (c: string) => {
      probe.fillStyle = "#000";
      probe.fillStyle = c;
      probe.clearRect(0, 0, 1, 1);
      probe.fillRect(0, 0, 1, 1);
      const d = probe.getImageData(0, 0, 1, 1).data;
      return { c: `rgb(${d[0]}, ${d[1]}, ${d[2]})`, a: (d[3] ?? 0) / 255 };
    };
    let bg = "rgb(255, 255, 255)";
    for (let n: HTMLElement | null = el; n; n = n.parentElement) {
      const b = rgba(getComputedStyle(n).backgroundColor);
      if (b.a >= 0.99) {
        bg = b.c;
        break;
      }
    }
    return { w: r.width, h: r.height, color: rgba(getComputedStyle(el).color).c, bg };
  }, sel);
  assert.ok(m, `${sel} is missing`);
  assert.ok(m.w > 0 && m.h > 0, `${sel} has a zero-size box`);
  const c = contrast(m.color, m.bg);
  assert.ok(c >= min, `${sel} contrast ${c.toFixed(2)} < ${min} (${m.color} on ${m.bg})`);
  return c;
}

/** The background behind an element must be opaque (floating notices). */
async function opaque(sel: string): Promise<void> {
  const a = await page().evaluate((s) => {
    const el = document.querySelector(s);
    return el ? getComputedStyle(el).backgroundColor : null;
  }, sel);
  assert.ok(a && !/rgba\([^)]*,\s*0(\.\d+)?\)$/.test(a) && a !== "transparent", `${sel} background is not opaque: ${a}`);
}

describe("visual check and the host probes", { timeout: 15 * 60_000 }, () => {
  before(async () => {
    process.stderr.write(`w3-visual: profile ${profile}\nw3-visual: probe ${probeFile}\n`);
    const env: Record<string, string> = { HELM_TEST_PROBE: probeFile };
    if (fs.existsSync(W3_FIXTURE)) env.HELM_TEST_CLAUDE = JSON.stringify({ command: process.execPath, args: [FAKE_CLAUDE, W3_FIXTURE] });
    try {
      const mod = (await import("../../fakes/board/server.ts")) as { startFakeBoard: (o?: object) => Promise<NonNullable<typeof board>> };
      board = await mod.startFakeBoard();
      env.HELM_TEST_STANDUP_URL = board.url;
      env.HELM_TEST_STANDUP_TOKEN = board.token;
    } catch (e) {
      process.stderr.write(`w3-visual: no fake board (${(e as Error).message})\n`);
    }
    // A temporary vault with the fixture note, offered through the debug hook.
    vault = scratchDir("helm-w3v-vault-");
    fs.mkdirSync(path.join(vault, ".obsidian"), { recursive: true });
    fs.writeFileSync(path.join(vault, "Fixture Note.md"), "# Fixture heading\n\nThe vault note the visual check opens.\n");
    fs.mkdirSync(path.join(vault, "src"), { recursive: true });
    fs.writeFileSync(path.join(vault, "src", "main.ts"), "// line 1\n// line 2\nexport const three = 3;\n// line 4\n");
    const obsidian = path.join(scratchDir("helm-w3v-obsidian-"), "obsidian.json");
    fs.writeFileSync(obsidian, JSON.stringify({ vaults: { fixture: { path: vault, ts: Date.now(), open: true } } }));
    env.HELM_TEST_OBSIDIAN_CONFIG = obsidian;
    app = await launchApp(profile, { env });
    await page().setViewportSize({ width: 1280, height: 800 }).catch(() => undefined);
  });

  after(async () => {
    await dispose(app);
    await board?.close().catch(() => undefined);
  });

  test("frame, tokens and readability in light and dark", async () => {
    await step("frame", async () => {
      for (const mode of ["light", "dark"] as const) {
        await setTheme(mode);
        const side = await measure(page(), ".shell-sidebar");
        assert.ok(Math.abs(side.w - 480) <= 2, `sidebar is 480 px by default (${side.w})`);
        const top = await measure(page(), tid("topbar"));
        assert.ok(Math.abs(top.h - 49) <= 1, `top bar is 49 px (${top.h})`);
        const bg = await page().evaluate(() => getComputedStyle(document.documentElement).getPropertyValue("--bg").trim());
        assert.equal(bg, mode === "light" ? "#fbfbfa" : "#121212", `--bg in ${mode}`);
        await readable(`${tid("topbar")} .crumb-title`);
        await readable(tid("new-chat"), 3);
        assert.ok((await page().locator(tid("caption")).count()) === 1, "Helm's caption buttons are drawn");
      }
      await setTheme("light");
    });
  });

  test("V1-V3: one Agent Standup sync, board groups, local nesting under a board project", async (t) => {
    if (!board) return t.skip("the fake board (fakes/board) is not available");
    await step("V1", async () => {
      await page().waitForSelector(tid("board-sync", '[data-state="ok"]'), { timeout: 30_000 });
      const groups = await page().$$eval(tid("sidebar-group", '[data-kind="board"]'), (els) =>
        els.map((e) => ({ id: e.getAttribute("data-project-id"), depth: e.getAttribute("data-depth"), h: e.getBoundingClientRect().height, text: e.textContent ?? "" })),
      );
      assert.ok(groups.length >= 5, `the fixture's five board projects show (${groups.length})`);
      for (const title of ["Desktop app", "Website", "Home automations", "Finances", "Research notes"]) {
        assert.ok(groups.some((g) => g.text.includes(title)), `group "${title}"`);
      }
      // A 38 px header row in a 40 px slot.
      for (const g of groups) assert.ok(g.h >= 37 && g.h <= 41, `group header 38-40 px (${g.h})`);
      // V2: a local folder project nested under a board project draws at depth 1.
      const parent = groups[0]!.id!;
      const child = (await api(page(), "project_create", { name: "Nested here", path: scratchDir("helm-w3v-proj-"), parentId: parent })) as { id: string };
      await page().waitForSelector(tid("sidebar-group", `[data-project-id="${child.id}"][data-depth="1"]`), { timeout: 10_000 });
      // V3: a local rename of a board project survives a second sync.
      const p = (await api(page(), "project_list", {})) as { id: string; revision: number; kind: string }[];
      const target = p.find((x) => x.id === parent)!;
      await api(page(), "project_update", { projectId: parent, revision: target.revision, name: "Renamed in Helm" });
      await api(page(), "board_sync", {});
      await page().waitForSelector(tid("board-sync", '[data-state="ok"]'), { timeout: 30_000 });
      await page().waitForFunction((id) => (document.querySelector(`[data-testid="sidebar-group"][data-project-id="${id}"]`)?.textContent ?? "").includes("Renamed in Helm"), parent, { timeout: 10_000 });
      assert.deepEqual(board!.violations, [], "only GETs went to the board");
      await readable(`${tid("sidebar-group")} .sb-name`);
    });
  });

  test("V2: chat rows are 36 px and a nested chat draws at depth 1", async () => {
    await step("V2", async () => {
      const folder = scratchDir("helm-w3v-chat-");
      const parent = await newChat(page(), folder);
      const row = await measure(page(), tid("chat-item", `[data-chat-id="${parent}"]`));
      assert.ok(row.h >= 33 && row.h <= 37, `chat row: a 34 px button in a 36 px slot (${row.h})`);
      // Chats nest only within one project, so the child shares the folder.
      const child = await newChat(page(), folder);
      await api(page(), "chat_organize", { chatId: child, parentChatId: parent });
      await page().waitForSelector(tid("chat-item", `[data-chat-id="${child}"][data-depth="1"]`), { timeout: 10_000 });
    });
  });

  test("V4-V5: a vault wikilink opens its doc in the panel; the gate refuses a relative file link and C:javascript:", async (t) => {
    if (!fs.existsSync(W3_FIXTURE)) return t.skip("the w3-visual fake-claude fixture is missing");
    const vaults = await api(page(), "vault_list", {}).catch(() => null);
    if (vaults === null) return t.skip("the vault operations are not available");
    await step("V4", async () => {
      await newChat(page(), vault!);
      await send(page(), "[LINKS] show me");
      await page().waitForFunction(() => [...document.querySelectorAll('[data-testid="transcript-row"][data-role="assistant"]')].some((r) => (r.textContent ?? "").includes("That is all of them")), null, { timeout: 30_000 });
      await waitStatus(page(), "idle", 30_000);
      // V5: the renderer already refuses "C:javascript:" (no anchor at all); clicking its text
      // changes nothing.
      const before = page().url();
      const refused = await page().evaluate(() => [...document.querySelectorAll('[data-testid="transcript-row"] a')].some((a) => (a.textContent ?? "").includes("do not open")));
      assert.equal(refused, false, "the C:javascript: link is not a link");
      await page().click('[data-testid="transcript-row"] >> text=do not open');
      await page().waitForTimeout(300);
      assert.equal(page().url(), before, "no navigation");
      assert.equal(await page().locator(tid("panel-tab")).count(), 0, "no tab opened");
      await page().click('[data-testid="transcript-row"] a:text("Fixture Note")');
      await page().waitForSelector(tid("panel-tab", '[data-kind="vault-doc"]'), { timeout: 10_000 }).catch(async () => { const d = await page().evaluate(() => ({ tabs: [...document.querySelectorAll("[data-testid=panel-tab]")].map((t) => t.getAttribute("data-kind")), notice: document.querySelector("[data-testid=link-notice]")?.textContent ?? null, toast: document.querySelector("[data-testid=notice]")?.textContent ?? null, modal: !!document.querySelector("[data-testid=file-modal]"), panel: !!document.querySelector("[data-testid=side-panel]") })); throw new Error("panel state: " + JSON.stringify(d)); });
      await page().waitForFunction(() => (document.querySelector('[data-testid="vault-doc"]')?.textContent ?? "").includes("Fixture heading"), null, { timeout: 10_000 });
      await page().click('[data-testid="transcript-row"] a:text("main.ts line 3")');
      // A refused open shows its reason in the toast slot for a few seconds: report it if it comes.
      const refusal = await page()
        .waitForSelector(`${tid("panel-tab", '[data-kind="file"]')}, ${tid("notice")}`, { timeout: 10_000 })
        .then(() => page().textContent(tid("notice")).catch(() => null))
        .catch(() => null);
      // The gate opens a relative file link only from inside a document; in a
      // chat reply it is refused with one plain-sentence notice. The viewer at a line is covered by
      // the w3-panel check; here the refusal must be the gate's, shown once, with no tab opened.
      assert.match(refusal ?? "", /Relative file links open only from inside a document/, `the relative file link is refused by the gate (${refusal})`);
      assert.equal(await page().locator(tid("notice")).count(), 1, "one notice");
      assert.equal(await page().locator(tid("panel-tab", '[data-kind="file"]')).count(), 0, "no file tab");
      await readable(`${tid("panel-tab")}`, 3);
    });
  });

  test("V6: the terminal dock runs a real shell", async () => {
    await step("V6", async () => {
      await page().keyboard.press("Control+Backquote");
      await page().waitForSelector(tid("terminal-dock", '[data-open="true"]'), { timeout: 10_000 });
      const dock = await measure(page(), tid("terminal-dock"));
      assert.ok(Math.abs(dock.h - 300) <= 2, `dock opens at 300 px (${dock.h})`);
      await page().waitForSelector(tid("terminal-session"), { timeout: 10_000 });
      // Wait for the prompt, then type into the focused terminal.
      const tail = () => page().evaluate(() => [...document.querySelectorAll('[data-testid="terminal-session"]')].map((s) => `${(s as HTMLElement).hidden ? "hidden" : "shown"}: ${(s as HTMLElement).dataset.tail ?? ""}`).join(" | "));
      await page()
        .waitForFunction(() => (document.querySelector('[data-testid="terminal-session"]:not([hidden])') as HTMLElement | null)?.dataset.tail?.includes(">") ?? false, null, { timeout: 30_000 })
        .catch(async (e) => {
          throw new Error(`no prompt: ${await tail()} (${(e as Error).message.split("\n")[0]})`);
        });
      // Input goes through the same op the xterm view uses (keystrokes typed through CDP while
      // PSReadLine loads are unreliable, so the check drives the session's input directly).
      const sid = await page().getAttribute('[data-testid="terminal-session"]:not([hidden])', "data-session");
      await page().waitForTimeout(1_000);
      await api(page(), "terminal_write", { id: sid, data: "echo hi-w3-visual\r" });
      await page().waitForFunction(
        () => ((document.querySelector('[data-testid="terminal-session"]:not([hidden])') as HTMLElement | null)?.dataset.tail ?? "").split("\n").some((l) => l.trim() === "hi-w3-visual"),
        null,
        { timeout: 30_000 },
      ).catch(async (e) => {
        throw new Error(`no echo: ${await tail()} (${String((e as Error).message).slice(0, 80)})`);
      });
      await readable(tid("terminal-tab"), 3);
      await page().keyboard.press("Control+Backquote");
      await page().waitForSelector(tid("terminal-dock", '[data-open="false"]'), { state: "attached", timeout: 5_000 });
    });
  });

  test("V8 and C4: a background completion shows one toast, opening it withdraws it; badge and keep-awake follow", async () => {
    await step("V8", async () => {
      const bg = await newChat(page(), scratchDir("helm-w3v-chat-"));
      await send(page(), "finish quickly");
      // Move away before it finishes, so the completion lands in a background chat.
      await newChat(page(), scratchDir("helm-w3v-chat-"));
      await page().waitForSelector(tid("toast", `[data-chat-id="${bg}"]`), { timeout: 30_000 });
      assert.equal(await page().locator(tid("toast")).count(), 1, "one toast");
      const toast = await measure(page(), tid("toast"));
      assert.ok(toast.x + toast.w > toast.vw - 60 && toast.y + toast.h > toast.vh - 60, "bottom-right");
      await opaque(tid("toast"));
      await readable(`${tid("toast")} .notice-title`);
      // Nothing overlaps it.
      const covered = await page().evaluate(() => {
        const el = document.querySelector('[data-testid="toast"]')!;
        const r = el.getBoundingClientRect();
        const hit = document.elementFromPoint(r.x + r.width / 2, r.y + r.height / 2);
        return !el.contains(hit);
      });
      assert.equal(covered, false, "nothing overlaps the toast");
      const badges = probes("badge").map((p) => Number(p.count));
      assert.ok(badges.some((c) => c >= 1), `badge reached 1 (${badges})`);
      await page().click(tid("toast", `[data-chat-id="${bg}"]`) + ' [data-action="open"]');
      await page().waitForSelector(tid("toast"), { state: "detached", timeout: 10_000 });
      await page().waitForFunction(() => true);
      const end = Date.now() + 10_000;
      while (Date.now() < end && Number(probes("badge").at(-1)?.count) !== 0) await new Promise((r) => setTimeout(r, 200));
      assert.equal(Number(probes("badge").at(-1)?.count), 0, "badge back to 0 after reading");
      const awake = probes("keep_awake").map((p) => p.on);
      assert.ok(awake.includes(true) && awake.at(-1) === false, `keep-awake went on during the run and off after (${awake})`);
    });
  });

  test("C4: tray menu with microphones, and the hotkey registered and rebound (probe)", async () => {
    const tray = probes("tray_menu").at(-1) as { mics?: { label: string }[] } | undefined;
    assert.equal(tray?.mics?.[0]?.label, "Automatic", "the tray lists Automatic first");
    const first = probes("hotkey")[0];
    assert.equal(first?.chord, "Ctrl+Alt+H", "the default chord is Ctrl+Alt+H");
    // Another Helm on this PC (a parallel check run) may already hold Ctrl+Alt+H, so the
    // registration itself is proven on a chord nobody else uses: rebinding re-registers at once.
    const view = (await api(page(), "shell_settings_get", {})) as { settings: Record<string, unknown> };
    const chord = "Ctrl+Alt+Shift+F9";
    const saved = (await api(page(), "shell_settings_save", { ...view.settings, globalHotkey: chord })) as { hotkeyRegistered: boolean; hotkeyError: string | null };
    assert.equal(saved.hotkeyRegistered, true, `rebinding registered (${saved.hotkeyError})`);
    assert.ok(probes("hotkey").some((p) => p.chord === chord && p.registered === true), "probe: rebound chord registered");
    if (first?.registered !== true) process.stderr.write(`w3-visual: Ctrl+Alt+H was taken on this PC: ${JSON.stringify(first)}\n`);
  });

  test("narrow window (900x700): panel overlays, frame stays readable, no CSP violations", async () => {
    await step("narrow", async () => {
      await page().setViewportSize({ width: 900, height: 700 }).catch(() => undefined);
      await readable(tid("new-chat"), 3);
      const v = await app!.cspViolations();
      assert.deepEqual(v, [], `no CSP violations: ${v.join("; ")}`);
      await page().setViewportSize({ width: 1280, height: 800 }).catch(() => undefined);
    });
  });
});
