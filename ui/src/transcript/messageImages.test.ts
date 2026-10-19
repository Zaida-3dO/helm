import assert from 'node:assert/strict';
import { test } from 'node:test';
import { THUMB_MAX_HEIGHT, THUMB_MAX_WIDTH, THUMB_MIN_SIDE, bytesFromBase64, imageLabel, thumbSize } from './messageImages.ts';

test('a small image is shown at its own size, never scaled up', () => {
  assert.deepEqual(thumbSize(100, 60), { width: 100, height: 60 });
});

test('a large image is scaled to fit the box and keeps its aspect ratio', () => {
  assert.deepEqual(thumbSize(1920, 1080), { width: THUMB_MAX_WIDTH, height: 135 });
  assert.deepEqual(thumbSize(1000, 2000), { width: 90, height: THUMB_MAX_HEIGHT });
});

test('a very thin image keeps a clickable minimum side', () => {
  const t = thumbSize(4000, 10);
  assert.equal(t.width, THUMB_MAX_WIDTH);
  assert.equal(t.height, THUMB_MIN_SIDE);
});

test('an unknown size falls back to a square box instead of NaN', () => {
  assert.deepEqual(thumbSize(0, 0), { width: THUMB_MAX_HEIGHT, height: THUMB_MAX_HEIGHT });
  assert.deepEqual(thumbSize(Number.NaN, 5), { width: THUMB_MAX_HEIGHT, height: THUMB_MAX_HEIGHT });
});

test('base64 decodes back to the original bytes', () => {
  assert.deepEqual([...bytesFromBase64('iVBORw0=')], [0x89, 0x50, 0x4e, 0x47, 0x0d]);
  assert.equal(bytesFromBase64('').length, 0);
});

test('the label names the file, and numbers a set of several', () => {
  assert.equal(imageLabel('shot.png', 0, 1), 'shot.png');
  assert.equal(imageLabel('shot.png', 1, 3), 'shot.png (image 2 of 3)');
  assert.equal(imageLabel('  ', 0, 1), 'image');
});
