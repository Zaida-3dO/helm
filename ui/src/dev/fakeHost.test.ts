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

test('a roll over makes one stack: old session archived, one window spans both, find reaches both', async () => {
  const events: unknown[] = [];
  const invoke = createFakeHost((e) => {
    events.push(e);
  }, { stepMs: 1 });
  const call = (op: string, args: Record<string, unknown> = {}) => invoke('api', { op, args }) as Promise<Record<string, unknown>>;
  const a = await call('chat_create', { cwd: 'C:/w' });
  await call('chat_send', { chatId: a.id, submissionId: 's1', text: 'zebra word in the first session', action: 'default', draftRevision: 0 });
  const r = await call('chat_roll_over', { chatId: a.id });
  assert.deepEqual(r, { started: true });
  const st = await call('chat_stack', { chatId: a.id });
  assert.equal(st.stackId, a.id);
  const sessions = st.sessions as { id: string; archived: boolean }[];
  const b = sessions[sessions.length - 1]!.id;
  assert.notEqual(b, a.id);
  assert.equal(sessions[0]!.archived, true, 'the old session archived');
  assert.equal(st.latestId, b);
  // The successor keeps the title; a plain page of it shows only its rows; a stack page shows both
  // sessions in one order with a mark where they meet.
  assert.equal(((await call('chat_get', { chatId: b })).chat as { title: string }).title, 'zebra word in the first session');
  const plain = await call('transcript_page', { chatId: b });
  assert.ok((plain.messages as { chatId: string }[]).every((m) => m.chatId === b));
  const stack = await call('transcript_page', { chatId: b, stack: true });
  const both = stack.messages as { id: number; chatId: string }[];
  assert.equal(both.some((m) => m.chatId === a.id), true);
  assert.deepEqual((stack.sessionMarks as { sessionId: string }[]).map((m) => m.sessionId), [a.id]);
  // Find across the stack reaches the archived session; the single-chat find does not.
  const found = await call('stack_find', { chatId: b, query: 'zebra' });
  assert.equal((found.rows as { sessionId: string }[])[0]?.sessionId, a.id);
  const one = await call('chat_find', { chatId: b, query: 'zebra' });
  assert.equal((one.rows as unknown[]).length, 0);
  // The open event the roll over sent names the successor while the reader is on the old window.
  const open = events.find((e) => (e as { kind?: string }).kind === 'shell_open_chat') as { chatId: string; ifViewing: string } | undefined;
  assert.equal(open?.chatId, b);
  assert.equal(open?.ifViewing, a.id);
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
