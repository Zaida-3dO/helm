// The renderer performance bench (its budgets are advisory checks, so it is not part of
// `npm test`). It builds the real UI for production against the in-memory fake host
// (checks/perf/app), serves it, drives it in Edge with playwright-core, and measures the renderer
// speed work:
//
//   stream       main-thread ms per second early (~1 KB) and late (~7 KB) in a long streamed reply
//   typing       keydown handling while idle and while a reply streams (Event Timing)
//   scroll       layout shift scrolling up through a long chat (83 turns)
//   turn         layout shift across one tool turn, from send to settle
//   markdown     per-update cost of streaming Markdown at 2 KB and 32 KB (node, no browser)
//   applyRows    one streaming row update in a 600-row thread (node)
//
// Enforced budgets (exit code 1 when one fails; `--baseline` records without enforcing):
// - scroll layout shift < 0.1; tool-turn layout shift < 0.05;
// - keydown processing p95 < 12 ms, idle and while streaming (the editor transaction);
// - streaming cost flat with length: late ms/s at most 1.5x early + 10;
// - Markdown per-update cost flat with length: p95 at 32 KB at most 3x the p95 at 2 KB + 0.5 ms.
// Everything else is reported only, because it depends on the display: frame-bound numbers
// (input to next frame, frame interval) cannot pass on a 32 Hz Remote Desktop display.
//
// Usage: `npm run perf -w checks` (Edge must be installed; HELM_PERF_CHANNEL picks another
// playwright channel). `HELM_PERF_OUT=<dir>` writes perf-<time>.json there and never overwrites.

import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { chromium, type Browser, type BrowserContext, type Page } from 'playwright-core';
import { build, preview } from 'vite';

const here = path.dirname(fileURLToPath(import.meta.url));
const configFile = path.join(here, 'app', 'vite.config.ts');
const baseline = process.argv.includes('--baseline');
const VIEWPORT = { width: 1920, height: 1080 };

type Result = Record<string, unknown>;
const results: Record<string, Result> = {};
const failures: string[] = [];

function budget(name: string, ok: boolean, detail: string): void {
  if (!ok) failures.push(`${name}: ${detail}`);
  process.stdout.write(`${ok ? 'ok  ' : 'FAIL'} ${name}: ${detail}\n`);
}

const round = (n: number, d = 1) => Math.round(n * 10 ** d) / 10 ** d;
function p95(xs: number[]): number | null {
  if (!xs.length) return null;
  const s = [...xs].sort((a, b) => a - b);
  return s[Math.ceil(0.95 * s.length) - 1]!;
}

// ---- browser scenarios -------------------------------------------------------------------------

async function open(browser: Browser, url: string, seed: number): Promise<{ ctx: BrowserContext; page: Page }> {
  const ctx = await browser.newContext({ viewport: VIEWPORT, deviceScaleFactor: 1, colorScheme: 'dark' });
  const page = await ctx.newPage();
  page.on('pageerror', (e) => process.stderr.write(`page error: ${e.message}\n`));
  await page.goto(`${url}?seed=${seed}`);
  await page.locator('text=Seeded chat').first().click({ timeout: 15_000 });
  await page.waitForSelector('[data-testid="chat-view"]');
  // Let the seeded turn and the motion check (perf/cadence.ts, up to ~6 s) settle.
  await waitIdle(page, 60_000);
  await page.waitForTimeout(6500);
  return { ctx, page };
}

async function waitIdle(page: Page, timeoutMs = 60_000): Promise<void> {
  await page.waitForFunction(() => document.querySelector('[data-testid="chat-status"]')?.textContent?.trim() === 'idle', null, { timeout: timeoutMs });
}

async function send(page: Page, text: string): Promise<void> {
  await page.locator('[data-testid="composer"]').click();
  await page.keyboard.type(text);
  await page.keyboard.press('Control+Enter');
}

async function metrics(page: Page): Promise<() => Promise<Record<string, number>>> {
  const cdp = await page.context().newCDPSession(page);
  await cdp.send('Performance.enable');
  return async () => {
    const r = (await cdp.send('Performance.getMetrics')) as { metrics: { name: string; value: number }[] };
    return Object.fromEntries(r.metrics.map((m) => [m.name, m.value]));
  };
}

/** Installs a layout-shift sum, an animation-frame counter and a keydown recorder. */
async function instrument(page: Page): Promise<void> {
  await page.evaluate(() => {
    const w = window as unknown as Record<string, unknown> & { __bench: { ls: number; frames: number; events: { name: string; proc: number; dur: number }[] } };
    w.__bench = { ls: 0, frames: 0, events: [] };
    new PerformanceObserver((l) => {
      for (const e of l.getEntries() as unknown as { value: number; hadRecentInput: boolean }[]) if (!e.hadRecentInput) w.__bench.ls += e.value;
    }).observe({ type: 'layout-shift', buffered: false });
    new PerformanceObserver((l) => {
      for (const e of l.getEntries() as unknown as { name: string; duration: number; processingStart: number; processingEnd: number }[]) {
        if (['keydown', 'keypress', 'beforeinput', 'input', 'keyup'].includes(e.name)) w.__bench.events.push({ name: e.name, proc: e.processingEnd - e.processingStart, dur: e.duration });
      }
    }).observe({ type: 'event', durationThreshold: 16, buffered: false } as PerformanceObserverInit);
    const loop = () => {
      w.__bench.frames++;
      requestAnimationFrame(loop);
    };
    requestAnimationFrame(loop);
  });
}

async function resetBench(page: Page): Promise<void> {
  await page.evaluate(() => {
    const b = (window as unknown as { __bench: { ls: number; frames: number; events: unknown[] } }).__bench;
    b.ls = 0;
    b.frames = 0;
    b.events = [];
  });
}

async function readBench(page: Page): Promise<{ ls: number; frames: number; events: { name: string; proc: number; dur: number }[] }> {
  return page.evaluate(() => (window as unknown as { __bench: { ls: number; frames: number; events: { name: string; proc: number; dur: number }[] } }).__bench);
}

async function lastAssistantChars(page: Page): Promise<number> {
  return page.evaluate(() => {
    const rows = [...document.querySelectorAll('[data-testid="transcript-row"][data-role="assistant"]')];
    return (rows.at(-1)?.textContent ?? '').length;
  });
}

async function typingProbe(page: Page, chars: number): Promise<Result> {
  await resetBench(page);
  await page.locator('[data-testid="composer"]').click();
  await page.keyboard.type('typing while the bench runs, '.repeat(4).slice(0, chars), { delay: 40 });
  await page.waitForTimeout(300);
  const b = await readBench(page);
  await page.keyboard.press('Control+A');
  await page.keyboard.press('Backspace');
  const proc = b.events.map((e) => e.proc);
  const dur = b.events.filter((e) => e.name === 'keydown').map((e) => e.dur);
  return {
    eventsOver16ms: b.events.length,
    processingP95Ms: proc.length ? round(p95(proc)!) : null,
    processingMaxMs: proc.length ? round(Math.max(...proc)) : null,
    keydownToPaintP95Ms: dur.length ? round(p95(dur)!) : null,
  };
}

async function streamScenario(browser: Browser, url: string): Promise<void> {
  const { ctx, page } = await open(browser, url, 20);
  const motionLow = await page.evaluate(() => (window as unknown as { __helmPerf?: { motion: { low: boolean } } }).__helmPerf?.motion.low ?? null);
  await instrument(page);
  results.typingIdle = await typingProbe(page, 40);
  const read = await metrics(page);
  await send(page, '[stream] bench');
  const windowAt = async (minChars: number): Promise<Result> => {
    const until = Date.now() + 60_000;
    while ((await lastAssistantChars(page)) < minChars) {
      if (Date.now() > until) throw new Error(`the streamed reply never reached ${minChars} chars`);
      await page.waitForTimeout(50);
    }
    const c0 = await lastAssistantChars(page);
    await resetBench(page);
    const a = await read();
    const t0 = Date.now();
    await page.waitForTimeout(2000);
    const b = await read();
    const secs = (Date.now() - t0) / 1000;
    const bench = await readBench(page);
    const d = (k: string) => (b[k] ?? 0) - (a[k] ?? 0);
    return {
      chars: [c0, await lastAssistantChars(page)],
      mainThreadMsPerSec: Math.round((d('TaskDuration') * 1000) / secs),
      scriptMsPerSec: Math.round((d('ScriptDuration') * 1000) / secs),
      layoutMsPerSec: Math.round((d('LayoutDuration') * 1000) / secs),
      styleMsPerSec: Math.round((d('RecalcStyleDuration') * 1000) / secs),
      layoutsPerFrame: bench.frames ? round(d('LayoutCount') / bench.frames, 2) : null,
      framesPerSec: Math.round(bench.frames / secs),
    };
  };
  const early = await windowAt(800);
  const typingStreaming = await typingProbe(page, 30);
  const late = await windowAt(6500);
  results.stream = { motionLow, early, late };
  results.typingStreaming = typingStreaming;
  await waitIdle(page, 120_000);
  await ctx.close();

  const e = early.mainThreadMsPerSec as number;
  const l = late.mainThreadMsPerSec as number;
  budget('stream flat with length', l <= 1.5 * e + 10, `early ${e} ms/s, late ${l} ms/s (motion ${motionLow ? 'reduced' : 'full'}; reported, not enforced: the absolute numbers)`);
  for (const [name, r] of [
    ['typing idle', results.typingIdle],
    ['typing while streaming', typingStreaming],
  ] as const) {
    const p = (r as Result).processingP95Ms as number | null;
    budget(`${name}: keydown processing p95`, p === null || p < 12, p === null ? 'every event under 16 ms (none recorded)' : `${p} ms (input to paint p95 ${(r as Result).keydownToPaintP95Ms} ms, reported only)`);
  }
}

async function scrollScenario(browser: Browser, url: string): Promise<void> {
  const { ctx, page } = await open(browser, url, 250);
  await instrument(page);
  const box = await page.locator('[data-testid="transcript"]').boundingBox();
  if (!box) throw new Error('no transcript');
  await page.mouse.move(box.x + box.width / 2, box.y + box.height / 2);
  const top0 = await page.evaluate(() => document.querySelector('[data-testid="transcript"]')!.scrollTop);
  await resetBench(page);
  for (let i = 0; i < 60; i++) {
    await page.mouse.wheel(0, -400);
    await page.waitForTimeout(16);
  }
  await page.waitForTimeout(400);
  const b = await readBench(page);
  const top1 = await page.evaluate(() => document.querySelector('[data-testid="transcript"]')!.scrollTop);
  const mounted = await page.locator('[data-key]').count();
  results.scroll = { layoutShift: round(b.ls, 3), scrolledPx: Math.round(top0 - top1), mountedRows: mounted };
  await ctx.close();
  budget('scroll up: layout shift', b.ls < 0.1, `${round(b.ls, 3)} over ${Math.round(top0 - top1)} px (${mounted} rows mounted)`);
  budget('scroll up: real travel and bounded rows', top0 - top1 > 2000 && mounted < 80, `${Math.round(top0 - top1)} px travelled, ${mounted} rows mounted`);
}

async function turnScenario(browser: Browser, url: string): Promise<void> {
  const { ctx, page } = await open(browser, url, 60);
  await instrument(page);
  await resetBench(page);
  await send(page, 'please look at the folder');
  await page.waitForTimeout(300);
  await waitIdle(page, 60_000);
  await page.waitForTimeout(600);
  const b = await readBench(page);
  const following = (await page.locator('[data-testid="jump-latest"]').count()) === 0;
  results.turn = { layoutShift: round(b.ls, 3), stillFollowing: following };
  await ctx.close();
  budget('tool turn: layout shift, send to settle', b.ls < 0.05, `${round(b.ls, 3)}`);
  budget('tool turn: the view still follows the end', following, following ? 'following' : 'the Latest pill is showing');
}

// ---- node scenarios ----------------------------------------------------------------------------

async function markdownScenario(): Promise<void> {
  const { StreamBlocks } = await import('../../ui/src/markdown/blocks.ts');
  const chunk = (i: number) => (i % 12 === 11 ? '\n\n' : i % 40 === 20 ? '\n\n```js\nconst x = 1;\n```\n\n' : `word${i} **bold** and \`c${i}\` `);
  const s = new StreamBlocks();
  let text = '';
  const at: Record<number, number[]> = { 2000: [], 32000: [] };
  for (let i = 0; text.length < 34_000; i++) {
    text += chunk(i);
    const t0 = performance.now();
    s.update(text, { streaming: true, interactive: true });
    const ms = performance.now() - t0;
    for (const k of [2000, 32000]) if (text.length >= k && text.length < k + 2000) at[k]!.push(ms);
  }
  const small = p95(at[2000]!)!;
  const large = p95(at[32000]!)!;
  results.markdown = { updateP95MsAt2KB: round(small, 3), updateP95MsAt32KB: round(large, 3) };
  budget('Markdown per update flat with length', large <= 3 * small + 0.5, `p95 ${round(small, 3)} ms at 2 KB, ${round(large, 3)} ms at 32 KB`);
}

async function applyRowsScenario(): Promise<void> {
  const { applyRows, emptyView } = await import('../../ui/src/state/thread.ts');
  const { row } = await import('../../ui/src/dev/fixtures.ts');
  const rows = Array.from({ length: 600 }, (_, i) => row({ id: i + 1, chatId: 'c', role: i % 2 ? 'assistant' : 'user', text: 'x'.repeat(200), timestamp: i, sequence: i, revision: 1 }));
  let v = applyRows(emptyView('c'), rows);
  const t0 = performance.now();
  for (let k = 0; k < 2000; k++) v = applyRows(v, [{ ...rows[599]!, text: 'y'.repeat(k), revision: k + 2 }]);
  const per = (performance.now() - t0) / 2000;
  results.applyRows = { msPerStreamingUpdate600Rows: round(per, 3) };
  process.stdout.write(`info applyRows: ${round(per, 3)} ms per streaming update in a 600-row thread (no fast path needed)\n`);
}

// ---- main --------------------------------------------------------------------------------------

async function main(): Promise<void> {
  process.stdout.write('building the bench page...\n');
  await build({ configFile, logLevel: 'error' });
  const server = await preview({ configFile, logLevel: 'error', preview: { host: '127.0.0.1', port: 0, strictPort: false } });
  const url = server.resolvedUrls?.local[0];
  if (!url) throw new Error('the preview server has no address');
  const browser = await chromium.launch({ channel: process.env.HELM_PERF_CHANNEL ?? 'msedge' });
  try {
    await markdownScenario();
    await applyRowsScenario();
    await streamScenario(browser, url);
    await scrollScenario(browser, url);
    await turnScenario(browser, url);
  } finally {
    await browser.close();
    await new Promise<void>((r) => server.httpServer.close(() => r()));
  }
  const out = process.env.HELM_PERF_OUT;
  if (out) {
    fs.mkdirSync(out, { recursive: true });
    const file = path.join(out, `perf-${new Date().toISOString().replace(/[:.]/g, '-')}.json`);
    fs.writeFileSync(file, JSON.stringify({ baseline, failures, results }, null, 2), { flag: 'wx' });
    process.stdout.write(`results: ${file}\n`);
  } else {
    process.stdout.write(`${JSON.stringify(results, null, 2)}\n`);
  }
  if (failures.length && !baseline) {
    process.stdout.write(`\n${failures.length} budget(s) failed\n`);
    process.exitCode = 1;
  }
}

await main();
