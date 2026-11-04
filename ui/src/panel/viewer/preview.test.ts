import assert from 'node:assert/strict';
import { test } from 'node:test';
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
