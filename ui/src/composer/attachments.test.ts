import assert from 'node:assert/strict';
import { test } from 'node:test';
import { MAX_IMAGE_BYTES, refusal, toBase64, type Attached } from './attachments.ts';

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
