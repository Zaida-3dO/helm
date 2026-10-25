import { test } from 'node:test';
import assert from 'node:assert/strict';
import { flatten, NoticeStack, FRESH_MS, ANNOUNCED_MAX, STACK_MAX } from './model.ts';

const c = (id: string, chatId: string, at: number, extra: Partial<{ read: boolean; dismissed: boolean; title: string }> = {}) => ({
  id,
  chatId,
  title: extra.title ?? 'T',
  project: 'P',
  preview: 'done\n  well',
  completedAt: at,
  read: extra.read ?? false,
  dismissed: extra.dismissed ?? false,
});

test('posts once per id, only while fresh, never for read, dismissed or on-screen chats', () => {
  let now = 100_000;
  const s = new NoticeStack(() => now);
  assert.equal(s.completion(c('k1', 'a', now), null), true);
  assert.equal(s.completion(c('k1', 'a', now), null), false, 'announced once');
  assert.equal(s.completion(c('k2', 'b', now - FRESH_MS - 1), null), false, 'stale');
  assert.equal(s.completion(c('k3', 'c', now, { read: true }), null), false);
  assert.equal(s.completion(c('k4', 'd', now, { dismissed: true }), null), false);
  assert.equal(s.completion(c('k5', 'e', now), 'e'), false, 'the chat on screen');
  assert.deepEqual(s.list().map((n) => n.chatId), ['a']);
  assert.equal(s.list()[0]?.preview, 'done well', 'flattened to one line');
  now += 1;
});

test('one per chat: a newer completion replaces the older; the stack is capped', () => {
  const s = new NoticeStack(() => 0);
  s.completion(c('k1', 'a', 0), null);
  s.completion(c('k2', 'a', 0, { title: 'Newer' }), null);
  assert.equal(s.list().length, 1);
  assert.equal(s.list()[0]?.title, 'Newer');
  for (let i = 0; i < STACK_MAX + 2; i++) s.completion(c(`x${i}`, `chat${i}`, 0), null);
  assert.equal(s.list().length, STACK_MAX);
});

test('withdraw on read: a chat no longer unread in the attention summary loses its notice', () => {
  const s = new NoticeStack(() => 0);
  s.completion(c('k1', 'a', 0), null);
  s.completion(c('k2', 'b', 0), null);
  s.summary(['a', 'b']);
  assert.equal(s.list().length, 2);
  s.summary(['b']);
  assert.deepEqual(s.list().map((n) => n.chatId), ['b']);
  s.withdraw('b');
  assert.equal(s.list().length, 0);
});

test('the announced memory is bounded at 1,024 ids', () => {
  const s = new NoticeStack(() => 0);
  for (let i = 0; i <= ANNOUNCED_MAX; i++) s.completion(c(`k${i}`, 'z', 0), null);
  assert.equal(s.completion(c('k0', 'z', 0), null), true, 'the oldest id fell out');
});

test('previews are one line of at most 240 characters', () => {
  assert.equal(flatten('a'.repeat(300)).length, 240);
  assert.ok(flatten('a'.repeat(300)).endsWith('…'));
  assert.equal(flatten('x\n\ny'), 'x y');
});

test('previews show the words of a Markdown reply, not its marks', () => {
  const cases: [string, string][] = [
    ['Edited **main.ts** and `app.ts`', 'Edited main.ts and app.ts'],
    ['## Done\n- fixed _two_ bugs\n1. see [the notes](https://x.test/n)', 'Done fixed two bugs see the notes'],
    ['> quoted ~~old~~ text and a_b_c and 2 * 3 * 4', 'quoted old text and a_b_c and 2 * 3 * 4'],
    ['```ts\nconst a = 1;\n```', 'const a = 1;'],
  ];
  for (const [md, plain] of cases) assert.equal(flatten(md), plain, md);
});

test('a blank title reads "Task finished"', () => {
  const s = new NoticeStack(() => 0);
  s.completion(c('k1', 'a', 0, { title: '  ' }), null);
  assert.equal(s.list()[0]?.title, 'Task finished');
});
