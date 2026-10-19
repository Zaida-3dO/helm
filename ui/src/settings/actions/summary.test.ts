import assert from 'node:assert/strict';
import { test } from 'node:test';
import { MAX_PREVIEW_CHARS, nameOf, previewLine, sendsTo, showsIn } from './summary.ts';

test('shows in names the placement', () => {
  assert.equal(showsIn({ placement: 'topbar' }), 'Top bar');
  assert.equal(showsIn({ placement: 'overflow' }), 'More menu');
});

test('sends to says this chat, or the bound project of a new chat', () => {
  assert.equal(sendsTo({ kind: 'prompt', projectId: null }, null), 'This chat');
  assert.equal(sendsTo({ kind: 'new_chat', projectId: null }, null), 'New chat (no project yet)');
  assert.equal(sendsTo({ kind: 'new_chat', projectId: 'p1' }, 'Helm'), 'New chat in Helm');
  assert.equal(sendsTo({ kind: 'new_chat', projectId: 'p1' }, null), 'New chat in an unavailable project');
});

test('the preview is the first non-empty line, trimmed', () => {
  assert.equal(previewLine('\n  \n  Give me a status.  \nMore text'), 'Give me a status.');
  assert.equal(previewLine(''), '');
  assert.equal(previewLine('one\r\ntwo'), 'one');
});

test('a long line is cut to the limit with an ellipsis', () => {
  const out = previewLine('x'.repeat(MAX_PREVIEW_CHARS + 50));
  assert.equal([...out].length, MAX_PREVIEW_CHARS);
  assert.ok(out.endsWith('…'));
  assert.equal(previewLine('x'.repeat(MAX_PREVIEW_CHARS)), 'x'.repeat(MAX_PREVIEW_CHARS), 'at the limit is kept whole');
});

test('an empty label is spoken as a new button', () => {
  assert.equal(nameOf({ label: '  ' }), 'new button');
  assert.equal(nameOf({ label: ' Ship it ' }), 'Ship it');
});
