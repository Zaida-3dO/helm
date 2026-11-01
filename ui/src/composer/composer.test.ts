import { test } from 'node:test';
import assert from 'node:assert/strict';
import { resolveKey, sendTooltip, stopTooltip, type KeyContext, type KeyShape } from './keymap.ts';
import { DRAFT_DEBOUNCE_MS, DraftSaver, nextRevision, type Timers } from './drafts.ts';
import { inFlightView } from './inFlight.ts';
import { submission } from '../dev/fixtures.ts';
import { liveRuns, wrapMarker } from './markup.ts';
import { permissionLabel } from './permission.ts';

const key = (k: string, m: Partial<KeyShape> = {}): KeyShape => ({ key: k, ctrl: false, shift: false, alt: false, meta: false, ...m });
const asap: KeyContext = { sendShortcut: 'mod-enter', mode: 'asap', showChoices: true };

test('send keys follow the mode: asap has send now as its alternate', () => {
  assert.deepEqual(resolveKey(key('Enter', { ctrl: true }), asap), { kind: 'send', action: 'default' });
  assert.deepEqual(resolveKey(key('Enter', { alt: true }), asap), { kind: 'none' }, 'Alt+Enter no longer queues');
  assert.deepEqual(resolveKey(key('Enter', { ctrl: true, alt: true }), asap), { kind: 'send', action: 'now' });
  // Steer is the default in asap, so its key is not an alternate there.
  assert.deepEqual(resolveKey(key('Enter', { ctrl: true, shift: true }), asap), { kind: 'send', action: 'default' });
  assert.deepEqual(resolveKey(key('Enter'), asap), { kind: 'none' }, 'plain Enter is a newline by default');
  assert.deepEqual(resolveKey(key('Escape'), asap), { kind: 'stop' });
});

test('interrupt mode offers steer', () => {
  const i: KeyContext = { ...asap, mode: 'interrupt' };
  assert.deepEqual(resolveKey(key('Enter', { ctrl: true, shift: true }), i), { kind: 'send', action: 'steer' });
  assert.deepEqual(resolveKey(key('Enter', { ctrl: true, alt: true }), i), { kind: 'send', action: 'default' });
});

test('with choices hidden, alternate keys send with the default mode', () => {
  const hidden = { ...asap, showChoices: false };
  assert.deepEqual(resolveKey(key('Enter', { ctrl: true, alt: true }), hidden), { kind: 'send', action: 'default' });
});

test('the Enter shortcut sends on plain Enter, Shift+Enter is a newline', () => {
  const e: KeyContext = { ...asap, sendShortcut: 'enter' };
  assert.deepEqual(resolveKey(key('Enter'), e), { kind: 'send', action: 'default' });
  assert.deepEqual(resolveKey(key('Enter', { shift: true }), e), { kind: 'none' });
});

test('tooltips name what Send and Stop do', () => {
  assert.match(sendTooltip(asap, true), /^Steer into the running task · Ctrl\+Enter · Ctrl\+Alt\+Enter to send now$/);
  assert.equal(sendTooltip(asap, false), 'Send · Ctrl+Enter');
  assert.equal(stopTooltip(true), 'Stop task · whatever has not been read is sent again right away · Esc stops the same way');
  assert.equal(stopTooltip(false), 'Stop task · whatever has not been read is sent again right away');
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

test('in flight: order, states, labels, tones and actions in one list', () => {
  const caps = { withdraw: true, deliverNow: true, stopTool: false, compact: false };
  const pending = [
    submission({ id: 'u1', state: 'uncertain', createdAt: 1 }),
    submission({ id: 's1', mode: 'steer', state: 'accepted', createdAt: 2 }),
    submission({ id: 'g1', mode: 'steer', state: 'saved', createdAt: 3 }),
    submission({ id: 'p1', mode: 'prompt', state: 'dispatching', createdAt: 4 }),
    submission({ id: 'r1', state: 'rejected', createdAt: 5 }),
    submission({ id: 'x1', state: 'withdrawn', createdAt: 6 }),
    submission({ id: 'o1', state: 'complete', createdAt: 7 }),
  ];
  const v = inFlightView(pending, { capabilities: caps });
  // In the order they were sent; settled rows are gone; nothing waits behind a turn.
  assert.deepEqual(
    v.map((i) => [i.submission.id, i.state, i.label, i.tone, i.actions]),
    [
      ['u1', 'uncertain', 'Not sent', 'amber', ['recover']],
      ['s1', 'pending', 'Pending', 'muted', ['withdraw']],
      ['g1', 'pending', 'Pending', 'muted', []],
      ['p1', 'sending', 'Sending', 'amber', []],
      ['r1', 'failed', 'Not delivered', 'error', ['recover']],
    ],
  );
  // Withdraw is hidden when the worker cannot withdraw.
  const v2 = inFlightView([submission({ id: 's2', mode: 'steer', state: 'accepted' })], { capabilities: { ...caps, withdraw: false } });
  assert.deepEqual(v2[0]!.actions, []);
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

test('live styling keeps every character and styles the Markdown it finds', () => {
  const src = '# Title\nSome **bold**, *it* and `code` at https://example.com/a?b=1.\n- item ~~gone~~\n```\n**not bold**\n```\n2x*3 and snake_case_name';
  const lines = liveRuns(src);
  assert.equal(lines.map((l) => l.map((r) => r.text).join('')).join('\n'), src, 'joining the runs gives the draft back');
  const styled = (line: number, text: string) => lines[line]!.find((r) => r.text === text)?.style ?? null;
  assert.match(styled(0, 'Title')!, /font-size:1\.32em/);
  assert.equal(styled(0, '# '), 'color:var(--muted);font-size:inherit');
  assert.match(styled(1, 'bold')!, /font-weight:650/);
  assert.equal(styled(1, '**'), 'color:var(--muted)');
  assert.match(styled(1, 'it')!, /italic/);
  assert.match(styled(1, 'code')!, /monospace/);
  assert.match(styled(1, 'https://example.com/a?b=1')!, /underline/, 'a link is underlined');
  assert.match(styled(2, '-')!, /var\(--accent\)/);
  assert.match(styled(2, 'gone')!, /line-through/);
  assert.equal(lines[4]!.length, 1, 'inside a fenced block nothing is parsed');
  assert.equal(lines[6]!.length, 1, 'a lone star and snake_case are plain text');
  const chip = liveRuns('/review the diff, not /review', new Set(['review']))[0]!;
  assert.match(chip[0]!.style, /background/, 'a leading known command is a chip');
  assert.equal(chip.length, 2, 'only the leading one');
  assert.equal(liveRuns('/nope x', new Set(['review']))[0]!.length, 1);
});
