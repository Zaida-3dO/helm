import { test } from 'node:test';
import assert from 'node:assert/strict';
import { resolveKey, sendTooltip, stopTooltip, type KeyContext, type KeyShape } from './keymap.ts';
import { DRAFT_DEBOUNCE_MS, DraftSaver, nextRevision, type Timers } from './drafts.ts';
import { movePosition, queueView } from './queue.ts';
import { submission } from '../dev/fixtures.ts';
import { wrapMarker } from './markup.ts';
import { permissionLabel } from './permission.ts';

const key = (k: string, m: Partial<KeyShape> = {}): KeyShape => ({ key: k, ctrl: false, shift: false, alt: false, meta: false, ...m });
const asap: KeyContext = { sendShortcut: 'mod-enter', mode: 'asap', showChoices: true };

test('send keys follow the mode: asap has queue and send now as alternates', () => {
  assert.deepEqual(resolveKey(key('Enter', { ctrl: true }), asap), { kind: 'send', action: 'default' });
  assert.deepEqual(resolveKey(key('Enter', { alt: true }), asap), { kind: 'send', action: 'queue' });
  assert.deepEqual(resolveKey(key('Enter', { ctrl: true, alt: true }), asap), { kind: 'send', action: 'now' });
  // Steer is the default in asap, so its key is not an alternate there.
  assert.deepEqual(resolveKey(key('Enter', { ctrl: true, shift: true }), asap), { kind: 'send', action: 'default' });
  assert.deepEqual(resolveKey(key('Enter'), asap), { kind: 'none' }, 'plain Enter is a newline by default');
  assert.deepEqual(resolveKey(key('Escape'), asap), { kind: 'stop-continue' });
});

test('queue mode offers steer; interrupt offers steer and queue', () => {
  const q: KeyContext = { ...asap, mode: 'queue' };
  assert.deepEqual(resolveKey(key('Enter', { ctrl: true, shift: true }), q), { kind: 'send', action: 'steer' });
  assert.deepEqual(resolveKey(key('Enter', { alt: true }), q), { kind: 'send', action: 'default' });
  const i: KeyContext = { ...asap, mode: 'interrupt' };
  assert.deepEqual(resolveKey(key('Enter', { alt: true }), i), { kind: 'send', action: 'queue' });
  assert.deepEqual(resolveKey(key('Enter', { ctrl: true, alt: true }), i), { kind: 'send', action: 'default' });
});

test('with choices hidden, alternate keys send with the default mode', () => {
  const hidden = { ...asap, showChoices: false };
  assert.deepEqual(resolveKey(key('Enter', { alt: true }), hidden), { kind: 'send', action: 'default' });
});

test('the Enter shortcut sends on plain Enter, Shift+Enter is a newline', () => {
  const e: KeyContext = { ...asap, sendShortcut: 'enter' };
  assert.deepEqual(resolveKey(key('Enter'), e), { kind: 'send', action: 'default' });
  assert.deepEqual(resolveKey(key('Enter', { shift: true }), e), { kind: 'none' });
});

test('tooltips name what Send and Stop do', () => {
  assert.match(sendTooltip(asap, true), /^Steer into the running task · Ctrl\+Enter · Alt\+Enter to queue$/);
  assert.equal(sendTooltip(asap, false), 'Send · Ctrl+Enter');
  assert.equal(stopTooltip(false, true), 'Stop task and pause queue · Esc interrupts and continues the queue');
  assert.equal(stopTooltip(true, false), 'Stop task and send waiting messages');
});

function fakeTimers(start = 1000) {
  let now = start;
  let pending: { f: () => void; at: number } | null = null;
  const t: Timers & { advance(ms: number): void } = {
    now: () => now,
    set: (f, ms) => {
      pending = { f, at: now + ms };
      return pending;
    },
    clear: (h) => {
      if (pending === h) pending = null;
    },
    advance(ms) {
      now += ms;
      if (pending && pending.at <= now) {
        const p = pending;
        pending = null;
        p.f();
      }
    },
  };
  return t;
}

test('draft revision is max(now, prev + 1)', () => {
  assert.equal(nextRevision(5, 100), 100);
  assert.equal(nextRevision(500, 100), 501);
});

test('drafts save once, DRAFT_DEBOUNCE_MS after the last edit, with increasing revisions', async () => {
  const t = fakeTimers();
  const saves: Array<[string, number]> = [];
  const d = new DraftSaver('', 0, async (text, rev) => void saves.push([text, rev]), t);
  d.edit('a');
  t.advance(DRAFT_DEBOUNCE_MS - 1);
  d.edit('ab');
  t.advance(DRAFT_DEBOUNCE_MS - 1);
  assert.equal(saves.length, 0);
  t.advance(1);
  await Promise.resolve();
  assert.equal(saves.length, 1);
  assert.equal(saves[0]![0], 'ab');
  assert.ok(saves[0]![1] > 0);
  // Same clock reading: the next revision still goes up.
  d.edit('abc');
  await d.flush();
  assert.ok(saves[1]![1] > saves[0]![1]);
});

test('a send clears the draft unless a newer revision was written since it began', async () => {
  const t = fakeTimers();
  const saves: Array<[string, number]> = [];
  const d = new DraftSaver('', 0, async (text, rev) => void saves.push([text, rev]), t);
  d.edit('hello');
  const token = d.beginSend();
  assert.equal(d.sent(token), true);
  await Promise.resolve();
  assert.equal(saves.at(-1)?.[0], '');
  d.edit('one');
  const token2 = d.beginSend();
  d.edit('typed during the send');
  assert.equal(d.sent(token2), false);
  // A late change event with the same text as was sent is not newer text.
  const token3 = d.beginSend();
  d.edit('typed during the send');
  assert.equal(d.sent(token3, true), true);
});

test('queue: an accepted steer is Pending with Withdraw; queued follow-ups reorder', () => {
  const pending = [
    submission({ id: 'q2', position: 2, createdAt: 1 }),
    submission({ id: 's1', mode: 'steer', state: 'accepted', createdAt: 5 }),
    submission({ id: 'q1', position: 1, createdAt: 2 }),
    submission({ id: 'u1', state: 'uncertain', position: 3, createdAt: 3 }),
  ];
  const ctx = { running: true, showChoices: true, capabilities: { withdraw: true, deliverNow: true, stopTool: false, compact: false } };
  const v = queueView(pending, ctx);
  assert.deepEqual(
    v.map((i) => [i.submission.id, i.state, i.label]),
    [
      ['s1', 'pending', 'Pending'],
      ['q1', 'queued', 'Queued'],
      ['q2', 'queued', 'Queued'],
      ['u1', 'uncertain', 'Not sent'],
    ],
  );
  assert.deepEqual(v[0]!.actions, ['withdraw']);
  assert.deepEqual(v[1]!.actions, ['steerNow', 'sendNow', 'move_down']);
  assert.deepEqual(v[2]!.actions, ['steerNow', 'sendNow', 'move_up']);
  assert.deepEqual(v[3]!.actions, ['recover']);
  assert.equal(movePosition(pending, 'q2', -1), 1);
  assert.equal(movePosition(pending, 'q1', -1), null);
});

test('queue: Withdraw is hidden when the worker cannot withdraw; idle offers no reorder', () => {
  const pending = [submission({ id: 's1', mode: 'steer', state: 'accepted' }), submission({ id: 'q1' }), submission({ id: 'q2' })];
  const ctx = { running: false, showChoices: true, capabilities: { withdraw: false, deliverNow: true, stopTool: false, compact: false } };
  const v = queueView(pending, ctx);
  assert.deepEqual(v[0]!.actions, []);
  assert.deepEqual(v[1]!.actions, []);
});

test('formatting keys wrap, unwrap, and leave the caret between empty markers', () => {
  assert.deepEqual(wrapMarker('word', '**'), { text: '**word**', caretBack: 0 });
  assert.deepEqual(wrapMarker('**word**', '**'), { text: 'word', caretBack: 0 });
  assert.deepEqual(wrapMarker('', '~~'), { text: '~~~~', caretBack: 2 });
});

test('the permission chip shows bypass until a live mode is reported', () => {
  assert.equal(permissionLabel(null), 'Bypass permissions');
  assert.equal(permissionLabel('default'), 'Asks permission');
  assert.equal(permissionLabel('plan'), 'Plan mode');
});
