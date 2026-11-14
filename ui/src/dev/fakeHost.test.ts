import assert from 'node:assert/strict';
import { test } from 'node:test';
import { createFakeHost, registerFakeOps } from './fakeHost.ts';

test('registered fake ops answer by prefix; an action send keeps the draft', async () => {
  registerFakeOps(['zz_probe_'], (op, args) => ({ op, args }));
  const invoke = createFakeHost(() => undefined, { stepMs: 1 });
  const call = (op: string, args: Record<string, unknown> = {}) => invoke('api', { op, args }) as Promise<Record<string, unknown>>;
  assert.deepEqual(await call('zz_probe_list', { x: 1 }), { op: 'zz_probe_list', args: { x: 1 } });
  await assert.rejects(call('nothing_here'), (e: { code: string }) => e.code === 'not_implemented');
  const chat = await call('chat_create', { cwd: 'C:/w' });
  await call('draft_save', { chatId: chat.id, text: 'typed', revision: 10 });
  const r = await call('chat_send', { chatId: chat.id, submissionId: 'a', text: 'Update', action: 'default', draftRevision: 0, origin: 'action' });
  assert.equal((r.chat as { draft: string }).draft, 'typed');
  // chat_fork: a copy titled "Fork · …" (no stacked prefix), the same fork id answers the same chat,
  // and a side chat quotes its excerpt and stays out of the list.
  const fork = await call('chat_fork', { chatId: chat.id, forkId: 'f1' });
  assert.equal(fork.title, 'Fork · Update');
  assert.equal((await call('chat_fork', { chatId: chat.id, forkId: 'f1' })).id, fork.id);
  assert.equal((await call('chat_fork', { chatId: fork.id, forkId: 'f2' })).title, 'Fork · Update');
  const side = await call('chat_fork', { chatId: chat.id, forkId: 'f3', side: true, excerpt: 'one\ntwo' });
  assert.equal(side.sideSourceId, chat.id);
  assert.match(String(side.draft), /^Regarding this excerpt:\n\n> one\n> two/);
  await assert.rejects(call('chat_fork', { chatId: chat.id, forkId: 'f4', side: true }), (e: { code: string }) => e.code === 'bad_args');
  assert.ok(!((await call('chat_list')) as unknown as { id: string }[]).some((c) => c.id === side.id));
  const att = await call('attention_get');
  assert.equal(att.busy, 1);
});

test('attachment_get answers the stored image, and for a thumbnail the same or a smaller copy', async () => {
  const invoke = createFakeHost(() => undefined, { stepMs: 1 });
  const call = (op: string, args: Record<string, unknown> = {}) => invoke('api', { op, args }) as Promise<Record<string, unknown>>;
  const chat = await call('chat_create', { cwd: 'C:/w' });
  const put = await call('attachment_put', { chatId: chat.id, name: 'a.png', mime: 'image/png', bytes: 'iVBORw0=' });
  const args = { chatId: chat.id, attachmentId: put.attachmentId };
  assert.deepEqual(await call('attachment_get', args), { mime: 'image/png', bytes: 'iVBORw0=' });
  // No canvas here, so the thumbnail is the stored image; the host still accepts the flag.
  assert.deepEqual(await call('attachment_get', { ...args, thumb: true }), { mime: 'image/png', bytes: 'iVBORw0=' });
  await assert.rejects(call('attachment_get', { ...args, attachmentId: 'nope', thumb: true }), (e: { code: string }) => e.code === 'not_found');
});
