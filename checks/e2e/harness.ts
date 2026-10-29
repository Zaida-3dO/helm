// End-to-end harness: launches the debug `helm.exe` with a scratch profile and the fake claude, and
// drives its WebView2 page over the DevTools protocol with playwright-core (no browser download).
// Only the stable data-testid hooks in ui/TESTIDS.md are used.

import { spawn, spawnSync, type ChildProcess } from "node:child_process";
import fs from "node:fs";
import net from "node:net";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { chromium, type Browser, type Locator, type Page } from "playwright-core";

const here = path.dirname(fileURLToPath(import.meta.url));
export const REPO = path.resolve(here, "../..");
export const HELM_EXE = process.env.HELM_EXE ?? path.join(REPO, "target", "debug", process.platform === "win32" ? "helm.exe" : "helm");
export const FAKE_CLAUDE = path.join(REPO, "fakes", "claude", "fake-claude.ts");
export const E2E_FIXTURE = path.join(REPO, "fakes", "claude", "fixtures", "e2e.json");

export interface App {
  proc: ChildProcess;
  browser: Browser;
  page: Page;
  profileDir: string;
  /** Text the app wrote to stdout and stderr (for failure reports). */
  log: () => string;
  /** CSP violations reported by the page since connect. */
  cspViolations: () => Promise<string[]>;
}

export function scratchDir(prefix: string): string {
  return fs.mkdtempSync(path.join(os.tmpdir(), prefix));
}

async function freePort(): Promise<number> {
  return new Promise((resolve, reject) => {
    const s = net.createServer();
    s.once("error", reject);
    s.listen(0, "127.0.0.1", () => {
      const port = (s.address() as net.AddressInfo).port;
      s.close(() => resolve(port));
    });
  });
}

async function waitForCdp(port: number, proc: ChildProcess, timeoutMs: number): Promise<void> {
  const end = Date.now() + timeoutMs;
  for (;;) {
    if (proc.exitCode !== null) throw new Error(`helm.exe exited early (code ${proc.exitCode})`);
    try {
      const r = await fetch(`http://127.0.0.1:${port}/json/version`);
      if (r.ok) return;
    } catch {
      // not up yet
    }
    if (Date.now() > end) throw new Error(`WebView2 DevTools endpoint never came up${webviewDiagnostics(proc.pid)}`);
    await new Promise((r) => setTimeout(r, 200));
  }
}

/** For a failed start: which WebView2 processes our helm.exe has, and in which session (CI). */
function webviewDiagnostics(pid: number | undefined): string {
  if (process.platform !== "win32" || !pid) return "";
  const ps = `Get-CimInstance Win32_Process | Where-Object { $_.ParentProcessId -eq ${pid} -or $_.Name -like 'msedgewebview2*' } | ForEach-Object { "$($_.ProcessId) $($_.Name) session=$($_.SessionId) $($_.CommandLine)" }; "helm session=" + (Get-Process -Id ${pid}).SessionId; "runtime=" + (Get-ItemProperty 'HKLM:\\SOFTWARE\\WOW6432Node\\Microsoft\\EdgeUpdate\\Clients\\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}' -ErrorAction SilentlyContinue).pv`;
  const r = spawnSync("powershell", ["-NoProfile", "-Command", ps], { encoding: "utf8", timeout: 20_000 });
  return `; diagnostics: ${(r.stdout ?? "").slice(0, 3000)}${(r.stderr ?? "").slice(0, 500)}`;
}

/** Launches the debug app against `profileDir` (created if needed) and connects to its page. */
export interface LaunchOptions {
  /** false: the real claude executable (live checks); default true: the fake claude. */
  fake?: boolean;
  /** Written to claude-runtime.json on first launch. Default false, for the approval scenarios. */
  skipPermissions?: boolean;
  /** Extra environment for helm.exe; wins over the defaults above (for example HELM_TEST_LOCAL_SERVERS). */
  env?: Record<string, string>;
}

export async function launchApp(profileDir: string, opts: LaunchOptions = {}): Promise<App> {
  const fake = opts.fake !== false;
  if (!fs.existsSync(HELM_EXE)) throw new Error(`${HELM_EXE} not found: run \`npm run build:debug\` first`);
  fs.mkdirSync(profileDir, { recursive: true });
  // Approval scenarios need permission prompts: switch skip-permissions off.
  const runtime = path.join(profileDir, "claude-runtime.json");
  if (!fs.existsSync(runtime)) fs.writeFileSync(runtime, JSON.stringify({ skipPermissions: opts.skipPermissions === true }));
  const port = await freePort();
  const env: Record<string, string> = {};
  for (const [k, v] of Object.entries(process.env)) if (v !== undefined && k.toUpperCase() !== "CLAUDECODE") env[k] = v;
  Object.assign(env, {
    HELM_PROFILE_DIR: profileDir,
    WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS: `--remote-debugging-port=${port}`,
    HELM_TEST_CONFIRM: "ok",
    // No local model server is ever contacted from a check: each shows as unreachable. A check that
    // wants one points this at the fake in fakes/local-model through `opts.env`.
    HELM_TEST_LOCAL_SERVERS: "off",
    ...(fake ? { HELM_TEST_CLAUDE: JSON.stringify({ command: process.execPath, args: [FAKE_CLAUDE, E2E_FIXTURE] }), FAKE_CLAUDE_LOG: path.join(profileDir, "fake-claude.log") } : {}),
    ...(opts.env ?? {}),
  });
  if (!fake) delete env.HELM_TEST_CLAUDE;
  const proc = spawn(HELM_EXE, [], { env, stdio: ["ignore", "pipe", "pipe"], windowsHide: false });
  let out = "";
  proc.stdout?.on("data", (d: Buffer) => (out = (out + d.toString()).slice(-20_000)));
  proc.stderr?.on("data", (d: Buffer) => (out = (out + d.toString()).slice(-20_000)));
  try {
    // A cold first start (CI runner, fresh WebView2 profile) can take far longer than a warm one.
    await waitForCdp(port, proc, Number(process.env.HELM_E2E_START_MS ?? 120_000));
  } catch (e) {
    // Never leave our helm.exe behind: it would keep the test process (and a CI job) alive.
    proc.kill();
    throw new Error(`${(e as Error).message}; helm.exe output: ${out.slice(-4000) || "(none)"}`);
  }
  const browser = await chromium.connectOverCDP(`http://127.0.0.1:${port}`);
  let page: Page | undefined;
  const end = Date.now() + 15_000;
  while (!page) {
    page = browser.contexts().flatMap((c) => c.pages()).find((p) => !p.url().startsWith("devtools:"));
    if (!page) {
      if (Date.now() > end) throw new Error("no app page over CDP");
      await new Promise((r) => setTimeout(r, 200));
    }
  }
  await page.waitForSelector('[data-testid="new-chat"]', { timeout: 30_000 }).catch(async (e) => {
    const body = await page!.evaluate(() => document.body?.innerText?.slice(0, 2000) ?? "").catch(() => "?");
    proc.kill();
    throw new Error(`app never showed the chat list (${(e as Error).message.split("\n")[0]}); page text: ${body}; log: ${out.slice(-2000)}`);
  });
  await page.evaluate(() => {
    const w = window as unknown as { __cspv?: string[] };
    w.__cspv = [];
    window.addEventListener("securitypolicyviolation", (e) => w.__cspv!.push(`${e.violatedDirective} ${e.blockedURI}`));
  });
  return {
    proc,
    browser,
    page,
    profileDir,
    log: () => out,
    cspViolations: () => page!.evaluate(() => (window as unknown as { __cspv?: string[] }).__cspv ?? []),
  };
}

function exited(proc: ChildProcess, timeoutMs: number): Promise<boolean> {
  if (proc.exitCode !== null) return Promise.resolve(true);
  return new Promise((resolve) => {
    const t = setTimeout(() => resolve(false), timeoutMs);
    proc.once("exit", () => {
      clearTimeout(t);
      resolve(true);
    });
  });
}

/**
 * Graceful quit: the same `quit_request` op the UI's Quit uses (closing the window only hides it).
 * The debug-only HELM_TEST_CONFIRM=ok answers the native confirmation; the host then drains its
 * workers and exits.
 */
export async function quitGracefully(app: App, timeoutMs = 30_000): Promise<void> {
  await app.page
    .evaluate(() => {
      void (window as unknown as { __TAURI_INTERNALS__: { invoke: (c: string, p: unknown) => Promise<unknown> } }).__TAURI_INTERNALS__.invoke("api", { op: "quit_request", args: {} }).catch(() => undefined);
      return true;
    })
    .catch(() => undefined);
  if (!(await exited(app.proc, timeoutMs))) {
    app.proc.kill();
    throw new Error(`helm.exe did not quit within ${timeoutMs} ms of quit_request`);
  }
  await app.browser.close().catch(() => undefined);
}

/** The crash path: ends helm.exe at once (TerminateProcess on Windows). Our own child only. */
export async function killHard(app: App): Promise<void> {
  await app.browser.close().catch(() => undefined);
  app.proc.kill("SIGKILL");
  await exited(app.proc, 15_000);
}

export async function dispose(app: App | undefined): Promise<void> {
  if (!app) return;
  if (app.proc.exitCode === null) await killHard(app);
}

// ---- UI helpers (data-testid only) -----------------------------------------------------

export const tid = (id: string, extra = "") => `[data-testid="${id}"]${extra}`;

export async function newChat(page: Page, folder: string): Promise<string> {
  // Wait for the chat that opens, not for the row count to grow: the sidebar is virtualised, so
  // past a screenful the count stops growing (it scrolls the open chat into view instead).
  // The sidebar has no path box: the new-chat page creates a chat only on its first send, so checks
  // that need an empty chat make it through the host, then open its row.
  const before = await page.getAttribute(tid("chat-view"), "data-chat-id", { timeout: 1_000 }).catch(() => null);
  const made = (await api(page, "chat_create", { cwd: folder })) as { id: string };
  await page.waitForSelector(tid("chat-item", `[data-chat-id="${made.id}"]`), { timeout: 15_000 });
  await page.click(tid("chat-item", `[data-chat-id="${made.id}"]`));
  const h = await page.waitForFunction(
    (prev) => {
      const id = document.querySelector('[data-testid="chat-view"]')?.getAttribute("data-chat-id");
      return id && id !== prev ? id : false;
    },
    before,
    { timeout: 15_000 },
  );
  const id = String(await h.jsonValue());
  await page.waitForSelector(tid("chat-item", `[data-chat-id="${id}"]`), { timeout: 15_000 });
  return id;
}

/**
 * The sidebar hides projects with no chats, and a freshly synced board project holds none: put one
 * chat under each board project so its group draws. Returns the board projects' ids, in list order.
 */
export async function seedBoardChats(page: Page): Promise<string[]> {
  const folder = scratchDir("helm-e2e-board-");
  const nodes = (await api(page, "project_list", {})) as { id: string; board: unknown }[];
  const ids: string[] = [];
  for (const n of nodes.filter((x) => x.board)) {
    const made = (await api(page, "chat_create", { cwd: folder })) as { id: string };
    await api(page, "chat_organize", { chatId: made.id, groupProjectId: n.id });
    ids.push(n.id);
  }
  await page.waitForFunction((n) => document.querySelectorAll('[data-testid="sidebar-group"][data-kind="board"]').length >= n, ids.length, { timeout: 20_000 });
  return ids;
}

export async function openChat(page: Page, chatId: string): Promise<void> {
  await page.click(tid("chat-item", `[data-chat-id="${chatId}"]`));
  await page.waitForSelector(tid("chat-view", `[data-chat-id="${chatId}"]`), { timeout: 15_000 });
}

export async function typeMessage(page: Page, text: string): Promise<void> {
  const box = page.locator(tid("composer"));
  await box.click();
  await page.keyboard.type(text);
}

export async function send(page: Page, text: string): Promise<void> {
  await typeMessage(page, text);
  await page.click(tid("send"));
}

export function rows(page: Page, role?: string): Locator {
  return page.locator(role ? tid("transcript-row", `[data-role="${role}"]`) : tid("transcript-row"));
}

export async function waitText(page: Page, text: string, timeoutMs = 20_000): Promise<void> {
  await page.waitForFunction(
    (t) => [...document.querySelectorAll('[data-testid="transcript-row"]')].some((r) => (r.textContent ?? "").includes(t)),
    text,
    { timeout: timeoutMs },
  );
}

export async function countRowsWith(page: Page, text: string): Promise<number> {
  return page.evaluate((t) => [...document.querySelectorAll('[data-testid="transcript-row"]')].filter((r) => (r.textContent ?? "").includes(t)).length, text);
}

export async function status(page: Page): Promise<string> {
  return ((await page.locator(tid("chat-status")).textContent()) ?? "").trim();
}

export async function waitStatus(page: Page, want: string | string[], timeoutMs = 20_000): Promise<void> {
  const list = Array.isArray(want) ? want : [want];
  await page.waitForFunction((l) => l.includes((document.querySelector('[data-testid="chat-status"]')?.textContent ?? "").trim()), list, { timeout: timeoutMs });
}

/** Calls the host through the same `api` command the UI uses. */
export async function api(page: Page, op: string, args: Record<string, unknown>): Promise<unknown> {
  return page.evaluate(
    ([o, a]) => (window as unknown as { __TAURI_INTERNALS__: { invoke: (c: string, p: unknown) => Promise<unknown> } }).__TAURI_INTERNALS__.invoke("api", { op: o, args: a }),
    [op, args] as const,
  );
}

/** Opens every folded work block so the rows inside are in the DOM again. */
export async function unfoldAll(page: Page): Promise<void> {
  for (let i = 0; i < 20; i++) {
    const fold = page.locator('[data-testid="work-fold"]').first();
    if ((await fold.count()) === 0) return;
    await fold.click();
    await page.waitForTimeout(100);
  }
}

/** Waits for a row containing `text` and returns that row's text at that moment. */
export async function waitRowText(page: Page, text: string, timeoutMs = 20_000): Promise<string> {
  const h = await page.waitForFunction(
    (t) => [...document.querySelectorAll('[data-testid="transcript-row"]')].map((r) => r.textContent ?? "").find((x) => x.includes(t)) ?? false,
    text,
    { timeout: timeoutMs },
  );
  return String(await h.jsonValue());
}

export interface Measure {
  w: number;
  h: number;
  x: number;
  y: number;
  vw: number;
  vh: number;
  color: string;
  bg: string;
  display: string;
  visibility: string;
}

/** Layout and colours of the first element matching `selector` (U11: DOM checks, not PNGs). */
export async function measure(page: Page, selector: string): Promise<Measure> {
  return page.evaluate((sel) => {
    const el = document.querySelector(sel) as HTMLElement | null;
    if (!el) throw new Error(`no element for ${sel}`);
    const r = el.getBoundingClientRect();
    const cs = getComputedStyle(el);
    // Colours as rgba numbers through a canvas, so any CSS form parses (`color(srgb ...)`,
    // `color-mix()` results, alpha); a plain-regex parse read `color(srgb 0.96 ...)` as near black.
    const probe = document.createElement("canvas").getContext("2d", { willReadFrequently: true })!;
    const rgba = (c: string): [number, number, number, number] => {
      probe.clearRect(0, 0, 1, 1);
      probe.fillStyle = "#000";
      probe.fillStyle = c;
      probe.fillRect(0, 0, 1, 1);
      const d = probe.getImageData(0, 0, 1, 1).data;
      return [d[0]!, d[1]!, d[2]!, d[3]! / 255];
    };
    // The effective background: every translucent layer from the element up, over the first
    // opaque one (white if none is), composited the way they paint.
    const layers: [number, number, number, number][] = [];
    for (let n: HTMLElement | null = el; n; n = n.parentElement) {
      const b = rgba(getComputedStyle(n).backgroundColor);
      if (b[3] > 0) layers.push(b);
      if (b[3] >= 0.999) break;
    }
    let base: [number, number, number] = [255, 255, 255];
    for (const [lr, lg, lb, la] of layers.reverse()) base = [lr * la + base[0] * (1 - la), lg * la + base[1] * (1 - la), lb * la + base[2] * (1 - la)];
    const fg = rgba(cs.color);
    const color = `rgb(${[fg[0] * fg[3] + base[0] * (1 - fg[3]), fg[1] * fg[3] + base[1] * (1 - fg[3]), fg[2] * fg[3] + base[2] * (1 - fg[3])].map(Math.round).join(", ")})`;
    const bg = `rgb(${base.map(Math.round).join(", ")})`;
    return { w: r.width, h: r.height, x: r.x, y: r.y, vw: innerWidth, vh: innerHeight, color, bg, display: cs.display, visibility: cs.visibility };
  }, selector);
}

/** Relative luminance contrast ratio of two `rgb(...)` colours. */
export function contrast(a: string, b: string): number {
  const lum = (c: string) => {
    const [r, g, bl] = (c.match(/[\d.]+/g) ?? ["0", "0", "0"]).slice(0, 3).map((x) => {
      const v = Number(x) / 255;
      return v <= 0.03928 ? v / 12.92 : ((v + 0.055) / 1.055) ** 2.4;
    });
    return 0.2126 * r + 0.7152 * g + 0.0722 * bl;
  };
  const [hi, lo] = [lum(a), lum(b)].sort((x, y) => y - x);
  return (hi + 0.05) / (lo + 0.05);
}

/** Asserts an element is laid out on screen and readable. */
export async function assertShown(page: Page, selector: string, minContrast = 3): Promise<Measure> {
  const m = await measure(page, selector);
  const where = `${selector} ${JSON.stringify(m)}`;
  if (!(m.w > 10 && m.h > 10)) throw new Error(`not laid out: ${where}`);
  if (m.x < 0 || m.y < 0 || m.x + m.w > m.vw + 1 || m.y + m.h > m.vh + 1) throw new Error(`off screen: ${where}`);
  if (m.display === "none" || m.visibility === "hidden") throw new Error(`hidden: ${where}`);
  if (contrast(m.color, m.bg) < minContrast) throw new Error(`unreadable (contrast ${contrast(m.color, m.bg).toFixed(2)}): ${where}`);
  return m;
}

export async function shot(page: Page, dir: string, name: string): Promise<void> {
  fs.mkdirSync(dir, { recursive: true });
  await page.screenshot({ path: path.join(dir, `${name}.png`) }).catch(() => undefined);
}
