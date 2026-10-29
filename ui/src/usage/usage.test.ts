import assert from 'node:assert/strict';
import { test } from 'node:test';
import { EventBus } from '../api/events.ts';
import type { UsageSnapshot, UsageWindow } from '../api/usage.ts';
import { asOf, bars, type BarModel, compactTokensText, emptyTitle, localText, percentText, rangeOf, relativeFuture, shareText, tokensText, usdText } from './model.ts';
import { POLL_MS, UsageStore } from './store.ts';

const NOW = Date.parse('2026-10-18T12:00:00Z');
const win = (pct: number | undefined, resetsAt: number | undefined, at = NOW - 1000, source: UsageWindow['source'] = 'control'): UsageWindow => ({ pct, resetsAt, at, source });

function snap(over: Partial<NonNullable<UsageSnapshot['reading']>> = {}): UsageSnapshot {
  return {
    reading: { fiveHour: win(5, NOW + 3_600_000), weekly: win(56, NOW + 90_000_000), perModel: [], source: 'control', fetchedAt: NOW - 1000, stale: false, ...over },
    readiness: { state: 'ready', reason: 'Plan usage is up to date.' },
    refreshing: false,
  };
}

test('two bars with the percentage, reset times and source in the tooltip', () => {
  const b = bars(snap(), NOW) as [BarModel, BarModel];
  assert.deepEqual(b.map((x) => [x.id, x.text, x.pct, x.stale]), [['five_hour', '5%', 5, false], ['weekly', '56%', 56, false]]);
  assert.match(b[0].tooltip, /5-hour window: 5% used/);
  assert.match(b[0].tooltip, /in 1h 0m/);
  assert.match(b[0].tooltip, /Claude Code usage report/);
});

test('an old reading, and a window whose reset has passed, are greyed', () => {
  assert.ok(bars(snap({ stale: true }), NOW).every((x) => x.stale));
  const b = bars(snap({ fiveHour: win(80, NOW - 1) }), NOW) as [BarModel, BarModel];
  assert.equal(b[0].stale, true);
  assert.match(b[0].tooltip, /Window has reset/);
  assert.equal(b[1].stale, false, 'the weekly window is still current');
});

test('a reset time with no number reads "?", and no reading gives no bars and a reason', () => {
  assert.equal(bars(snap({ weekly: win(undefined, NOW + 5000) }), NOW)[1]?.text, '—');
  assert.deepEqual(bars(null, NOW), []);
  const none: UsageSnapshot = { readiness: { state: 'inactive', reason: 'No plan usage has been read yet. Try Refresh' }, refreshing: false };
  assert.deepEqual(bars(none, NOW), []);
  assert.equal(emptyTitle(none), 'No plan usage has been read yet. Try Refresh');
  assert.equal(emptyTitle(null), 'Reading plan usage…');
  assert.equal(percentText(55.6), '56%');
  assert.equal(percentText(null), '—');
  assert.equal(percentText(undefined), '—');
});

test('relative times and ranges', () => {
  // The Cost dialog's numbers.
  const cases: [string, string][] = [
    [tokensText(1234567.4), '1,234,567'],
    [compactTokensText(950), '950'],
    [compactTokensText(12_345), '12.3k'],
    [compactTokensText(1_234_567), '1.2M'],
    [localText({ totalTokens: 1_200_000, wouldCostUsd: 4.1, comparedName: 'Sonnet' }), 'Local: 1.2M tokens, about $4.10 on Sonnet'],
    [usdText(0), '$0.00'],
    [usdText(0.004), '$0.004'],
    [usdText(1234.5), '$1,234.50'],
    [shareText(0), '0%'],
    [shareText(0.04), '<0.1%'],
    [shareText(4.56), '4.6%'],
    [shareText(37.6), '38%'],
  ];
  for (const [got, want] of cases) assert.equal(got, want);
  assert.equal(relativeFuture(NOW + 90 * 60_000, NOW), 'in 1h 30m');
  assert.equal(relativeFuture(NOW + 50 * 3_600_000, NOW), 'in 2d 2h');
  assert.equal(relativeFuture(NOW - 5, NOW), 'now');
  assert.equal(asOf(NOW - 30_000, NOW), 'just now');
  assert.equal(asOf(NOW - 5 * 60_000, NOW), '5 min ago');
  const today = rangeOf('today', NOW);
  assert.ok(today.since <= NOW && NOW - today.since < 86_400_000 + 3_600_000 && new Date(today.since).getHours() === 0);
  assert.deepEqual(rangeOf('all', NOW), { since: 0, until: NOW });
  assert.equal(rangeOf('hour', NOW).until - rangeOf('hour', NOW).since, 3_600_000);
});

test('the store reads at start, applies the event, refreshes on its timer only while visible', async () => {
  const calls: string[] = [];
  const bus = new EventBus();
  let visible = true;
  let tick: (() => void) | undefined;
  const api = (async (op: string) => {
    calls.push(op);
    return snap();
  }) as never;
  const store = new UsageStore({ api, bus, visible: () => visible, setInterval: (f, ms) => { assert.equal(ms, POLL_MS); tick = f; return 1; }, clearInterval: () => {} });
  let changes = 0;
  store.subscribe(() => changes++);
  store.start();
  store.start();
  await new Promise((r) => setTimeout(r, 0));
  assert.deepEqual(calls.sort(), ['usage_get', 'usage_refresh'], 'start twice reads once');
  assert.equal(store.snapshot?.reading?.weekly?.pct, 56);
  bus.dispatch({ kind: 'plan_usage', ...snap({ weekly: win(60, NOW + 1) }) });
  assert.equal(store.snapshot?.reading?.weekly?.pct, 60, 'the event carries the new snapshot');
  assert.equal((store.snapshot as unknown as { kind?: string }).kind, undefined, 'the event tag is not kept');
  calls.length = 0;
  visible = false;
  tick?.();
  await new Promise((r) => setTimeout(r, 0));
  assert.deepEqual(calls, [], 'hidden: no read');
  visible = true;
  tick?.();
  await new Promise((r) => setTimeout(r, 0));
  assert.deepEqual(calls, ['usage_refresh']);
  assert.ok(changes >= 3);
  store.stop();
});

test('a host failure is kept as the error, not thrown', async () => {
  const bus = new EventBus();
  const api = (async () => {
    throw new Error('host gone');
  }) as never;
  const store = new UsageStore({ api, bus, setInterval: () => 1, clearInterval: () => {} });
  store.start();
  await new Promise((r) => setTimeout(r, 0));
  assert.equal(store.error, 'host gone');
  assert.equal(store.snapshot, null);
});
