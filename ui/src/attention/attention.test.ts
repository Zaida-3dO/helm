import assert from 'node:assert/strict';
import { test } from 'node:test';
import type { AttentionRow, AttentionSummary } from '../api/types.ts';
import { countText, inboxCount, inboxRows, nextAttention, snoozeChoices, whenText } from './model.ts';

const row = (chatId: string, kind: AttentionRow['kind'] = 'question'): AttentionRow => ({ chatId, title: chatId, project: 'p', kind, reason: 'r' });
const summary = (over: Partial<AttentionSummary> = {}): AttentionSummary => ({
  badge: 0,
  busy: 0,
  needsYou: [],
  running: [],
  unread: [],
  stalled: [],
  revision: 1,
  ...over,
});

test('the inbox lists waiting-on-you, then stuck, then unread, each chat once; running chats are not listed', () => {
  const s = summary({
    badge: 4,
    needsYou: [row('a'), row('b', 'approval')],
    stalled: [row('c', 'stalled'), row('a', 'stalled')],
    unread: [row('d', 'unread'), row('b', 'unread')],
    running: [row('r', 'running')],
  });
  assert.deepEqual(
    inboxRows(s).map((r) => `${r.chatId}:${r.group}`),
    ['a:needsYou', 'b:needsYou', 'c:stalled', 'd:unread'],
  );
  assert.deepEqual(inboxRows(null), []);
});

test('the count is the badge, never fewer than the rows, with "+" when the host holds more', () => {
  assert.equal(inboxCount(summary({ badge: 2, needsYou: [row('a'), row('b'), row('c')] })), 3);
  assert.equal(countText(summary({ badge: 11, needsYou: [row('a')] })), '11+');
  assert.equal(countText(summary({ badge: 1, needsYou: [row('a')] })), '1');
  assert.equal(inboxCount(null), 0);
});

test('Ctrl+Alt+A walks the inbox from the open chat, wrapping; from elsewhere it starts at the top', () => {
  const rows = inboxRows(summary({ needsYou: [row('a'), row('b')], unread: [row('c', 'unread')] }));
  assert.equal(nextAttention(rows, null), 'a');
  assert.equal(nextAttention(rows, 'zzz'), 'a');
  assert.equal(nextAttention(rows, 'a'), 'b');
  assert.equal(nextAttention(rows, 'c'), 'a');
  assert.equal(nextAttention([], 'a'), null);
});

test('snooze choices: 20 min, 1 h, 3 h, tomorrow 9:15, and Monday 9:15 only when it is not tomorrow', () => {
  // Wednesday 21 Oct 2026, 14:00 local.
  const wed = new Date(2026, 9, 21, 14, 0, 0);
  const c = snoozeChoices(wed);
  assert.deepEqual(
    c.map((x) => x.id),
    ['20m', '1h', '3h', 'tomorrow', 'monday'],
  );
  assert.equal(c[0]!.until - wed.getTime(), 20 * 60_000);
  assert.equal(c[2]!.until - wed.getTime(), 3 * 3_600_000);
  assert.equal(new Date(c[3]!.until).getDate(), 22);
  assert.equal(new Date(c[3]!.until).getHours(), 9);
  assert.equal(new Date(c[3]!.until).getMinutes(), 15);
  assert.equal(new Date(c[4]!.until).getDay(), 1);
  assert.equal(new Date(c[4]!.until).getDate(), 26);
  assert.equal(c[0]!.when, '14:20');
  assert.equal(c[3]!.when, 'Thu Oct 22, 09:15');
  // On a Sunday, tomorrow is Monday: no separate Monday entry.
  const sun = new Date(2026, 9, 25, 10, 0, 0);
  assert.deepEqual(
    snoozeChoices(sun).map((x) => x.id),
    ['20m', '1h', '3h', 'tomorrow'],
  );
  // On a Monday, "Monday" is next week's.
  const mon = new Date(2026, 9, 26, 10, 0, 0);
  const m = snoozeChoices(mon).find((x) => x.id === 'monday')!;
  assert.equal(new Date(m.until).getDate(), 2);
  assert.equal(whenText(new Date(2026, 9, 26, 23, 5), mon), '23:05');
});
