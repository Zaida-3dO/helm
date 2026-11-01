import assert from 'node:assert/strict';
import { test } from 'node:test';
import { MAX_IMAGE_BYTES, isAnimated, refusal, toBase64, unstagedOnly, type Attached } from './attachments.ts';
import { isBlocked } from './runtimeCheck.ts';

const att = (size: number): Attached => ({ id: 'a', name: 'a.png', size, url: 'blob:x' });

test('the composer applies the attachment limits before uploading', () => {
  assert.equal(refusal([], { name: 'a.png', type: 'image/png', size: 10 }), null);
  assert.match(refusal([], { name: 'a.txt', type: 'text/plain', size: 10 }) ?? '', /only PNG/);
  assert.match(refusal([], { name: 'b.png', type: 'image/png', size: MAX_IMAGE_BYTES + 1 }) ?? '', /25 MB/);
  assert.match(refusal(Array.from({ length: 20 }, () => att(1)), { name: 'c.png', type: 'image/png', size: 1 }) ?? '', /20 attachments/);
  assert.match(refusal([att(25 * 1024 * 1024), att(25 * 1024 * 1024)], { name: 'd.png', type: 'image/png', size: 11 * 1024 * 1024 }) ?? '', /60 MB/);
});

test('bytes are base64-encoded', async () => {
  assert.equal(await toBase64(new Blob([new Uint8Array([104, 105])])), 'aGk=');
});

test('a GIF is refused as an image (it goes as a file), and animated PNG and WebP are detected', () => {
  assert.match(refusal([], { name: 'a.gif', type: 'image/gif', size: 10 }) ?? '', /file reference/);
  const chunk = (type: string, len = 0) => [0, 0, 0, len, ...[...type].map((c) => c.charCodeAt(0)), ...new Array(len + 4).fill(0)];
  const png = (...types: string[]) => new Uint8Array([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, ...types.flatMap((x) => chunk(x))]);
  assert.equal(isAnimated('image/png', png('IHDR', 'IDAT', 'IEND')), false);
  assert.equal(isAnimated('image/png', png('IHDR', 'acTL', 'IDAT', 'IEND')), true);
  const webp = (flags: number) => new Uint8Array([...[...'RIFF'].map((c) => c.charCodeAt(0)), 0, 0, 0, 0, ...[...'WEBPVP8X'].map((c) => c.charCodeAt(0)), 10, 0, 0, 0, flags, 0, 0, 0]);
  assert.equal(isAnimated('image/webp', webp(0x10)), false);
  assert.equal(isAnimated('image/webp', webp(0x12)), true);
  assert.equal(isAnimated('image/jpeg', new Uint8Array([0xff, 0xd8])), false);
  // A corrupt length (unsigned 0xFFFFFFF4) ends the scan instead of stalling it.
  assert.equal(isAnimated('image/png', new Uint8Array([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0xff, 0xff, 0xff, 0xf4, 0x74, 0x45, 0x58, 0x74, 0, 0, 0, 0])), false);
});

test('the composer restages only the attachments the chat still holds unstaged', () => {
  const pool = [{ id: 'i1' }, { id: 'i2' }, { id: 'i3' }];
  assert.deepEqual(unstagedOnly(pool, [{ id: 'i2' }]).map((x) => x.id), ['i1', 'i3']);
  assert.deepEqual(unstagedOnly(pool, pool), [], 'everything already staged: nothing to take');
  assert.deepEqual(unstagedOnly([], [{ id: 'i1' }]), [], 'an empty pool stages nothing');
});

test('only a missing component blocks sending', () => {
  const found = { path: 'x', source: 'PATH', version: null, problem: null };
  const missing = { path: null, source: null, version: null, problem: 'not found' };
  assert.equal(isBlocked({ node: found, claude: { ...found, problem: 'did not answer --version.' } }), false);
  assert.equal(isBlocked({ node: found, claude: missing }), true);
  assert.equal(isBlocked({ node: missing, claude: found }), true);
});
