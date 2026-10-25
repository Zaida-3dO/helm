import { test } from 'node:test';
import assert from 'node:assert/strict';
import { matchesQuery, searchTerms } from './search.ts';

test('search words must all appear, in any order, ignoring case', () => {
  const rows: [string, string, boolean][] = [
    ['Send message with Ctrl+Enter sends and Enter starts a new line', 'send', true],
    ['Interface text size The font size of menus', 'FONT', true],
    ['Interface text size The font size of menus', 'size font', true],
    ['Interface text size The font size of menus', 'font colour', false],
    ['Zoom Make everything larger', '', true],
    ['Zoom Make everything larger', '   ', true],
    ['Compact sidebar', 'send', false],
  ];
  for (const [text, q, want] of rows) assert.equal(matchesQuery(text, q), want, `${q} in ${text}`);
  assert.deepEqual(searchTerms('  Send  Key '), ['send', 'key']);
});
