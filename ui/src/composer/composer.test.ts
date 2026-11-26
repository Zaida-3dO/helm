import { test } from 'node:test';
import assert from 'node:assert/strict';
import { resolveKey, sendTooltip, stopTooltip, type KeyContext, type KeyShape } from './keymap.ts';
import { DRAFT_DEBOUNCE_MS, DraftSaver, nextRevision, takeHostDraft, type Timers } from './drafts.ts';
import { claudeHold, holdLine, inFlightView, waitReason } from './inFlight.ts';
import { row, submission } from '../dev/fixtures.ts';
import type { MessageRow } from '../api/types.ts';
import { liveRuns, wrapMarker } from './markup.ts';
import { permissionLabel } from './permission.ts';
import { SendHold, holdStep } from './hold.ts';
import { fastModeShown, fastModeTitle, fastModeVisible } from './fastMode.ts';

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

test('a newer host draft replaces the editor, except typing a rolled-over chat has not saved yet', async () => {
  const t = fakeTimers();
  const saves: Array<[string, number]> = [];
  const d = new DraftSaver('', 0, async (text, rev) => void saves.push([text, rev]), t);
  assert.equal(d.unsaved(), null);
  // Nothing newer than the composer's own revision: the editor is left alone.
  assert.equal(takeHostDraft(d, { draft: 'x', draftRevision: 0, nextSessionId: null }), null);
  // Recover to draft on an ordinary chat replaces what is shown, typed or not.
  d.edit('typing');
  assert.equal(takeHostDraft(d, { draft: 'recovered', draftRevision: 5_000_000_000_000, nextSessionId: null }), 'recovered');
  await d.flush();
  // The chat rolled over and the host moved the draft on while the reader had unsaved text: the
  // text stays and is saved under a revision newer than the host's.
  d.edit('half a thought, and a bit more');
  const host = 9_000_000_000_000;
  assert.equal(takeHostDraft(d, { draft: '', draftRevision: host, nextSessionId: 'next' }), null);
  assert.ok(d.revision > host);
  await d.flush();
  assert.deepEqual(saves.at(-1), ['half a thought, and a bit more', d.revision]);
  // Text already saved is not unsaved: the host's empty draft simply replaces it.
  assert.equal(takeHostDraft(d, { draft: '', draftRevision: host + 1_000_000_000_000, nextSessionId: 'next' }), '');
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
  // Only an unread message says why it waits.
  assert.deepEqual(v.map((i) => i.reason), [null, 'Claude reads it at its next step', 'Claude reads it at its next step', null, null]);
  assert.equal(inFlightView(pending, { capabilities: caps, reason: 'Waiting for Bash to finish' })[1]!.reason, 'Waiting for Bash to finish');

  // A message whose own row is in the transcript is not listed a second time. A prompt that opens a
  // turn has its row (saved) from the moment it is handed over, the bubble says Sending and then
  // Pending until Claude reads it; a steer's row comes when Claude reads it. A message known
  // unsent keeps its bubble: it carries Recover.
  const own = (id: string, extra: MessageRow['details'] = {}) => row({ id: Number(id.slice(1)) + 100, role: 'user', status: 'saved', details: { submissionId: id, ...extra } });
  const listed = (rows: MessageRow[]) => inFlightView(pending, { capabilities: caps, rows }).map((i) => i.submission.id);
  assert.deepEqual(listed([]), ['u1', 's1', 'g1', 'p1', 'r1']);
  assert.deepEqual(listed([own('p1')]), ['u1', 's1', 'g1', 'r1'], 'a prompt that opens a turn: its saved row stands for it while it is Sending');
  assert.deepEqual(listed([own('s1', { mode: 'steer' }), own('g1', { mode: 'steer' })]), ['u1', 'p1', 'r1'], 'a steer read by Claude: its row stands for it, Pending or not yet handed over');
  assert.deepEqual(listed([own('u1'), own('r1')]), ['u1', 's1', 'g1', 'p1', 'r1'], 'Not sent and Not delivered keep their bubble and their Recover');
  assert.deepEqual(listed([row({ id: 7, role: 'assistant', details: { submissionId: 'p1' } }), row({ id: 8, role: 'user', details: {} })]), ['u1', 's1', 'g1', 'p1', 'r1'], 'only a user row that names the message stands for it');
});

test('wait reason: the running tool of this turn, else the next step', () => {
  const user = row({ id: 1, role: 'user', text: 'go' });
  const steer = row({ id: 4, role: 'user', text: 'also', details: { mode: 'steer' } });
  const running = (name: string, id = 2) => row({ id, role: 'toolResult', toolName: name, status: 'streaming' });
  for (const [rows, want] of [
    [[], 'Claude reads it at its next step'],
    [[user, row({ id: 2, role: 'toolResult', toolName: 'Read' })], 'Claude reads it at its next step'],
    [[user, running('Bash')], 'Waiting for Bash to finish'],
    [[user, running('mcp__helm__app_wait_for')], 'Waiting for a Helm tool to finish'],
    [[user, running('mcp__github__search_code')], 'Waiting for github · search_code to finish'],
    [[user, { ...running('mcp__helm__app_mcp_call'), details: { toolArgs: { name: 'github__search_code', arguments: {} } } }], 'Waiting for github · search_code to finish'],
    [[user, { ...running('mcp__helm__app_mcp_call'), details: { toolArgs: '{"name":"app_slow"}' } }], 'Waiting for a Helm tool to finish'],
    [[user, running('Bash', 2), steer, row({ id: 5, role: 'assistant', text: 'ok' })], 'Waiting for Bash to finish'],
    [[row({ id: 0, role: 'toolResult', toolName: 'Bash', status: 'streaming' }), user], 'Claude reads it at its next step'],
  ] as const) assert.equal(waitReason(rows as never), want, JSON.stringify(rows.map((r) => r.id)));
});

test('the fast-mode bolt: Claude chats only, own pick over default, no guessing', () => {
  // A local-model chat hides the control entirely; Claude and an unset model show it.
  for (const [model, want] of [
    ['claude-opus-5-5', true],
    [null, true],
    [undefined, true],
    ['local/ollama/llama3', false],
    ['local/strata/x', false],
  ] as const) assert.equal(fastModeVisible(model), want, String(model));
  // The chat's own pick wins; with none the PC's default shows; a default not read yet holds the bolt back.
  assert.equal(fastModeShown(true, false), true);
  assert.equal(fastModeShown(false, true), false);
  assert.equal(fastModeShown(null, true), true);
  assert.equal(fastModeShown(null, false), false);
  assert.equal(fastModeShown(null, null), null);
  assert.equal(fastModeShown(true, null), true, 'an own pick needs no default');
  // The tooltip says what it costs and when it applies, and names where the fallback lives.
  const t = fastModeTitle(true);
  assert.match(t, /on/, 'says its state');
  assert.match(t, /higher token price/);
  assert.match(t, /agent next starts/);
  assert.match(t, /Settings → Runtime/);
  assert.match(fastModeTitle(false), /off/);
});

test('a Claude usage hold names the allowance and its opening time', () => {
  const now = 1_763_000_000_000;
  assert.equal(
    holdLine({ kind: 'queued', reason: 'claude_capacity', pct: 95, until: now + 20 * 60_000 }, now),
    "Waiting for Claude's plan allowance (95% used) — opens again in 20 min",
  );
  // A missing part says nothing about itself; the line still names the limit.
  assert.equal(holdLine({ kind: 'queued', reason: 'claude_capacity' }, now), "Waiting for Claude's plan allowance");
  assert.equal(
    holdLine({ kind: 'queued', reason: 'at_limit', ahead: 1 }, now),
    'Waiting for a chat place — Helm is running all its chats at once',
  );
  // Every other wait — and a chat that is not queued — keeps the generic line.
  assert.equal(holdLine({ kind: 'queued', reason: 'local_cap', ahead: 2 }, now), null);
  assert.equal(holdLine({ kind: 'queued' }, now), null);
  assert.equal(holdLine({ kind: 'generating', since: now }, now), null);
  assert.equal(holdLine(null, now), null);
  // Only the Claude hold arms the override; the chat limit has no button, only its words.
  assert.ok(claudeHold({ kind: 'queued', reason: 'claude_capacity' }));
  assert.ok(!claudeHold({ kind: 'queued', reason: 'at_limit' }));
  assert.ok(!claudeHold({ kind: 'queued' }));
  assert.ok(!claudeHold(undefined));
});

test('a waiting message on a Claude hold offers Start anyway', () => {
  const caps = { withdraw: true, deliverNow: true, stopTool: false, compact: false };
  const v = inFlightView([submission({ id: 'p1', mode: 'prompt', state: 'saved' })], { capabilities: caps, claudeHold: true });
  assert.deepEqual(v.map((i) => [i.submission.id, i.label, i.actions]), [['p1', 'Pending', ['startAnyway']]]);
  // No hold: the same row offers nothing it did before.
  assert.deepEqual(inFlightView([submission({ id: 'p2', mode: 'prompt', state: 'saved' })], { capabilities: caps })[0]!.actions, []);
});

test('a send to a device that is away waits and is handed over exactly once, keeping its submission id', () => {
  const h = new SendHold();
  assert.equal(h.release(), null, 'nothing waits yet');
  h.hold({ submissionId: 'sub-1', action: 'default', text: 'hello', deviceUid: 'desk' });
  const held = h.release();
  assert.deepEqual(held, { submissionId: 'sub-1', action: 'default', text: 'hello', deviceUid: 'desk' }, 'once the device is live the held send comes to light, under the id made when Send was pressed');
  assert.equal(h.release(), null, 'more live updates never send it a second time, whatever the answer was');
  const c = new SendHold();
  c.hold({ submissionId: 'sub-3', action: 'steer', text: 'wait for me', deviceUid: 'desk' });
  assert.deepEqual(c.cancel()?.submissionId, 'sub-3', 'Cancel takes the message back');
  assert.equal(c.release(), null, 'and nothing goes on its own afterwards');
  const r = new SendHold();
  r.hold({ submissionId: 'sub-4', action: 'default', text: 'first', deviceUid: 'desk' });
  r.hold({ submissionId: 'sub-5', action: 'default', text: 'second', deviceUid: 'desk' });
  assert.equal(r.release()?.submissionId, 'sub-5', 'pressing Send again replaces what waited');
});

test('a held send only goes to the device it waited for, once that device is live and nothing else is starting', () => {
  const held = { deviceUid: 'desk' };
  const live = (deviceUid: string) => ({ deviceUid, blocked: null });
  const away = (deviceUid: string) => ({ deviceUid, blocked: 'Desk is offline' });
  for (const [h, remote, sending, want, why] of [
    [held, live('desk'), false, 'release', 'its device is back and nothing else is starting'],
    [held, away('desk'), false, 'keep', 'its device is still away'],
    [held, live('desk'), true, 'keep', 'another send is still starting: it waits for that one, it is not dropped'],
    [held, live('laptop'), false, 'cancel', 'another device was picked: it never goes there'],
    [held, away('laptop'), false, 'cancel', 'another device was picked, even one that is away'],
    [held, null, false, 'cancel', 'the chat is on this PC now: it is taken back'],
    [null, live('desk'), false, 'keep', 'nothing waits'],
    [null, null, false, 'keep', 'nothing waits, nothing to take back'],
  ] as const) assert.equal(holdStep(h, remote, sending), want, why);
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
