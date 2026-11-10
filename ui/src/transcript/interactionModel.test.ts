import assert from 'node:assert/strict';
import { test } from 'node:test';
import { defaultIndex, eyebrowOf, moveIndex, optionAnswer, pickIndex, splitRecommended } from './interactionModel.ts';

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

test('a recommended tag is split off the label, and only a question focuses a default option', () => {
  assert.deepEqual(splitRecommended('Blue (Recommended)'), { label: 'Blue', recommended: true });
  assert.deepEqual(splitRecommended('Blue - default'), { label: 'Blue', recommended: true });
  assert.deepEqual(splitRecommended('Keep the default settings'), { label: 'Keep the default settings', recommended: false });
  assert.equal(defaultIndex('question', ['Red', 'Blue (Recommended)']), 1);
  assert.equal(defaultIndex('question', ['Red', 'Blue']), 0, "the question tool's first option is its default");
  // A permission or a plan never takes focus, so a stray Enter cannot allow or approve.
  assert.equal(defaultIndex('approval', ['Allow once (Recommended)', 'Deny']), null);
  assert.equal(defaultIndex('plan', ['Approve plan', 'Keep planning']), null);
  assert.equal(defaultIndex('question', []), null);
});

test('arrow keys, Home and End walk the options and wrap; other keys move nothing', () => {
  for (const [key, from, count, to] of [
    ['ArrowDown', -1, 3, 0], ['ArrowDown', 0, 3, 1], ['ArrowDown', 2, 3, 0], ['ArrowRight', 1, 3, 2],
    ['ArrowUp', -1, 3, 2], ['ArrowUp', 0, 3, 2], ['ArrowUp', 2, 3, 1], ['ArrowLeft', 1, 3, 0],
    ['Home', 2, 3, 0], ['End', 0, 3, 2], ['Enter', 0, 3, null], ['a', 0, 3, null], ['ArrowDown', 0, 0, null],
  ] as const) {
    assert.equal(moveIndex(key, from, count), to, `${key} from ${from} of ${count}`);
  }
});

