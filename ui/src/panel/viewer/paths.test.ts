import assert from 'node:assert/strict';
import { test } from 'node:test';
import { basename, dirname, displayPath, joinPath, pathKey, splitSegments } from './paths.ts';

const r = String.raw;
const ROOT = 'C:\\';

test('displayPath and pathKey normalise slashes, drive case and trailing separators', () => {
  assert.equal(displayPath('c:/x/y/'), r`C:\x\y`);
  assert.equal(displayPath('C:/'), ROOT);
  assert.equal(pathKey(r`C:\X\Y.ts`), pathKey('c:/x/y.ts'));
});

test('dirname stops at the drive root', () => {
  assert.equal(dirname(r`C:\a\b.txt`), r`C:\a`);
  assert.equal(dirname(r`C:\a`), ROOT);
  assert.equal(dirname(ROOT), null);
  assert.equal(basename(r`C:\a\b.txt`), 'b.txt');
});

test('joinPath resolves . and .. without climbing above the drive', () => {
  assert.equal(joinPath(r`C:\a\b`, '../c/d.md'), r`C:\a\c\d.md`);
  assert.equal(joinPath(r`C:\a`, './x'), r`C:\a\x`);
  assert.equal(joinPath(r`C:\a`, '../../../x'), r`C:\x`);
  assert.equal(joinPath(ROOT, 'x'), r`C:\x`);
});

test('splitSegments gives each breadcrumb its own path', () => {
  assert.deepEqual(splitSegments('C:/a/b'), [
    { name: 'C:', path: ROOT },
    { name: 'a', path: r`C:\a` },
    { name: 'b', path: r`C:\a\b` },
  ]);
});
