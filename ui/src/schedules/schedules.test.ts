import assert from 'node:assert/strict';
import { test } from 'node:test';
import { EventBus } from '../api/events.ts';
import type { Api } from '../api/client.ts';
import type { Schedule, ScheduleList } from './api.ts';
import {
  daysText,
  defaultDraft,
  draftToTiming,
  intervalText,
  relativeFuture,
  sameTiming,
  statusText,
  textProblem,
  timingText,
  timingToDraft,
  toLocalInput,
} from './model.ts';
import { ScheduleStore } from './store.ts';

const NOW = new Date(2026, 9, 19, 9, 0, 0).getTime(); // Monday 09:00 local

test('timings read in plain words', () => {
  assert.equal(intervalText(1800), 'Every 30 minutes');
  assert.equal(intervalText(60), 'Every minute');
  assert.equal(intervalText(3600), 'Every hour');
  assert.equal(intervalText(7200), 'Every 2 hours');
  assert.equal(intervalText(86_400), 'Every day');
  assert.equal(intervalText(90), 'Every 90 seconds');
  assert.equal(daysText([0, 1, 2, 3, 4, 5, 6]), 'every day');
  assert.equal(daysText([4, 0, 3, 1, 2]), 'weekdays');
  assert.equal(daysText([6, 5]), 'weekends');
  assert.equal(daysText([2, 0]), 'Mon, Wed');
  assert.equal(timingText({ kind: 'weekly', hour: 12, minute: 5, days: [0, 1, 2, 3, 4, 5, 6] }), 'At 12:05, every day');
  assert.match(timingText({ kind: 'once', at: NOW }), /^Once, /);
});

test('the editor defaults are 30 minutes, 09:00 on weekdays, and an hour from now', () => {
  const d = defaultDraft(NOW);
  assert.equal(d.everyMinutes, '30');
  assert.equal(d.at, '09:00');
  assert.deepEqual(d.days, [true, true, true, true, true, false, false]);
  assert.equal(d.onceAt, '2026-10-19T10:00');
});

test('fields become a timing, and a timing becomes the same fields', () => {
  const every = { ...defaultDraft(NOW), kind: 'interval' as const, everyMinutes: '1.5' };
  assert.deepEqual(draftToTiming(every, NOW), { ok: true, value: { kind: 'interval', seconds: 90 } });
  const weekly = { ...defaultDraft(NOW), kind: 'weekly' as const, at: '12:00', days: [true, false, false, false, false, false, true] };
  const w = draftToTiming(weekly, NOW);
  assert.deepEqual(w, { ok: true, value: { kind: 'weekly', hour: 12, minute: 0, days: [0, 6] } });
  if (w.ok) assert.deepEqual(timingToDraft(w.value, NOW), { ...weekly, onceAt: defaultDraft(NOW).onceAt, everyMinutes: '30' });
  const once = draftToTiming({ ...defaultDraft(NOW), kind: 'once' }, NOW);
  assert.equal(once.ok && once.value.kind === 'once' && once.value.at, NOW + 3_600_000);
  assert.equal(toLocalInput(NOW + 3_600_000), '2026-10-19T10:00');
});

test('bad fields name what to fix', () => {
  const d = defaultDraft(NOW);
  const err = (x: Parameters<typeof draftToTiming>[0]) => {
    const r = draftToTiming(x, NOW);
    return r.ok ? null : r.error;
  };
  assert.match(err({ ...d, kind: 'interval', everyMinutes: '0.5' }) ?? '', /1 minute/);
  assert.match(err({ ...d, kind: 'interval', everyMinutes: '' }) ?? '', /minutes/);
  assert.match(err({ ...d, kind: 'interval', everyMinutes: '600000' }) ?? '', /525,600/);
  assert.match(err({ ...d, kind: 'once', onceAt: '2026-10-19T08:59' }) ?? '', /future/);
  assert.match(err({ ...d, kind: 'once', onceAt: '' }) ?? '', /date/);
  assert.match(err({ ...d, kind: 'weekly', days: d.days.map(() => false) }) ?? '', /weekday/);
  assert.match(err({ ...d, kind: 'weekly', at: '25:00' }) ?? '', /time of day/);
  assert.equal(err({ ...d, kind: 'interval', everyMinutes: '1' }), null);
});

test('titles and messages are checked the way the host checks them', () => {
  assert.match(textProblem('  ', 'x') ?? '', /name/);
  assert.match(textProblem('a', ' ') ?? '', /message/);
  assert.match(textProblem('é'.repeat(81), 'x') ?? '', /too long/, '162 bytes');
  assert.equal(textProblem('é'.repeat(80), 'x'), null, '160 bytes');
  assert.match(textProblem('a', 'x'.repeat(8001)) ?? '', /8000/);
  assert.equal(sameTiming({ kind: 'interval', seconds: 60 }, { kind: 'interval', seconds: 60 }), true);
  assert.equal(sameTiming({ kind: 'interval', seconds: 60 }, { kind: 'interval', seconds: 61 }), false);
});

test('the status line says when it runs or why it does not', () => {
  assert.equal(statusText({ state: 'paused', nextRunAt: NOW + 1 }, NOW), 'Paused');
  assert.equal(statusText({ state: 'completed', nextRunAt: null }, NOW), 'Finished');
  assert.equal(statusText({ state: 'cancelled', nextRunAt: null }, NOW), 'Cancelled');
  assert.equal(statusText({ state: 'active', nextRunAt: NOW + 25 * 60_000 }, NOW), 'Next run in 25 min');
  assert.match(statusText({ state: 'active', nextRunAt: NOW - 1 }, NOW), /waiting for the chat/);
  assert.equal(relativeFuture(NOW + 125 * 60_000, NOW), 'in 2h 5m');
  assert.equal(relativeFuture(NOW + 52 * 3_600_000, NOW), 'in 2d 4h');
});

// The store.

function sched(over: Partial<Schedule> = {}): Schedule {
  return {
    id: 's1',
    definition: { title: 'Check-in', message: 'hi', timing: { kind: 'interval', seconds: 1800 }, chatId: 'c1' },
    state: 'active',
    nextRunAt: NOW,
    revision: 1,
    createdAt: NOW,
    chatTitle: 'Chat',
    ...over,
  };
}

function fakeApi(lists: ScheduleList[]): { api: Api; calls: [string, unknown][] } {
  const calls: [string, unknown][] = [];
  let n = 0;
  const api = (async (op: string, args?: unknown) => {
    calls.push([op, args]);
    if (op === 'schedule_list') return lists[Math.min(n++, lists.length - 1)];
    if (op === 'schedule_runs') return { runs: [] };
    return sched();
  }) as Api;
  return { api, calls };
}

const empty = (schedules: Schedule[]): ScheduleList => ({ schedules, templates: [], now: NOW });
const flush = () => new Promise((r) => setTimeout(r, 0));

test('the store reads once on start and again on the schedules event', async () => {
  const bus = new EventBus();
  const { api, calls } = fakeApi([empty([]), empty([sched()])]);
  const store = new ScheduleStore({ api, bus });
  let seen = 0;
  store.subscribe(() => seen++);
  store.start();
  store.start();
  await flush();
  assert.equal(calls.filter((c) => c[0] === 'schedule_list').length, 1, 'start is idempotent');
  assert.deepEqual(store.list?.schedules, []);
  bus.dispatch({ kind: 'schedules' });
  await flush();
  assert.equal(store.list?.schedules.length, 1);
  assert.deepEqual(calls[0], ['schedule_list', { includeInactive: true }]);
  assert.ok(seen >= 2);
  store.stop();
  bus.dispatch({ kind: 'schedules' });
  await flush();
  assert.equal(calls.filter((c) => c[0] === 'schedule_list').length, 2, 'stopped: no more reads');
});

test('a change goes to the host and then re-reads; a failure is kept as the error', async () => {
  const bus = new EventBus();
  const { api, calls } = fakeApi([empty([sched()])]);
  const store = new ScheduleStore({ api, bus });
  await store.update({ scheduleId: 's1', revision: 1, paused: true });
  await store.cancel({ id: 's1', revision: 2 });
  assert.deepEqual(
    calls.map((c) => c[0]),
    ['schedule_update', 'schedule_list', 'schedule_cancel', 'schedule_list'],
  );
  assert.deepEqual(calls[2], ['schedule_cancel', { scheduleId: 's1', revision: 2 }]);
  const failing = new ScheduleStore({
    api: (async () => {
      throw new Error('host is down');
    }) as Api,
    bus: new EventBus(),
  });
  await failing.refresh();
  assert.equal(failing.error, 'host is down');
  assert.equal(failing.list, null);
});

test('a slow older read cannot overwrite a newer one', async () => {
  let n = 0;
  const api = (async () => {
    const mine = n++;
    if (mine === 0) await new Promise((r) => setTimeout(r, 30));
    return empty(mine === 0 ? [] : [sched({ id: 'newer' })]);
  }) as Api;
  const store = new ScheduleStore({ api, bus: new EventBus() });
  await Promise.all([store.refresh(), store.refresh()]);
  assert.equal(store.list?.schedules[0]?.id, 'newer');
});
