import assert from 'node:assert/strict';
import { test } from 'node:test';
import type { Api } from '../api/client.ts';
import type { ActionButton } from '../api/actions.ts';
import { ActionError, buildActionSend, EXPLAIN_PLAINLY_TEXT, runButton, sendActionText } from './send.ts';

interface Call {
  op: string;
  args: Record<string, unknown>;
}

function fakeApi(handlers: Record<string, (args: Record<string, unknown>) => unknown>): { api: Api; calls: Call[] } {
  const calls: Call[] = [];
  const api = (async (op: string, args: unknown) => {
    const a = (args ?? {}) as Record<string, unknown>;
    calls.push({ op, args: a });
    const h = handlers[op];
    if (!h) throw new Error(`no handler for ${op}`);
    return h(a);
  }) as Api;
  return { api, calls };
}

const button = (over: Partial<ActionButton> = {}): ActionButton => ({
  id: 'b1',
  label: 'Update',
  kind: 'prompt',
  text: 'Give me an update.',
  projectId: null,
  placement: 'topbar',
  hidden: false,
  seedId: null,
  ...over,
});

test('an action send is a default-mode send with origin action that leaves the draft alone', () => {
  const a = buildActionSend('c1', 'hello', 's1');
  assert.deepEqual(a, { chatId: 'c1', submissionId: 's1', text: 'hello', action: 'default', origin: 'action', draftRevision: -1 });
});

test('every action send has a fresh submission id', () => {
  assert.notEqual(buildActionSend('c', 'x').submissionId, buildActionSend('c', 'x').submissionId);
});

test('a prompt button sends exactly its text into the chat in view', async () => {
  const { api, calls } = fakeApi({ chat_send: () => ({}) });
  await runButton(api, button(), { chatId: 'c1', select: () => assert.fail('no selection for a prompt') });
  assert.equal(calls.length, 1);
  assert.equal(calls[0]!.op, 'chat_send');
  assert.equal(calls[0]!.args.text, 'Give me an update.');
  assert.equal(calls[0]!.args.chatId, 'c1');
  assert.equal(calls[0]!.args.origin, 'action');
  assert.ok(!('draft' in calls[0]!.args), 'no draft is sent');
  assert.ok(!calls.some((c) => c.op === 'draft_save'), 'the draft is never saved or cleared');
});

test('a prompt button with no chat open does nothing and says why', async () => {
  const { api, calls } = fakeApi({});
  await assert.rejects(runButton(api, button(), { chatId: null, select: () => undefined }), ActionError);
  assert.equal(calls.length, 0);
});

test('a send failure surfaces as a readable error', async () => {
  const { api } = fakeApi({
    chat_send: () => {
      throw new Error('the chat is gone');
    },
  });
  await assert.rejects(runButton(api, button(), { chatId: 'c1', select: () => undefined }), /Could not send "Update": the chat is gone/);
});

test('a new-chat button that is not bound to a project does nothing', async () => {
  const { api, calls } = fakeApi({});
  const b = button({ kind: 'new_chat', label: 'New session', text: "Let's start." });
  await assert.rejects(runButton(api, b, { chatId: null, select: () => undefined }), /not bound to a project/);
  assert.equal(calls.length, 0);
});

test('a bound new-chat button creates a chat in the project folder, sends the text there, and selects it', async () => {
  const selected: string[] = [];
  const { api, calls } = fakeApi({
    project_list: () => [{ id: 'p0', path: 'C:/other' }, { id: 'p1', path: 'C:/work/site' }],
    chat_create: () => ({ id: 'new-chat' }),
    chat_send: () => ({}),
  });
  const b = button({ kind: 'new_chat', projectId: 'p1', text: "Let's start." });
  await runButton(api, b, { chatId: 'c-old', select: (id) => selected.push(id) });
  assert.deepEqual(
    calls.map((c) => c.op),
    ['project_list', 'chat_create', 'chat_send'],
  );
  assert.equal(calls[1]!.args.cwd, 'C:/work/site');
  assert.equal(calls[2]!.args.chatId, 'new-chat');
  assert.equal(calls[2]!.args.text, "Let's start.");
  assert.deepEqual(selected, ['new-chat']);
});

test('a bound project that has no folder never creates a chat', async () => {
  const { api, calls } = fakeApi({ project_list: () => [{ id: 'p1', path: null }] });
  await assert.rejects(runButton(api, button({ kind: 'new_chat', projectId: 'p1' }), { chatId: null, select: () => undefined }), /gone or has no folder/);
  assert.deepEqual(calls.map((c) => c.op), ['project_list']);
});

test('sendActionText and the explain prompt', async () => {
  const { api, calls } = fakeApi({ chat_send: () => ({}) });
  await sendActionText(api, 'c1', EXPLAIN_PLAINLY_TEXT);
  assert.equal(calls[0]!.args.text, 'Please explain your last reply in plain language, avoiding jargon, and say what decision you need from me.');
});
