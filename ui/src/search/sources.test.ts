import assert from 'node:assert/strict';
import { test } from 'node:test';
import type { Api } from '../api/client.ts';
import { chatsSource, filesSource } from './defaultSources.ts';
import { absolutePath, baseName, dirName, groupOrder, looksLikePath, rowId } from './sources.ts';

test('a query looks like a path when it has a separator, a ~ or drive start, or is one name.ext word', () => {
  for (const q of ['src/main.ts', 'a\\b', '~/notes', 'C:', 'c:/x', 'README.md', 'main.rs', './x']) assert.equal(looksLikePath(q), true, q);
  for (const q of ['fix login', 'login', 'v1.2 notes', 'a.b.c d', '', '   ', 'what is this.']) assert.equal(looksLikePath(q), false, q);
});

test('groups are ordered Chats, Files, Pull requests, Board; Files leads for a path', () => {
  const all = ['chats', 'files', 'prs', 'board'] as const;
  assert.deepEqual(groupOrder('login', all), ['chats', 'files', 'prs', 'board']);
  assert.deepEqual(groupOrder('src/a.ts', all), ['files', 'chats', 'prs', 'board']);
  assert.deepEqual(groupOrder('login', ['files', 'chats']), ['chats', 'files']);
});

test('row ids have no whitespace and carry their source', () => {
  assert.equal(rowId('files', 'C:/my docs/a b.txt'), 'files:C:/my_docs/a_b.txt');
  assert.ok(!/\s/.test(rowId('chats', 'x y\tz')));
});

test('paths are made absolute against the chat folder, keeping absolute ones', () => {
  assert.equal(absolutePath('C:/work/app', 'src/a.ts'), 'C:/work/app/src/a.ts');
  assert.equal(absolutePath('C:\\work\\app\\', 'src\\a.ts'), 'C:/work/app/src/a.ts');
  assert.equal(absolutePath('C:/work/app', 'D:/other/a.ts'), 'D:/other/a.ts');
  assert.equal(absolutePath('C:/work/app', '~/a.ts'), '~/a.ts');
  assert.equal(baseName('src/deep/a.ts'), 'a.ts');
  assert.equal(dirName('src/deep/a.ts'), 'src/deep');
  assert.equal(dirName('a.ts'), '');
});

function fakeApi(handlers: Record<string, (args: Record<string, unknown>) => unknown>): { api: Api; calls: { op: string; args: Record<string, unknown> }[] } {
  const calls: { op: string; args: Record<string, unknown> }[] = [];
  const api = (async (op: string, args: unknown) => {
    const a = (args ?? {}) as Record<string, unknown>;
    calls.push({ op, args: a });
    return handlers[op]!(a);
  }) as Api;
  return { api, calls };
}

test('the chats source passes the filters and the current project, and labels title-only rows', async () => {
  const opened: string[] = [];
  const { api, calls } = fakeApi({
    chat_search: () => ({
      hits: [
        { chatId: 'c1', title: 'Plans', project: 'app', messageId: '7', snippet: 'a plan', titleOnly: false },
        { chatId: 'c2', title: '', project: 'app', messageId: null, snippet: '', titleOnly: true },
      ],
    }),
  });
  const src = chatsSource({
    api,
    filters: () => ({ projectId: 'p9', archived: true }),
    currentProjectId: () => 'p1',
    open: (h) => opened.push(`${h.chatId}:${h.messageId}`),
  });
  const rows = await src.run('plan', new AbortController().signal);
  assert.deepEqual(calls[0]!.args, { query: 'plan', projectId: 'p9', archived: true, currentProjectId: 'p1' });
  assert.deepEqual(rows.map((r) => [r.id, r.title, r.tag]), [
    ['chats:c1:7', 'Plans', undefined],
    ['chats:c2:title', 'Untitled chat', 'title'],
  ]);
  rows[0]!.open();
  assert.deepEqual(opened, ['c1:7']);
});

test('the files source searches the chat folder through quick_open and opens an absolute path', async () => {
  const opened: string[] = [];
  const { api, calls } = fakeApi({ quick_open: () => [{ path: 'src/main.ts', score: 3 }, { path: 'README.md' }] });
  const src = filesSource({ api, chatId: () => 'c1', cwd: () => 'C:/work/app', openFile: (p) => opened.push(p) });
  const rows = await src.run('ma', new AbortController().signal);
  assert.deepEqual(calls[0], { op: 'quick_open', args: { chatId: 'c1', query: 'ma' } });
  assert.deepEqual(rows.map((r) => [r.title, r.subtitle]), [['main.ts', 'src'], ['README.md', undefined]]);
  rows[0]!.open();
  assert.deepEqual(opened, ['C:/work/app/src/main.ts']);
});

test('the files source gives no rows, and asks nothing, when no chat is open', async () => {
  const { api, calls } = fakeApi({});
  const src = filesSource({ api, chatId: () => null, cwd: () => null, openFile: () => undefined });
  assert.deepEqual(await src.run('x', new AbortController().signal), []);
  assert.equal(calls.length, 0);
});
