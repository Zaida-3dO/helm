import assert from 'node:assert/strict';
import { test } from 'node:test';
import { row } from '../dev/fixtures.ts';
import { chatToMarkdown, loadWholeChat } from './wholeChat.ts';

test('the whole chat reads as Markdown: your messages and the agent answers, nothing else', () => {
  const rows = [
    row({ id: 1, role: 'user', text: 'Fix it' }),
    row({ id: 2, role: 'assistant', thinking: 'hm', text: 'Looking.' }),
    row({ id: 3, role: 'toolResult', toolName: 'Read', text: 'file body' }),
    row({ id: 4, role: 'assistant', text: 'Done.' }),
    row({ id: 5, role: 'context', text: 'compacted' }),
    row({ id: 6, role: 'user', text: 'Thanks' }),
    row({ id: 7, role: 'assistant', text: '   ' }),
  ];
  assert.equal(chatToMarkdown(rows), '### You\n\nFix it\n\n### Assistant\n\nLooking.\n\nDone.\n\n### You\n\nThanks\n');
  assert.equal(chatToMarkdown([]), '');
});

test('every page is read, newest first, and returned oldest first', async () => {
  const pages: Record<string, { messages: ReturnType<typeof row>[]; hasOlder: boolean; olderCursor: string | null }> = {
    newest: { messages: [row({ id: 5, role: 'user' }), row({ id: 6, role: 'assistant' })], hasOlder: true, olderCursor: 'c1' },
    c1: { messages: [row({ id: 3, role: 'user' }), row({ id: 4, role: 'assistant' })], hasOlder: true, olderCursor: 'c2' },
    c2: { messages: [row({ id: 1, role: 'user' }), row({ id: 2, role: 'assistant' })], hasOlder: false, olderCursor: null },
  };
  const asked: string[] = [];
  const { rows: all, capped } = await loadWholeChat(async (before) => {
    asked.push(before ?? 'newest');
    return pages[before ?? 'newest']!;
  });
  assert.equal(capped, false);
  assert.deepEqual(asked, ['newest', 'c1', 'c2']);
  assert.deepEqual(all.map((r) => r.id), [1, 2, 3, 4, 5, 6]);
});

test('with a side chat open, one view takes Ctrl+A when focus is on the page; a long chat says it was cut', async () => {
  const { ownsPage } = await import('./useWholeChat.ts');
  const { copiedNotice, MAX_ROWS } = await import('./wholeChat.ts');
  const main = Symbol('main');
  const side = Symbol('side');
  // Neither clicked yet: the first opened (the main chat) owns it; after a click in the side chat, that one.
  assert.deepEqual([ownsPage(main, [main, side], null), ownsPage(side, [main, side], null)], [true, false]);
  assert.deepEqual([ownsPage(main, [main, side], side), ownsPage(side, [main, side], side)], [false, true]);
  // The view clicked last was closed: back to the first.
  assert.deepEqual([ownsPage(main, [main], side), ownsPage(side, [main], side)], [true, false]);
  assert.equal(copiedNotice(false), 'Copied the whole chat.');
  assert.match(copiedNotice(true), new RegExp(`newest ${MAX_ROWS.toLocaleString().replace(',', ',?')} rows`));
});
