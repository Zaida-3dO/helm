import assert from 'node:assert/strict';
import { test } from 'node:test';
import { applyPanelOpen } from './push.ts';

interface Call {
  kind?: string;
  key?: string;
  data?: unknown;
  opts?: unknown;
  args: unknown[];
}

function setup(current: string | null, room = true) {
  const tabs: Call[] = [];
  const pages: Call[] = [];
  const modals: Call[] = [];
  const notices: string[] = [];
  const deps = {
    openTab: (kind: string, key: string, data: unknown, opts: { intent: 'user'; chatId: string }) => {
      tabs.push({ kind, key, data, opts, args: [kind, key, data, opts] });
      return room;
    },
    openPage: (path: string, chatId: string) => {
      pages.push({ args: [path, chatId] });
      return room;
    },
    openModal: (path: string, opts: { line?: number; chatId: string }) => {
      modals.push({ args: [path, opts] });
    },
    notice: (text: string) => notices.push(text),
    current: () => current,
  };
  return { deps, tabs, pages, modals, notices };
}

test("a background chat's push lands in its own set and changes nothing on screen", () => {
  const s = setup('c1');
  assert.equal(applyPanelOpen({ chatId: 'c2', doc: 'file', path: 'C:\\w\\app\\main.rs', line: 12 }, s.deps), true);
  assert.equal(s.tabs.length, 1);
  assert.equal(s.tabs[0]!.kind, 'file');
  assert.equal((s.tabs[0]!.opts as { chatId: string }).chatId, 'c2');
  assert.deepEqual(s.tabs[0]!.data, { path: 'C:\\w\\app\\main.rs', line: 12 });
  assert.equal(s.modals.length, 0, 'a refused fallback never fires for a chat that fits');
  assert.equal(s.notices.length, 0);
});

test('a page push opens in the pushing chat; a refused page push speaks up only when that chat is on screen', () => {
  const bg = setup('c1');
  assert.equal(applyPanelOpen({ chatId: 'c2', doc: 'page', path: 'C:\\w\\plan.html', pageId: 'p-1' }, bg.deps), true);
  assert.deepEqual(bg.pages[0]!.args, ['C:\\w\\plan.html', 'c2']);
  assert.equal(bg.notices.length, 0);

  const shown = setup('c1', false);
  assert.equal(applyPanelOpen({ chatId: 'c1', doc: 'page', path: 'C:\\w\\plan.html' }, shown.deps), false);
  assert.equal(shown.notices.length, 1, 'no room on the shown chat: a notice');

  const away = setup('c1', false);
  assert.equal(applyPanelOpen({ chatId: 'c2', doc: 'page', path: 'C:\\w\\plan.html' }, away.deps), false);
  assert.equal(away.notices.length, 0, 'no room for a background chat: silence, its card keeps Open');
});

test('a refused file open falls back to the modal only for the chat on screen', () => {
  const shown = setup('c1', false);
  assert.equal(applyPanelOpen({ chatId: 'c1', doc: 'file', path: 'C:\\w\\app\\a.ts', line: 3 }, shown.deps), false);
  assert.equal(shown.modals.length, 1);
  assert.deepEqual(shown.modals[0]!.args[1], { line: 3, chatId: 'c1' });

  const away = setup('c1', false);
  assert.equal(applyPanelOpen({ chatId: 'c2', doc: 'file', path: 'C:\\w\\app\\a.ts' }, away.deps), false);
  assert.equal(away.modals.length, 0);
});

test('a malformed or unknown event opens nothing and calls nothing', () => {
  for (const e of [
    {},
    { doc: 'file', path: 'C:\\w\\a.txt' },
    { chatId: '', doc: 'file', path: 'C:\\w\\a.txt' },
    { chatId: 'c2', doc: 'file' },
    { chatId: 'c2', doc: 'page' },
    { chatId: 'c2', doc: 'other', path: 'C:\\w\\a.txt' },
  ]) {
    const s = setup('c1');
    assert.equal(applyPanelOpen(e, s.deps), false, JSON.stringify(e));
    assert.equal(s.tabs.length + s.pages.length + s.modals.length + s.notices.length, 0, JSON.stringify(e));
  }
});

test('a non-positive or fractional line opens the file at its top', () => {
  const s = setup('c1');
  applyPanelOpen({ chatId: 'c2', doc: 'file', path: 'C:\\w\\a.txt', line: 0.5 }, s.deps);
  assert.deepEqual(s.tabs[0]!.data, { path: 'C:\\w\\a.txt' });
});
