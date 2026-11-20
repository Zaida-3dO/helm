// The queue row model: one table per string family. No host here — the words and the
// button gate are what the panel shows, so they are tested at their source.

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { aheadOf, actionsFor, claudeLine, needsSignIn, queueCount, reasonLabel, sameLine, serverLine, signInLine, waitLine } from './model.ts';
import type { QueueEntry, QueueSignIn, QueueSummary } from '../api/generated/index.ts';

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
  signIn: [],
  ...over,
});

test('reasons read in plain words, unknown ones show as stored', () => {
  assert.equal(reasonLabel('local_cap'), 'Waiting for the local model');
  assert.equal(reasonLabel('claude_capacity'), 'Claude allowance used up');
  assert.equal(reasonLabel('at_limit'), 'Too many chats running');
  assert.equal(reasonLabel('manual'), 'Parked by you');
  assert.equal(reasonLabel('sign_in'), "Waiting while Claude's sign-in is checked");
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
    [e({ reason: 'sign_in', serverId: null }), 0, null, "Waiting while Claude's sign-in is checked"],
    [e({ reason: 'sign_in', serverId: null }), 1, null, "Waiting while Claude's sign-in is checked · 1 waiting ahead"],
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
    [e({ reason: 'local_cap' }), { top: true, upDown: true, release: false, cancel: true }],
    [e({ reason: 'claude_capacity' }), { top: true, upDown: true, release: false, cancel: true }],
    [e({ reason: 'manual' }), { top: false, upDown: false, release: true, cancel: true }],
  ] as const) {
    assert.deepEqual(actionsFor(entry), want, entry.reason);
  }
});

test('the summary lines read in plain words', () => {
  assert.equal(serverLine(3, 1), '3 working, 1 waiting');
  assert.equal(serverLine(0, 4), '0 working, 4 waiting');
  const now = 1_000_000_000_000;
  assert.match(claudeLine(s({ claudeFull: true, claudeUntil: now + 30 * 60_000 }), now), /used up; starts wait until in 30 min/);
  assert.match(claudeLine(s({ claudeQueued: 2 }), now), /2 waiting for a place to open/);
  assert.equal(claudeLine(s({}), now), 'Nothing is waiting on Claude.');
  assert.equal(queueCount(null), 0);
  assert.equal(queueCount(s({ entries: [e({}), e({ chatId: 'x' })] })), 2);
});

test('a failed token refresh reads as retrying until Helm gives up, then asks for a sign-in', () => {
  const c = (gaveUp: boolean): QueueSignIn => ({ chatId: 'c', title: 'T', since: 0, gaveUp });
  assert.match(signInLine(c(false)), /trying again/);
  assert.match(signInLine(c(true)), /Sign in again/);
  assert.equal(needsSignIn(null), false);
  assert.equal(needsSignIn(s({})), false);
  assert.equal(needsSignIn(s({ signIn: [c(false)] })), false, 'still being tried: nothing for the person to do');
  assert.equal(needsSignIn(s({ signIn: [c(false), c(true)] })), true);
});
