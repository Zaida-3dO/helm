import assert from 'node:assert/strict';
import { test } from 'node:test';
import { pathInCode, pathInProse, splitPathSuffix } from './pathrefs.ts';

const r = String.raw;

test('a drive colon is part of the path; only a trailing suffix is a line', () => {
  assert.deepEqual(splitPathSuffix(r`C:\src\a.ts`), { path: r`C:\src\a.ts` });
  assert.deepEqual(splitPathSuffix('C:/src/a.ts:12'), { path: 'C:/src/a.ts', line: 12 });
  assert.deepEqual(splitPathSuffix('src/a.ts:12:4'), { path: 'src/a.ts', line: 12, column: 4 });
  assert.deepEqual(splitPathSuffix('src/a.ts:10-20'), { path: 'src/a.ts', line: 10, endLine: 20 });
  assert.deepEqual(splitPathSuffix('src/a.ts#L10'), { path: 'src/a.ts', line: 10 });
  assert.deepEqual(splitPathSuffix('src/a.ts#L10-L20'), { path: 'src/a.ts', line: 10, endLine: 20 });
  assert.deepEqual(splitPathSuffix('C:12'), { path: 'C:12' }, 'a drive and a number is not a line');
  assert.deepEqual(splitPathSuffix('a.ts:20-10'), { path: 'a.ts:20-10' }, 'a backwards range is not a range');
  assert.deepEqual(splitPathSuffix('a.ts:0'), { path: 'a.ts:0' });
});

test('inline code: whole tokens that look like paths', () => {
  for (const [code, path] of [
    ['src/app.ts', 'src/app.ts'],
    ['./src', './src'],
    ['../x/y.rs', '../x/y.rs'],
    ['~/notes/a.md', '~/notes/a.md'],
    ['~', '~'],
    [r`C:\Program Files\App\x.json`, r`C:\Program Files\App\x.json`],
    ['C:/x/y.ts:42', 'C:/x/y.ts'],
    ['/c/work/x', '/c/work/x'],
    ['package.json', 'package.json'],
    ['app.ts:12', 'app.ts'],
    ['Dockerfile', 'Dockerfile'],
  ] as const) {
    assert.equal(pathInCode(code)?.path, path, code);
  }
  for (const code of ['/api/health', '/admin/products/x', 'npm test', 'const x = 1', 'https://a.example/b.ts', 'www.x.com/a.js', '1.2.3', '2026/10/17', '1/2', 'foo', 'a|b.ts', 'C:', 'repo@main:src/a.ts', 'mailto:a@b.co', 'x\ny/z.ts']) {
    assert.equal(pathInCode(code), null, code);
  }
});

test('prose: only the unmistakable forms', () => {
  for (const [word, path] of [
    ['~/notes/a.md', '~/notes/a.md'],
    ['./build.sh', './build.sh'],
    ['../README.md', '../README.md'],
    [r`C:\work\x\a.txt`, r`C:\work\x\a.txt`],
    ['/c/work/x', '/c/work/x'],
    ['src/components/Button.tsx:12', 'src/components/Button.tsx'],
  ] as const) {
    assert.equal(pathInProse(word)?.path, path, word);
  }
  for (const word of ['/api/health', '/admin/products/x', 'and/or', '1/2', '2026/10/17', 'v1.2.3', 'package.json', 'src/app', r`C:\x`, 'https://a.example/x.ts', '/help', 'TCP/IP', 'client/server']) {
    assert.equal(pathInProse(word), null, word);
  }
});
