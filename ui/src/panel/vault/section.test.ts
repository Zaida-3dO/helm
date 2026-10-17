import assert from 'node:assert/strict';
import { test } from 'node:test';
import { blockSection, embedText, headingSection } from './section.ts';

const note = ['# Title', 'intro', '## Plan', 'step one', '### Detail', 'deep', '## Next', 'later', '```', '## not a heading', '```'].join('\n');

test('a heading section runs to the next heading of the same or a higher level', () => {
  assert.equal(headingSection(note, 'plan'), ['## Plan', 'step one', '### Detail', 'deep'].join('\n'));
  assert.equal(headingSection(note, 'Detail'), ['### Detail', 'deep'].join('\n'));
  assert.equal(headingSection(note, 'Missing'), null);
  assert.equal(headingSection(note, 'not a heading'), null, 'headings inside code fences do not count');
});

test('a block reference picks its paragraph and drops the marker', () => {
  const t = 'first para\n\nthe one ^abc\n\nlast';
  assert.equal(blockSection(t, 'abc'), 'the one');
  assert.equal(blockSection(t, 'zzz'), null);
});

test('an embed falls back to the whole note when the section is missing', () => {
  assert.equal(embedText(note, 'Missing'), note);
  assert.equal(embedText('a ^b', undefined, 'b'), 'a');
});
