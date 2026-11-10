import assert from 'node:assert/strict';
import { test } from 'node:test';
import type { AttentionRow, AttentionSummary } from '../api/types.ts';
import { countText, failedChecks, flagFor, inboxCount, inboxRows, mutedKindLabel, mutedRows, nextAttention, parseDelayMinutes, priorityFor, snoozeChoices, whenText } from './model.ts';

const row = (chatId: string, kind: AttentionRow['kind'] = 'question'): AttentionRow => ({ chatId, title: chatId, project: 'p', kind, reason: 'r' });
const summary = (over: Partial<AttentionSummary> = {}): AttentionSummary => ({
  badge: 0,
  busy: 0,
  needsYou: [],
  running: [],
  unread: [],
  stalled: [],
  flagged: [],
  muted: [],
  triage: [],
  revision: 1,
  ...over,
});

test('the inbox lists waiting-on-you, then quiet-but-flagged, then stuck, then unread, each chat once; running chats are not listed', () => {
  const s = summary({
    badge: 5,
    needsYou: [row('a'), row('b', 'approval')],
    flagged: [row('f', 'triage'), row('b', 'triage')],
    stalled: [row('c', 'stalled'), row('a', 'stalled')],
    unread: [row('d', 'unread'), row('b', 'unread')],
    running: [row('r', 'running')],
  });
  assert.deepEqual(
    inboxRows(s).map((r) => `${r.chatId}:${r.group}`),
    ['a:needsYou', 'b:needsYou', 'f:flagged', 'c:stalled', 'd:unread'],
  );
  assert.deepEqual(
    inboxRows(s, { flagged: false }).map((r) => r.chatId),
    ['a', 'b', 'c', 'd'],
    'with the idle check off, its rows are left out at once',
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

test('a chat shows the note only while the host lists it as flagged and the check is on', () => {
  const s = summary({ flagged: [{ ...row('f', 'triage'), reason: 'The tests were never run.' }], needsYou: [row('a')] });
  assert.equal(flagFor(s, 'f')?.reason, 'The tests were never run.');
  assert.equal(flagFor(s, 'a'), null, 'a chat waiting on you has its own card, not this note');
  assert.equal(flagFor(s, 'f', false), null, 'switching the check off hides it at once');
  assert.equal(flagFor(summary(), 'f'), null, 'once the flag clears, the note goes');
  assert.equal(flagFor(null, 'f'), null);
});

test('muted chats are kept in their own group, out of the rows and the count; the idle check adds a priority chip and Retry rows', () => {
  const s = summary({
    badge: 1,
    needsYou: [row('a')],
    flagged: [row('f', 'triage')],
    muted: [
      { chatId: 'm1', title: 'Crew', project: 'p', kind: 'flag', reason: 'Asked by the agent: Ship it?' },
      { chatId: 'm2', title: 'Reviewer', project: 'p', kind: 'approval', reason: 'Waiting for your approval' },
    ],
    triage: [
      { chatId: 'f', title: 'f', project: 'p', priority: 'high', failed: false },
      { chatId: 'n', title: 'n', project: 'p', priority: null, failed: false },
      { chatId: 'x', title: 'x', project: 'p', priority: null, failed: true },
    ],
  });
  assert.deepEqual(
    inboxRows(s).map((r) => r.chatId),
    ['a', 'f'],
    'a muted chat is not an inbox row',
  );
  assert.equal(inboxCount(s), 2);
  assert.deepEqual(
    mutedRows(s).map((r) => `${r.chatId}:${mutedKindLabel(r.kind)}`),
    ['m1:Flag', 'm2:Approval'],
  );
  assert.deepEqual(mutedRows(null), []);
  assert.equal(priorityFor(s, 'f'), 'high');
  assert.equal(priorityFor(s, 'n'), null, 'a verdict without a priority shows no chip');
  assert.equal(priorityFor(s, 'x'), null, 'a failed check has no priority');
  assert.deepEqual(failedChecks(s).map((r) => r.chatId), ['x']);
  assert.deepEqual(failedChecks(s, false), [], 'with the idle check off, Retry rows go at once');
  assert.deepEqual(failedChecks(null), []);
});

test('the alert delay is a whole number of minutes from 0 to 1,440', () => {
  const cases: [string, number | null][] = [
    ['15', 15],
    [' 0 ', 0],
    ['1440', 1440],
    ['1441', null],
    ['-1', null],
    ['1.5', null],
    ['', null],
    ['ten', null],
  ];
  for (const [text, want] of cases) assert.equal(parseDelayMinutes(text), want, text);
});
