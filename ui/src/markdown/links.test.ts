import assert from 'node:assert/strict';
import { test } from 'node:test';
import { classifyHref, isImageName, parseChatLink, parseWikilink, targetAttrs, targetFromAttrs } from './links.ts';

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

test('chat links are accepted only in their strict forms', () => {
  const id = '0b8f6a52-1c3d-4e5f-8a9b-0c1d2e3f4a5b';
  assert.deepEqual(classifyHref(`helm://chats/${id}`), { kind: 'chat', href: `helm://chats/${id}` });
  assert.deepEqual(parseChatLink(`helm://chats/${id}/messages/42`), { chatId: id, messageId: 42 });
  for (const bad of [
    `helm://chats/${id}/`,
    `helm://chats/${id}?x=1`,
    `helm://chats/${id}#m`,
    `helm://u@chats/${id}`,
    `helm://chats:80/${id}`,
    `helm://chats/${id}/messages/abc`,
    `helm://chats/${id}/extra`,
    `helm://chats/${id.toUpperCase()}`,
    'helm://chats/not-an-id',
    `other://chats/${id}`,
  ]) {
    assert.equal(parseChatLink(bad), null, bad);
    assert.equal(classifyHref(bad), null, bad);
  }
});

test('classifyHref treats a Windows drive path as a path, not a scheme', () => {
  assert.deepEqual(classifyHref('C:\\notes\\a.txt'), { kind: 'url', href: 'C:\\notes\\a.txt' });
  assert.deepEqual(classifyHref('C:\\notes\\a.md'), { kind: 'vault', note: 'C:\\notes\\a' });
});

test('classifyHref maps in-page anchors and md paths to vault targets', () => {
  assert.deepEqual(classifyHref('#My%20Heading'), { kind: 'vault', note: '', heading: 'My Heading' });
  assert.deepEqual(classifyHref('../x/Note.md#^blk'), { kind: 'vault', note: '../x/Note', block: 'blk' });
  assert.deepEqual(classifyHref('obsidian://open?vault=V&file=A%2FB.md'), { kind: 'vault', note: 'A/B', vault: 'V' });
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

// ---- The link gate's first layer (network shares and device paths) ----

const r = String.raw;

test('classifyHref refuses the "C:javascript:" family and drive-relative paths', () => {
  for (const bad of ['C:javascript:alert(1)', 'c:javascript:alert(1)', 'C:x', 'C:x.md', 'Z:vbscript:x']) {
    assert.equal(classifyHref(bad), null, JSON.stringify(bad));
  }
});

test('classifyHref accepts a drive letter followed by either slash', () => {
  assert.deepEqual(classifyHref('c:/x.md'), { kind: 'vault', note: 'c:/x' });
  assert.deepEqual(classifyHref(r`C:\x.md`), { kind: 'vault', note: r`C:\x` });
  assert.deepEqual(classifyHref('C:/src/a.ts'), { kind: 'url', href: 'C:/src/a.ts' });
});

test('classifyHref refuses UNC and device paths before anything can touch them', () => {
  for (const bad of [
    r`\\host\share\x.png`,
    r`\\host\share\note.md`,
    '//host/share/x',
    r`/\host\share`,
    r`\/host/share`,
    r`\\?\C:\x.txt`,
    r`\\.\pipe\x`,
    r`\??\C:\x`,
    'file://host/share/x.txt',
    'FILE://Host/share',
    'file:////host/share/x',
    'file://///host/share/x',
    'file:///%5C%5Chost/share',
    '%5C%5Chost%5Cshare%5Cx',
    'obsidian://open?vault=V&file=%5C%5Chost%5Cshare%5Cx',
  ]) {
    assert.equal(classifyHref(bad), null, JSON.stringify(bad));
  }
});

test('classifyHref keeps local file URLs', () => {
  assert.deepEqual(classifyHref('file:///C:/x/a.txt'), { kind: 'url', href: 'file:///C:/x/a.txt' });
  assert.deepEqual(classifyHref('file://localhost/C:/a.txt'), { kind: 'url', href: 'file://localhost/C:/a.txt' });
});

test('wikilinks naming a share or device path stay text', () => {
  assert.equal(parseWikilink(r`\\host\share\x`), null);
  assert.equal(parseWikilink('//host/share/x.png'), null);
  assert.equal(parseWikilink(r`\\?\C:\x`), null);
});

test('an obsidian:// link keeps the vault it names, through the data attributes too', () => {
  const t = classifyHref('obsidian://open?vault=My%20Vault&file=A%2FB.md');
  assert.deepEqual(t, { kind: 'vault', note: 'A/B', vault: 'My Vault' });
  const attrs = new Map(targetAttrs(t!));
  assert.deepEqual(targetFromAttrs((n) => attrs.get(n) ?? null), t);
});
