import assert from 'node:assert/strict';
import { test } from 'node:test';
import { endsWithQuestion, quickReplies, stoppedEarly, type QuickReplyContext } from './quickReplies.ts';

const row = (role: string, text: string, status = 'complete') => ({ role, text, status }) as QuickReplyContext['rows'][number];
const ctx = (over: Partial<QuickReplyContext> = {}): QuickReplyContext => ({
  status: 'idle',
  rows: [row('user', 'hi'), row('assistant', 'All done.')],
  interactionOpen: false,
  ...over,
});
const ids = (c: QuickReplyContext) => quickReplies(c).map((q) => q.id);

test('a normal finished reply shows no chip: there is nothing to continue', () => {
  assert.deepEqual(ids(ctx()), []);
  assert.deepEqual(ids(ctx({ rows: [] })), [], 'no rows');
});

test('a reply ending in a question offers Yes and No, and not Continue', () => {
  assert.deepEqual(ids(ctx({ rows: [row('assistant', 'Shall I go ahead?')] })), ['yes', 'no']);
});

test('the chips send fixed lowercase words', () => {
  const sent = quickReplies(ctx({ status: 'stopped', rows: [row('assistant', 'Ready?')] })).map((q) => q.text);
  assert.deepEqual(sent, ['continue', 'yes', 'no']);
});

test('each chip says what it does in its tooltip text, starting with its visible label', () => {
  const all = quickReplies(ctx({ status: 'stopped', rows: [row('assistant', 'Ready?')] }));
  for (const q of all) assert.ok(q.hint.startsWith(q.label), q.id);
  assert.match(all[0]?.hint ?? '', /picks up where it stopped/);
});

test('nothing shows while a turn is running, starting or stopping', () => {
  for (const status of ['running', 'starting', 'stopping'] as const) {
    assert.deepEqual(ids(ctx({ status, rows: [row('assistant', 'Cut off?', 'interrupted')] })), [], status);
  }
});

test('a stopped or interrupted chat offers Continue', () => {
  assert.deepEqual(ids(ctx({ status: 'stopped' })), ['continue']);
  assert.deepEqual(ids(ctx({ status: 'interrupted' })), ['continue']);
  assert.deepEqual(ids(ctx({ status: 'stopped', rows: [row('assistant', 'Ok?')] })), ['continue', 'yes', 'no']);
});

test('an interrupted or errored last row offers Continue even when the chat reads idle', () => {
  assert.deepEqual(ids(ctx({ rows: [row('user', 'go'), row('assistant', 'Partial', 'interrupted')] })), ['continue']);
  assert.deepEqual(ids(ctx({ rows: [row('user', 'go'), row('assistant', 'Ok?', 'error')] })), ['continue'], 'errored: no Yes or No');
  assert.deepEqual(ids(ctx({ rows: [row('assistant', '', 'error')] })), ['continue'], 'an error with no text');
  assert.deepEqual(ids(ctx({ rows: [row('toolResult', 'out', 'interrupted')] })), ['continue']);
});

test('an earlier interrupted row does not count once a later row has finished', () => {
  assert.deepEqual(ids(ctx({ rows: [row('assistant', 'Cut off', 'interrupted'), row('user', 'continue'), row('assistant', 'Done.')] })), []);
});

test('a failed message of the user is not something to continue', () => {
  assert.deepEqual(ids(ctx({ rows: [row('assistant', 'Done.'), row('user', 'next', 'error')] })), []);
});

test('nothing shows while an interaction card is open', () => {
  assert.deepEqual(ids(ctx({ interactionOpen: true, status: 'stopped', rows: [row('assistant', 'Ok?')] })), []);
});

test('Yes and No need a complete assistant reply as the last row', () => {
  assert.deepEqual(ids(ctx({ rows: [row('assistant', 'Ok?'), row('user', 'yes')] })), [], 'last row is the user');
  assert.deepEqual(ids(ctx({ rows: [row('assistant', 'Ok?', 'streaming')] })), [], 'still streaming');
  assert.deepEqual(ids(ctx({ rows: [row('assistant', 'Ok?'), row('toolResult', 'out')] })), [], 'a tool result is last');
  assert.deepEqual(ids(ctx({ rows: [row('assistant', '   ')] })), [], 'empty reply');
});

test('endsWithQuestion ignores trailing markdown, quotes and brackets', () => {
  assert.equal(endsWithQuestion('Do you want this?'), true);
  assert.equal(endsWithQuestion('Do you want this?  \n'), true);
  assert.equal(endsWithQuestion('**Do you want this?**'), true);
  assert.equal(endsWithQuestion('Is it "done"?'), true);
  assert.equal(endsWithQuestion('(maybe?)'), true);
  assert.equal(endsWithQuestion('Done. Next?”'), true);
  assert.equal(endsWithQuestion('Done.'), false);
  assert.equal(endsWithQuestion('What now? I will wait.'), false, 'the last sentence is not a question');
  assert.equal(endsWithQuestion(''), false);
});
