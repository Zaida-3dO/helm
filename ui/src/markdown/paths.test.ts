import assert from 'node:assert/strict';
import { test } from 'node:test';
import { driveForm, isRemoteOrDevicePath } from './paths.ts';

// String.raw keeps every backslash exactly as Windows would see it.
const r = String.raw;

test('isRemoteOrDevicePath flags shares and device namespaces in every spelling', () => {
  for (const p of [
    r`\\srv\share`,
    '//srv/share',
    r`\/srv`,
    r`/\srv`,
    r`\\?\C:\x`,
    r`\\.\COM1`,
    r`\??\C:\x`,
    'file://srv/share',
    'FILE://Srv/share',
    'file:////srv/share',
    'file:///%5c%5csrv/share',
    '%2F%2Fsrv%2Fshare',
    ' \t' + r`\\srv\share`,
    '\\\u0000\\srv\\share',
  ]) {
    assert.equal(isRemoteOrDevicePath(p), true, JSON.stringify(p));
  }
});

test('isRemoteOrDevicePath leaves local paths alone', () => {
  for (const p of [r`C:\x\y.txt`, 'C:/x', '/home/x', 'notes/a.md', 'file:///C:/x', 'file://localhost/C:/x', 'file://C:/x', '']) {
    assert.equal(isRemoteOrDevicePath(p), false, JSON.stringify(p));
  }
});

test('driveForm tells an absolute drive path from a drive-relative one', () => {
  assert.equal(driveForm(r`C:\x`), 'absolute');
  assert.equal(driveForm('c:/x'), 'absolute');
  assert.equal(driveForm('C:x'), 'relative');
  assert.equal(driveForm('C:javascript:alert(1)'), 'relative');
  assert.equal(driveForm('https://x'), null);
  assert.equal(driveForm('/x'), null);
});
