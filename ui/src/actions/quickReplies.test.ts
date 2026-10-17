import assert from 'node:assert/strict';
import { test } from 'node:test';
import { endsWithQuestion, quickReplies, type QuickReplyContext } from './quickReplies.ts';

const row = (role: string, text: string, status = 'complete') => ({ role, text, status }) as QuickReplyContext['rows'][number];
const ctx = (over: Partial<QuickReplyContext> = {}): QuickReplyContext => ({
  status: 'idle',
  rows: [row('user', 'hi'), row('assistant', 'All done.')],
  interactionOpen: false,
  ...over,
});
const ids = (c: QuickReplyContext) => quickReplies(c).map((q) => q.id);

test('an idle chat whose last row is a finished reply offers Continue', () => {
  assert.deepEqual(ids(ctx()), ['continue']);
});

test('a reply ending in a question also offers Yes and No', () => {
  assert.deepEqual(ids(ctx({ rows: [row('assistant', 'Shall I go ahead?')] })), ['continue', 'yes', 'no']);
});

test('the chips send fixed lowercase words', () => {
  const sent = quickReplies(ctx({ rows: [row('assistant', 'Ready?')] })).map((q) => q.text);
  assert.deepEqual(sent, ['continue', 'yes', 'no']);
});

test('nothing shows while a turn is running, starting or stopping', () => {
  for (const status of ['running', 'starting', 'stopping'] as const) assert.deepEqual(ids(ctx({ status })), [], status);
});

test('stopped and interrupted chats can still be continued', () => {
  assert.deepEqual(ids(ctx({ status: 'stopped' })), ['continue']);
  assert.deepEqual(ids(ctx({ status: 'interrupted' })), ['continue']);
});

test('nothing shows while an interaction card is open', () => {
  assert.deepEqual(ids(ctx({ interactionOpen: true, rows: [row('assistant', 'Ok?')] })), []);
});

test('nothing shows unless the last row is a complete assistant reply', () => {
  assert.deepEqual(ids(ctx({ rows: [row('assistant', 'Ok?'), row('user', 'yes')] })), [], 'last row is the user');
  assert.deepEqual(ids(ctx({ rows: [row('assistant', 'Ok?', 'streaming')] })), [], 'still streaming');
  assert.deepEqual(ids(ctx({ rows: [row('assistant', 'Ok?', 'error')] })), [], 'errored');
  assert.deepEqual(ids(ctx({ rows: [row('assistant', 'Ok?'), row('toolResult', 'out')] })), [], 'a tool result is last');
  assert.deepEqual(ids(ctx({ rows: [row('assistant', '   ')] })), [], 'empty reply');
  assert.deepEqual(ids(ctx({ rows: [] })), [], 'no rows');
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
