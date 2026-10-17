import { test } from 'node:test';
import assert from 'node:assert/strict';
import { PREVIEW_SOURCE, stripMarkdown, thinkingPreview } from './thinking.ts';

test('markdown is stripped', () => {
  assert.equal(stripMarkdown('## Plan\n- **read** the `file` [here](http://x)'), 'Plan read the file here');
});

test('the preview shows the latest sentence and only reads the tail', () => {
  assert.equal(thinkingPreview('First idea. Then the second thought.'), 'First idea. Then the second thought.');
  const long = 'Old sentence that must not appear. ' + 'x'.repeat(PREVIEW_SOURCE) + ' Latest one.';
  const p = thinkingPreview(long);
  assert.ok(!p.includes('Old sentence'));
  assert.ok(p.endsWith('Latest one.'));
  assert.ok(p.length <= 241);
});
