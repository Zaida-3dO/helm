import assert from 'node:assert/strict';
import { test } from 'node:test';
import { classifyHref, isImageName, parseWikilink, targetAttrs, targetFromAttrs } from './links.ts';

test('parseWikilink splits note, heading, block and alias', () => {
  assert.deepEqual(parseWikilink('Note'), { kind: 'vault', note: 'Note' });
  assert.deepEqual(parseWikilink('Note|Alias'), { kind: 'vault', note: 'Note', alias: 'Alias' });
  assert.deepEqual(parseWikilink('Folder/Note#Head|A'), {
    kind: 'vault', note: 'Folder/Note', heading: 'Head', alias: 'A',
  });
  assert.deepEqual(parseWikilink('Note#^b1'), { kind: 'vault', note: 'Note', block: 'b1' });
  assert.deepEqual(parseWikilink('Note#Head#^b1'), {
    kind: 'vault', note: 'Note', heading: 'Head', block: 'b1',
  });
  assert.deepEqual(parseWikilink('#Only heading'), { kind: 'vault', note: '', heading: 'Only heading' });
  assert.deepEqual(parseWikilink('Note\\|Alias'), { kind: 'vault', note: 'Note', alias: 'Alias' });
});

test('parseWikilink refuses links that name nothing', () => {
  for (const s of ['', ' ', '|alias', '#', '#^']) assert.equal(parseWikilink(s), null, JSON.stringify(s));
});

test('classifyHref allows only the listed schemes', () => {
  assert.deepEqual(classifyHref('https://a.example/x'), { kind: 'url', href: 'https://a.example/x' });
  assert.deepEqual(classifyHref('mailto:a@b.co'), { kind: 'url', href: 'mailto:a@b.co' });
  for (const bad of [
    'javascript:alert(1)',
    ' JAVASCRIPT:alert(1)',
    'java\nscript:alert(1)',
    'vbscript:x',
    'data:text/html,<b>',
    'ftp://x',
    'tauri://localhost',
    '//evil.example',
    '',
  ]) {
    assert.equal(classifyHref(bad), null, JSON.stringify(bad));
  }
});

test('classifyHref treats a Windows drive path as a path, not a scheme', () => {
  assert.deepEqual(classifyHref('C:\\notes\\a.txt'), { kind: 'url', href: 'C:\\notes\\a.txt' });
  assert.deepEqual(classifyHref('C:\\notes\\a.md'), { kind: 'vault', note: 'C:\\notes\\a' });
});

test('classifyHref maps in-page anchors and md paths to vault targets', () => {
  assert.deepEqual(classifyHref('#My%20Heading'), { kind: 'vault', note: '', heading: 'My Heading' });
  assert.deepEqual(classifyHref('../x/Note.md#^blk'), { kind: 'vault', note: '../x/Note', block: 'blk' });
  assert.deepEqual(classifyHref('obsidian://open?vault=V&file=A%2FB.md'), { kind: 'vault', note: 'A/B' });
  assert.equal(classifyHref('obsidian://other?x=1'), null);
  assert.equal(classifyHref('obsidian://open?vault=V'), null);
});

test('targets survive the round trip through data attributes', () => {
  const targets = [
    { kind: 'vault', note: 'A/B', heading: 'H', alias: 'x' },
    { kind: 'vault', note: '', block: 'b' },
    { kind: 'url', href: 'https://a.example/?q=1&r=2' },
  ] as const;
  for (const t of targets) {
    const attrs = new Map(targetAttrs(t));
    assert.deepEqual(targetFromAttrs((n) => attrs.get(n) ?? null), t);
  }
  assert.equal(targetFromAttrs(() => null), null);
  assert.equal(targetFromAttrs((n) => (n === 'data-md-kind' ? 'javascript' : null)), null);
});

test('isImageName recognises image extensions only', () => {
  assert.equal(isImageName('a.PNG'), true);
  assert.equal(isImageName('dir/a.webp'), true);
  assert.equal(isImageName('note'), false);
  assert.equal(isImageName('a.png.md'), false);
});
