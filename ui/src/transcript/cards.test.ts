import { test } from 'node:test';
import assert from 'node:assert/strict';
import { boardRef, cardKind, chosenText, galleryRef, optionForKey, optionMessage } from './cards.ts';
import { stepImage } from './messageImages.ts';
import { swipeStep, wheelStep, WHEEL_COOLDOWN_MS } from './swipe.ts';
import { row } from '../dev/fixtures.ts';

const tool = (name: string, text: string, extra: Partial<Parameters<typeof row>[0]> = {}) => row({ role: 'toolResult', toolName: `mcp__helm__${name}`, text, ...extra });

test('a card exists only for a finished, well-formed call of its tool', () => {
  const gallery = tool('app_show_images', '{"galleryId":"g1","count":3}');
  const board = tool('app_offer_options', '{"boardId":"b1","next":"wait"}');
  assert.deepEqual(galleryRef(gallery), { galleryId: 'g1', count: 3 });
  assert.deepEqual(boardRef(board), { boardId: 'b1' });
  assert.deepEqual([cardKind(gallery), cardKind(board)], ['gallery', 'board']);
  const plain = [
    tool('app_show_images', '{"galleryId":"g1","count":3}', { status: 'streaming' }),
    tool('app_show_images', 'Refused: no pictures', { status: 'error' }),
    tool('app_show_images', '{"count":3}'),
    tool('app_offer_options', 'not json'),
    tool('app_list_chats', '{"galleryId":"g1"}'),
    row({ role: 'toolResult', toolName: 'Bash', text: '{"galleryId":"g1"}' }),
    row({ role: 'assistant', text: '1. Red\n2. Blue' }),
  ];
  assert.deepEqual(plain.map(cardKind), plain.map(() => null));
});

test('a choice goes out as "<question>: <choice>", with a picked tile\'s detail, and number keys pick tiles', () => {
  const board = {
    question: ' Which colour? ',
    options: [
      { index: 0, label: 'Red', detail: 'warm', hasImage: false },
      { index: 1, label: 'Blue', detail: null, hasImage: true },
    ],
  };
  assert.equal(optionMessage(board, ['Red']), 'Which colour?: Red (warm)');
  assert.equal(optionMessage(board, ['Blue', 'Red']), 'Which colour?: Blue, Red (warm)');
  assert.equal(optionMessage(board, [], ' teal, please '), 'Which colour?: teal, please');
  assert.equal(chosenText({ labels: ['Red', 'Blue'], other: null, answeredAt: 1 }), 'Red, Blue');
  assert.equal(chosenText({ labels: [], other: 'teal', answeredAt: 1 }), 'Other: teal');
  assert.deepEqual(['1', '2', '3', '0', 'a', '10'].map((k) => optionForKey(k, 2)), [0, 1, null, null, null, null]);
});

test('a drag or a touchpad scroll steps once, and mostly-vertical movement does not', () => {
  assert.deepEqual([-80, 80, -30, 80, -80].map((dx, i) => swipeStep(dx, [0, 10, 0, 0, 90][i]!)), [1, -1, 0, -1, 0]);
  assert.equal(wheelStep(60, 4, 1000, -Infinity), 1);
  assert.equal(wheelStep(-60, 4, 1000, -Infinity), -1);
  assert.equal(wheelStep(8, 0, 1000, -Infinity), 0);
  assert.equal(wheelStep(4, 60, 1000, -Infinity), 0);
  // One flick is one step: the rest of its scroll events arrive inside the cool-down.
  assert.equal(wheelStep(60, 0, 1000 + WHEEL_COOLDOWN_MS - 1, 1000), 0);
  assert.equal(wheelStep(60, 0, 1000 + WHEEL_COOLDOWN_MS, 1000), 1);
  assert.deepEqual([stepImage(0, 3, -1), stepImage(2, 3, 1)], [2, 0]);
});
