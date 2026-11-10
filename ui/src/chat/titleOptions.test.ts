import assert from 'node:assert/strict';
import { test } from 'node:test';
import { suggestionOptions, suggestionsVisible } from './titleOptions.ts';

test('options: cleaned, no repeats, nothing equal to the opened title, three at most, anything odd gives none', () => {
  const rows: Array<[unknown, string, string[]]> = [
    [{ title: 'A', titles: ['A', ' B ', 'b', 'C', 'D', 'E'] }, 'New chat', ['A', 'B', 'C']],
    [{ title: 'Old', titles: ['old', 'Fresh'] }, 'Old', ['Fresh']],
    [{ title: 'Only one' }, 'x', ['Only one']],
    [{ titles: ['', '  ', 'bad\nline', 7, null, 'Ok'] }, 'x', ['Ok']],
    [{ titles: 'nope' }, 'x', []],
    [{}, 'x', []],
    [null, 'x', []],
    ['text', 'x', []],
  ];
  for (const [answer, opened, want] of rows) assert.deepEqual(suggestionOptions(answer, opened), want, JSON.stringify(answer));
});

test('suggestions show only while the field is as it was when the box opened', () => {
  assert.equal(suggestionsVisible('Fix the build', 'Fix the build'), true);
  assert.equal(suggestionsVisible('Fix the build!', 'Fix the build'), false);
  assert.equal(suggestionsVisible('', 'Fix the build'), false);
});
