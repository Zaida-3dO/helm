import assert from 'node:assert/strict';
import { test } from 'node:test';
import type { FileView } from '../../api/panel.ts';
import { changedSince, sameContent, updatedLabel, watchedSlice } from './live.ts';
import { treeKey, treeRows } from './tree.ts';
import type { Listing } from './tree.ts';
import { archiveRows, delimiterFor, filterRows, jsonMatches, parseDelimited, parseJson, sizeLabel, stepZoom, ZOOM_MAX, ZOOM_MIN } from './preview.ts';

test('delimited text handles quotes, embedded delimiters and newlines, and either line ending', () => {
  const rows: Array<[string, string, string[][]]> = [
    ['a,b\n1,2\n', ',', [['a', 'b'], ['1', '2']]],
    ['a,"b,c",d\r\n"x ""q"" y",2,"line\nbreak"\r\n', ',', [['a', 'b,c', 'd'], ['x "q" y', '2', 'line\nbreak']]],
    ['a\tb\n1\t\n', '\t', [['a', 'b'], ['1', '']]],
    ['', ',', []],
    ['no newline at end', ',', [['no newline at end']]],
  ];
  for (const [text, delimiter, want] of rows) assert.deepEqual(parseDelimited(text, delimiter), want, text);
  assert.equal(delimiterFor('C:/x/Data.CSV'), ',');
  assert.equal(delimiterFor('a.tsv'), '\t');
  assert.equal(delimiterFor('a.txt'), null);
});

test('row search keeps the original row index and ignores case and padding', () => {
  const rows = [['ann', 'x'], ['Bob', 'y'], ['cat', 'BOB']];
  assert.deepEqual(filterRows(rows, ' bob ').map((r) => r.index), [1, 2]);
  assert.equal(filterRows(rows, '').length, 3);
  assert.equal(filterRows(rows, 'zzz').length, 0);
});

test('json search matches keys and values at any depth, and bad json is reported', () => {
  const doc = { name: 'helm', deps: { react: '19', list: [1, 'two'] } };
  const hits: Array<[string, unknown, string, boolean]> = [
    ['root', doc, 'react', true],
    ['root', doc, 'TWO', true],
    ['deps', doc.deps, 'helm', false],
    ['name', 'helm', 'hel', true],
    ['root', doc, '', true],
  ];
  for (const [k, v, q, want] of hits) assert.equal(jsonMatches(k, v, q), want, `${k} ${q}`);
  assert.deepEqual(parseJson('{"a":1}'), { ok: true, value: { a: 1 } });
  assert.deepEqual(parseJson('{oops'), { ok: false });
});

test('zoom steps stay within the allowed range', () => {
  assert.equal(stepZoom(1, 1), 1.25);
  assert.equal(stepZoom(1.25, -1), 1);
  assert.equal(stepZoom(ZOOM_MAX, 1), ZOOM_MAX);
  assert.equal(stepZoom(ZOOM_MIN, -1), ZOOM_MIN);
});

test('archive entries become an indented folder tree, with implied folders and folders first', () => {
  const e = (path: string, size = 1, dir = path.endsWith('/')) => ({ path, size, dir, encrypted: false });
  const rows = archiveRows([e('b.txt', 5), e('src/deep/x.ts', 9), e('docs/', 0), e('src/a.ts', 3), e('A.md', 2), e('./odd//name.txt', 4)]);
  assert.deepEqual(
    rows.map((r) => `${' '.repeat(r.depth)}${r.name}${r.dir ? '/' : ''}`),
    ['docs/', 'odd/', ' name.txt', 'src/', ' deep/', '  x.ts', ' a.ts', 'A.md', 'b.txt'],
  );
  assert.equal(rows.find((r) => r.name === 'x.ts')?.size, 9);
  assert.equal(rows.find((r) => r.name === 'deep')?.size, undefined);
  assert.deepEqual(archiveRows([]), []);
  assert.deepEqual([512, 2048, 5 * 1024 * 1024, 3 * 1024 ** 3].map(sizeLabel), ['512 B', '2.0 KiB', '5.0 MiB', '3.0 GiB']);
});

test('live following: only a different time is a change, and an identical re-read draws nothing', () => {
  const changed: Array<[number | undefined, { modified: number; size: number } | null, boolean]> = [
    [5, { modified: 6, size: 1 }, true],
    [5, { modified: 5, size: 99 }, false],
    [undefined, { modified: 6, size: 1 }, false],
    [5, null, false],
  ];
  for (const [before, now, want] of changed) assert.equal(changedSince(before, now), want, JSON.stringify([before, now]));
  const a: FileView = { path: String.raw`C:\a`, kind: 'text', size: 3, text: 'abc', truncated: false, url: 'x' };
  assert.equal(sameContent(a, { ...a }), true);
  assert.equal(sameContent(a, { ...a, url: 'y' }), false);
  assert.equal(sameContent(a, { ...a, text: 'abd' }), false);
  assert.equal(sameContent(null, a), false);
  assert.equal(updatedLabel(400), 'updated just now');
  assert.equal(updatedLabel(3200), 'updated 3s ago');
  assert.deepEqual(watchedSlice([1, 2, 3, 4, 5, 6, 7, 8, 9, 10]), [1, 2, 3, 4, 5, 6, 7, 8]);
});

test('the file tree lists open folders only, hides dot files, and keys move, open and close', () => {
  const ready = (names: Array<[string, 'dir' | 'file' | 'link']>, truncated = false): Listing => ({
    status: 'ready',
    entries: names.map(([name, kind]) => ({ name, kind })),
    truncated,
  });
  const r = String.raw;
  const root = r`C:\p`;
  const listings = new Map<string, Listing>([
    [r`c:\p`, ready([['src', 'dir'], ['docs', 'dir'], ['.git', 'dir'], ['a.md', 'file']])],
    [r`c:\p\src`, ready([['ui', 'dir'], ['b.ts', 'file']])],
    [r`c:\p\src\ui`, { status: 'loading' }],
  ]);
  const shape = (rows: ReturnType<typeof treeRows>) => rows.map((x) => (x.type === 'entry' ? `${x.depth}${x.open ? '-' : '+'}${x.name}` : `${x.depth}:${x.text}`));
  assert.deepEqual(shape(treeRows(root, new Set(), listings, false)), ['0+src', '0+docs', '0+a.md']);
  assert.equal(shape(treeRows(root, new Set(), listings, true)).includes('0+.git'), true);
  const open = treeRows(root, new Set([r`c:\p\src`, r`c:\p\src\ui`]), listings, false);
  assert.deepEqual(shape(open), ['0-src', '1-ui', '2:Loading…', '1+b.ts', '0+docs', '0+a.md']);
  const withDocs = new Map<string, Listing>([...listings, [r`c:\p\docs`, ready([])]]);
  assert.deepEqual(shape(treeRows(root, new Set([r`c:\p\docs`]), withDocs, false)), ['0+src', '0-docs', '1:This folder is empty', '0+a.md']);

  const at = (name: string) => open.findIndex((x) => x.type === 'entry' && x.name === name);
  const keys: Array<[string, string, unknown]> = [
    ['src', 'ArrowDown', { type: 'focus', index: at('ui') }],
    ['ui', 'ArrowDown', { type: 'focus', index: at('b.ts') }],
    ['b.ts', 'ArrowUp', { type: 'focus', index: at('ui') }],
    ['docs', 'ArrowRight', { type: 'toggle', path: r`C:\p\docs`, open: true }],
    ['src', 'ArrowRight', { type: 'focus', index: at('ui') }],
    ['src', 'ArrowLeft', { type: 'toggle', path: r`C:\p\src`, open: false }],
    ['b.ts', 'ArrowLeft', { type: 'focus', index: at('src') }],
    ['a.md', 'Enter', { type: 'open', path: r`C:\p\a.md` }],
    ['docs', 'Enter', { type: 'toggle', path: r`C:\p\docs`, open: true }],
    ['a.md', 'End', null],
    ['src', 'Home', null],
    ['b.ts', 'x', null],
  ];
  for (const [name, key, want] of keys) assert.deepEqual(treeKey(open, at(name), key), want, `${name} ${key}`);
});
