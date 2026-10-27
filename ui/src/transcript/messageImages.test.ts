import assert from 'node:assert/strict';
import { test } from 'node:test';
import { THUMB_MAX_HEIGHT, THUMB_MAX_WIDTH, THUMB_MIN_SIDE, UrlCache, bytesFromBase64, fitImage, imageFacts, imageLabel, stepImage, thumbSize } from './messageImages.ts';

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

test('a kept thumbnail URL is reused while in use or recently released, and revoked when pushed out', () => {
  const revoked: string[] = [];
  const cache = new UrlCache(2, (u) => revoked.push(u));
  assert.equal(cache.acquire('a'), undefined);
  assert.equal(cache.put('a', 'blob:a'), 'blob:a');
  cache.put('b', 'blob:b');
  cache.put('c', 'blob:c');
  assert.deepEqual(revoked, [], 'all three are in use');
  cache.release('a');
  cache.release('b');
  assert.deepEqual(revoked, [], 'two idle entries fit');
  cache.release('c');
  assert.deepEqual(revoked, ['blob:a'], 'the oldest idle one goes when a third is idle');
  assert.equal(cache.acquire('a'), undefined);
  assert.equal(cache.acquire('b'), 'blob:b', 'a released one is found again');
  cache.release('b');
  cache.release('b');
  assert.equal(cache.size, 2);
});

test('a URL that is in use is never revoked by the limit', () => {
  const revoked: string[] = [];
  const cache = new UrlCache(0, (u) => revoked.push(u));
  cache.put('a', 'blob:a');
  cache.put('b', 'blob:b');
  assert.deepEqual(revoked, []);
  cache.release('a');
  assert.deepEqual(revoked, ['blob:a']);
  assert.equal(cache.acquire('b'), 'blob:b');
});

test('two users fetching the same image share the first URL and each release counts once', () => {
  const revoked: string[] = [];
  const cache = new UrlCache(0, (u) => revoked.push(u));
  assert.equal(cache.put('a', 'blob:first'), 'blob:first');
  assert.equal(cache.put('a', 'blob:second'), 'blob:first', 'the loser is told to use the first and revoke its own');
  assert.equal(cache.size, 1);
  cache.release('a');
  assert.deepEqual(revoked, [], 'the other user still shows it');
  assert.equal(cache.peek('a'), 'blob:first');
  cache.release('a');
  assert.deepEqual(revoked, ['blob:first']);
  assert.equal(cache.peek('a'), undefined);
});

test('the image viewer fits a picture to the window, steps through a set and states its size', () => {
  // Fitted at its own aspect ratio, never scaled up.
  for (const [w, h, maxW, maxH, want] of [
    [800, 600, 1200, 900, { width: 800, height: 600 }],
    [4000, 1000, 1000, 800, { width: 1000, height: 250 }],
    [1000, 4000, 1000, 800, { width: 200, height: 800 }],
    [3000, 2000, 900, 900, { width: 900, height: 600 }],
  ] as const) {
    assert.deepEqual(fitImage(w, h, maxW, maxH), want, `${w}x${h} in ${maxW}x${maxH}`);
  }
  // The arrow keys wrap at either end; a single image stays put.
  for (const [index, count, delta, want] of [
    [0, 3, 1, 1],
    [2, 3, 1, 0],
    [0, 3, -1, 2],
    [0, 1, 1, 0],
    [0, 1, -1, 0],
  ] as const) {
    assert.equal(stepImage(index, count, delta), want, `${index} of ${count} by ${delta}`);
  }
  // Size and pixel dimensions, whichever are known.
  for (const [facts, want] of [
    [{ size: 812, width: 0, height: 0 }, '812 bytes'],
    [{ size: 48 * 1024 + 100, width: 640, height: 480 }, '48 KB · 640 × 480 px'],
    [{ size: 2.4 * 1024 * 1024 }, '2.4 MB'],
    [{ width: 1920, height: 1080 }, '1920 × 1080 px'],
    [{}, ''],
  ] as const) {
    assert.equal(imageFacts(facts), want);
  }
});
