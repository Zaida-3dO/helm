import assert from 'node:assert/strict';
import { test } from 'node:test';
import { askForNames, suggestionOptions, suggestionsVisible } from './titleOptions.ts';

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

test('asking for names always comes back: an answer, the host reason, or a timeout note', async () => {
  const never = () => new Promise<unknown>(() => {});
  const rows: Array<[string, () => Promise<unknown>, number, { options: string[]; note: string }]> = [
    ['answer', async () => ({ titles: ['A', 'B'] }), 50, { options: ['A', 'B'], note: '' }],
    ['not from the model itself', async () => ({ titles: ['A'], message: 'No ideas from the model (timed out).' }), 50, { options: ['A'], note: 'No ideas from the model (timed out).' }],
    ['host refusal', async () => Promise.reject(new Error('No suggestion: the model took too long.')), 50, { options: [], note: 'No suggestion: the model took too long.' }],
    ['empty answer', async () => ({ titles: [] }), 50, { options: [], note: 'No names came back. Type one in.' }],
    ['host never answers', never, 20, { options: [], note: 'No names came back in time. Type one in.' }],
  ];
  for (const [label, ask, wait, want] of rows) assert.deepEqual(await askForNames(ask, 'New chat', wait), want, label);
});
