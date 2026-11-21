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

test('every pane is searchable by its own words, whichever group it lives in', () => {
  // The pane title matches whole when it matches at all, and grouped panes are no different:
  // a pane moved under a new group header keeps the words a person would type for it.
  const rows: [string, string, boolean][] = [
    ['Gatekeeper', 'gate', true],
    ['Gatekeeper', 'keeper', true],
    ['Gatekeeper', 'gate keeper', true], // each word appears inside the word
    ['Gatekeeper', 'gatekeeper mode', false],
    ['Usage governor', 'governor', true],
  ];
  for (const [text, q, want] of rows) assert.equal(matchesQuery(text, q), want, `${q} in ${text}`);
});
