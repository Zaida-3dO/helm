import assert from 'node:assert/strict';
import { test } from 'node:test';
import { eyebrowOf, optionAnswer, pickIndex } from './interactionModel.ts';

test('1-9 pick an option that exists; other keys pick nothing', () => {
  assert.equal(pickIndex('1', 3), 0);
  assert.equal(pickIndex('3', 3), 2);
  assert.equal(pickIndex('4', 3), null, 'past the options');
  assert.equal(pickIndex('0', 3), null);
  assert.equal(pickIndex('a', 3), null);
  assert.equal(pickIndex('Enter', 3), null);
});

test('a plan answer confirms only Approve plan; other kinds send the value', () => {
  assert.deepEqual(optionAnswer('plan', 'Approve plan'), { value: 'Approve plan', confirmed: true });
  assert.deepEqual(optionAnswer('plan', 'Keep planning'), { value: 'Keep planning', confirmed: false });
  assert.deepEqual(optionAnswer('approval', 'Allow once'), { value: 'Allow once' });
  assert.deepEqual(optionAnswer('question', 'Red'), { value: 'Red' });
  assert.equal(eyebrowOf('question'), 'Question');
  assert.equal(eyebrowOf('plan'), 'Plan');
  assert.equal(eyebrowOf('approval'), 'Approval');
});
