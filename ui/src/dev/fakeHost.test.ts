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
  const att = await call('attention_get');
  assert.equal(att.busy, 1);
});
