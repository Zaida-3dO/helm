import assert from 'node:assert/strict';
import { test } from 'node:test';
import { sideChatAnchor } from './anchor.ts';

const rows = [
  { id: 1, role: 'user', status: 'complete' },
  { id: 2, role: 'toolResult', status: 'complete' },
  { id: 3, role: 'assistant', status: 'complete' },
  { id: 4, role: 'user', status: 'complete' },
  { id: 5, role: 'assistant', status: 'streaming' },
];

test('a side chat anchors to the selected reply, or to the reply of the selected turn', () => {
  assert.equal(sideChatAnchor(rows, [3]), 3);
  assert.equal(sideChatAnchor(rows, [1]), 3, "a user message anchors to its turn's reply");
  assert.equal(sideChatAnchor(rows, [1, 3]), 3, 'across messages: the last one');
  assert.equal(sideChatAnchor(rows, [4]), null, 'no finished reply in that turn yet');
  assert.equal(sideChatAnchor(rows, []), null);
});
