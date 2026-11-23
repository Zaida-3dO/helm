// The queue row model: one table per string family. No host here — the words and the
// button gate are what the panel shows, so they are tested at their source. The build
// queue's families are tested the same way.

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { aheadOf, actionsFor, buildActionsFor, buildLine, buildStateLabel, claudeLine, queueCount, reasonLabel, sameLine, serverLine, waitLine } from './model.ts';
import type { BuildQueueEntry, QueueEntry, QueueSummary } from '../api/generated/index.ts';
import { dropBefore } from '../ui/useDragReorder.ts';

const e = (over: Partial<QueueEntry>): QueueEntry => ({
  chatId: 'c',
  title: 'T',
  reason: 'local_cap',
  class: 'normal',
  serverId: null,
  since: 0,
  top: false,
  ...over,
});

const s = (over: Partial<QueueSummary>): QueueSummary => ({
  entries: [],
  servers: [],
  claudeQueued: 0,
  claudeFull: false,
  ...over,
});

test('reasons read in plain words, unknown ones show as stored', () => {
  assert.equal(reasonLabel('local_cap'), 'Waiting for the local model');
  assert.equal(reasonLabel('claude_capacity'), 'Claude allowance used up');
  assert.equal(reasonLabel('at_limit'), 'Too many chats running');
  assert.equal(reasonLabel('manual'), 'Parked by you');
  assert.equal(reasonLabel('something_new'), 'something_new');
});

test('the wait line names the reason, the reset time, or the place ahead', () => {
  for (const [entry, ahead, until, want] of [
    [e({ reason: 'local_cap', serverId: 'strata' }), 0, null, 'Waiting for the local model'],
    [e({ reason: 'local_cap', serverId: 'strata' }), 2, null, 'Waiting for the local model · 2 waiting ahead'],
    [e({ reason: 'claude_capacity' }), 1, 'in 40 min', 'Claude allowance used up · opens again in 40 min'],
    [e({ reason: 'claude_capacity' }), 1, null, 'Claude allowance used up · 1 waiting ahead'],
    [e({ reason: 'manual' }), 3, null, 'Parked by you'],
    [e({ reason: 'at_limit' }), 0, null, 'Too many chats running'],
  ] as const) {
    assert.equal(waitLine(entry, ahead, until), want);
  }
});

test('ahead counts only starts on the same line, and manual holds have no line', () => {
  const entries = [
    e({ chatId: 'a', serverId: 'strata' }),
    e({ chatId: 'b', serverId: 'strata' }),
    e({ chatId: 'c', reason: 'claude_capacity', serverId: null }),
    e({ chatId: 'd', serverId: 'ollama' }),
    e({ chatId: 'e', reason: 'manual', serverId: 'strata' }),
  ];
  assert.equal(aheadOf(entries, 'b'), 1);
  assert.equal(aheadOf(entries, 'c'), 0, 'the Claude line is its own');
  assert.equal(aheadOf(entries, 'd'), 0, 'another server is another line');
  assert.equal(aheadOf(entries, 'e'), 0, 'a parked chat has no line');
  assert.equal(aheadOf(entries, 'missing'), 0);
  assert.equal(sameLine(e({ serverId: 'strata' }), e({ serverId: 'strata' })), true);
  assert.equal(sameLine(e({ serverId: null }), e({ serverId: null })), true);
  assert.equal(sameLine(e({ serverId: 'strata' }), e({ serverId: 'ollama' })), false);
  assert.equal(sameLine(e({ reason: 'manual' }), e({ serverId: 'strata' })), false);
});

test('a row offers only the moves the host would take', () => {
  for (const [entry, want] of [
    [e({ reason: 'local_cap' }), { top: true, upDown: true, drag: true, release: false, cancel: true }],
    [e({ reason: 'claude_capacity' }), { top: true, upDown: true, drag: true, release: false, cancel: true }],
    [e({ reason: 'manual' }), { top: false, upDown: false, drag: false, release: true, cancel: true }],
  ] as const) {
    assert.deepEqual(actionsFor(entry), want, entry.reason);
  }
});

test('the summary lines read in plain words', () => {
  assert.equal(serverLine(3, 1), '3 working, 1 waiting');
  assert.equal(serverLine(0, 4), '0 working, 4 waiting');
  const now = 1_000_000_000_000;
  assert.match(claudeLine(s({ claudeFull: true, claudeUntil: now + 30 * 60_000 }), now), /used up — new chats start again in 30 min/);
  assert.match(claudeLine(s({ claudeFull: true, claudeUntil: now + 120 * 60_000 }), now), /used up — new chats start again at \d\d:\d\d/);
  assert.equal(claudeLine(s({ claudeFull: true }), now), "Claude's allowance is used up — new chats start again when the window reopens.");
  assert.match(claudeLine(s({ claudeQueued: 2 }), now), /2 waiting for a place to open/);
  assert.equal(claudeLine(s({}), now), 'Nothing is waiting on Claude.');
  assert.equal(queueCount(null), 0);
  assert.equal(queueCount(s({ entries: [e({}), e({ chatId: 'x' })] })), 2);
});

const b = (over: Partial<BuildQueueEntry>): BuildQueueEntry => ({
  id: 'b1',
  profileId: 'p',
  title: 'T',
  position: 0,
  state: 'waiting',
  ...over,
});

test('build states read in plain words, unknown ones show as stored', () => {
  assert.equal(buildStateLabel('waiting'), 'Waiting');
  assert.equal(buildStateLabel('running'), 'Running');
  assert.equal(buildStateLabel('paused'), 'Paused');
  assert.equal(buildStateLabel('done'), 'Done');
  assert.equal(buildStateLabel('something_new'), 'something_new');
});

test('the build line names the work and the model, clipping a long brief', () => {
  for (const [entry, want] of [
    [b({ brief: 'Read the folder and import each file.' }), 'Read the folder and import each file. · default model'],
    [b({ itemId: 'item-42' }), 'Board item item-42 · default model'],
    [b({ brief: 'Trim first', itemId: 'item-42', model: 'claude-sonnet-5' }), 'Trim first · on claude-sonnet-5'],
    [b({}), 'Board item · default model'],
  ] as const) {
    assert.equal(buildLine(entry), want, entry.state);
  }
  assert.equal(buildLine(b({ brief: 'x'.repeat(200) })), `${'x'.repeat(119)}… · default model`, 'a long brief is shortened to fit the row');
});

test('a build row offers only what the host would take for its state', () => {
  for (const [entry, want] of [
    [b({ state: 'waiting' }), { open: false, pause: true, resume: false, reorder: true, remove: true }],
    [b({ state: 'running', chatId: 'c1' }), { open: true, pause: true, resume: false, reorder: false, remove: true }],
    [b({ state: 'paused' }), { open: false, pause: false, resume: true, reorder: true, remove: true }],
    [b({ state: 'paused', chatId: 'c1' }), { open: true, pause: false, resume: true, reorder: true, remove: true }],
    [b({ state: 'done', chatId: 'c1' }), { open: true, pause: false, resume: false, reorder: false, remove: true }],
  ] as const) {
    assert.deepEqual(buildActionsFor(entry), want, entry.state);
  }
});

test('a drop one place down is a move; dropping where it was is not', () => {
  const line = ['a', 'b', 'c', 'd'];
  for (const [from, to, want] of [
    [0, 0, undefined], // let go where it started
    [0, 1, 'c'], // one place down: in front of c
    [0, 3, null], // to the very end
    [2, 1, 'b'], // one place up: in front of the row it passed
    [3, 0, 'a'], // to the front
  ] as const) {
    assert.equal(dropBefore(line, from, to), want, `${from} -> ${to}`);
  }
});
