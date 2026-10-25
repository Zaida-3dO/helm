import assert from 'node:assert/strict';
import { test } from 'node:test';
import { MAX_IMAGE_BYTES, forgetDelivered, isAnimated, refusal, stashSent, takeSent, toBase64, type Attached } from './attachments.ts';
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

test('what a send carried is given back once, and only the latest few sends are kept', () => {
  const file = new Blob(['x']);
  stashSent('s0', { files: [], refs: [] });
  assert.equal(takeSent('s0'), null, 'nothing to remember for a text-only send');
  stashSent('s1', { files: [file], refs: [{ kind: 'path', target: 'C:/a.txt' }] });
  assert.deepEqual(takeSent('s1')?.refs, [{ kind: 'path', target: 'C:/a.txt' }]);
  assert.equal(takeSent('s1'), null, 'taken once');
  for (let i = 0; i < 12; i++) stashSent(`n${i}`, { files: [file], refs: [] });
  assert.equal(takeSent('n0'), null, 'older sends are forgotten');
  assert.ok(takeSent('n11'));
});

test('delivered sends are let go once the grace period passes, and only a missing component blocks sending', () => {
  const f = new Blob(['x']);
  stashSent('d1', { files: [f], refs: [] }, 1000);
  stashSent('d2', { files: [f], refs: [] }, 1000);
  forgetDelivered(['d1', 'd2'], new Set(['d2']), 2000);
  assert.ok(takeSent('d2') && true, 'still waiting: kept');
  stashSent('d3', { files: [f], refs: [] }, 1000);
  forgetDelivered(['d3'], new Set(), 1500);
  assert.ok(takeSent('d3'), 'inside the grace period: kept');
  stashSent('d4', { files: [f], refs: [] }, 1000);
  forgetDelivered(['d4'], new Set(), 9000);
  assert.equal(takeSent('d4'), null, 'delivered: forgotten');
  const found = { path: 'x', source: 'PATH', version: null, problem: null };
  const missing = { path: null, source: null, version: null, problem: 'not found' };
  assert.equal(isBlocked({ node: found, claude: { ...found, problem: 'did not answer --version.' } }), false);
  assert.equal(isBlocked({ node: found, claude: missing }), true);
  assert.equal(isBlocked({ node: missing, claude: found }), true);
});
