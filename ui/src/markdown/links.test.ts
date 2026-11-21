import assert from 'node:assert/strict';
import { test } from 'node:test';
import { classifyHref, helmFileHref, isImageName, linkFromClick, parseChatLink, parseHelmLink, parseWikilink, targetAttrs, targetFromAttrs } from './links.ts';

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

test('chat links: strict forms, with the id case-insensitive and one trailing slash tolerated', () => {
  const id = '0b8f6a52-1c3d-4e5f-8a9b-0c1d2e3f4a5b';
  assert.deepEqual(classifyHref(`helm://chats/${id}`), { kind: 'chat', href: `helm://chats/${id}` });
  assert.deepEqual(parseChatLink(`helm://chats/${id}/messages/42`), { chatId: id, messageId: 42 });
  // A hand-written or copied link: uppercase letters come back as the canonical lowercase id,
  // and one trailing slash at the end is tolerated (the gate still re-validates the same way).
  assert.deepEqual(parseChatLink(`helm://chats/${id.toUpperCase()}`), { chatId: id });
  assert.deepEqual(parseChatLink(`helm://chats/${id}/`), { chatId: id });
  assert.deepEqual(parseChatLink(`helm://chats/${id.toUpperCase()}/messages/42/`), { chatId: id, messageId: 42 });
  for (const bad of [
    `helm://chats/${id}//`,
    `helm://chats/${id}?x=1`,
    `helm://chats/${id}#m`,
    `helm://u@chats/${id}`,
    `helm://chats:80/${id}`,
    `helm://chats/${id}/messages/abc`,
    `helm://chats/${id}/extra`,
    'helm://chats/not-an-id',
    `other://chats/${id}`,
  ]) {
    assert.equal(parseChatLink(bad), null, bad);
    assert.equal(classifyHref(bad), null, bad);
  }
});

test('file, page and panel links are read in their strict forms; anything else under helm: stays text', () => {
  const ok: [string, unknown][] = [
    ['helm://files/C%3A%2Fsrc%2Fa.ts', { form: 'file', path: 'C:/src/a.ts' }],
    ['helm://files/C:/src/a.ts:12', { form: 'file', path: 'C:/src/a.ts', line: 12 }],
    ['helm://files/C%3A%5Csrc%5Ca%20b.ts:12:4', { form: 'file', path: String.raw`C:\src\a b.ts`, line: 12, column: 4 }],
    ['helm://files/src%2Fa.ts', { form: 'file', path: 'src/a.ts' }],
    ['helm://files/~/notes/x.md', { form: 'file', path: '~/notes/x.md' }],
    // An encoded colon stays part of the name.
    ['helm://files/C%3A%2Fa%3A5', { form: 'file', path: 'C:/a:5' }],
    ['helm://pages/3f2a9c0d4b5e6f708192a3b4c5d6e7f8', { form: 'page', pageId: '3f2a9c0d4b5e6f708192a3b4c5d6e7f8' }],
    ['helm://panel/changes', { form: 'panel', view: 'changes' }],
    ['helm://panel/files', { form: 'panel', view: 'files' }],
    ['helm://panel/tasks', { form: 'panel', view: 'tasks' }],
  ];
  for (const [href, want] of ok) {
    assert.deepEqual(parseHelmLink(href), want, href);
    assert.deepEqual(classifyHref(href), { kind: 'helm', href }, href);
  }
  for (const bad of [
    // Control characters, encoded (a raw one never survives classifyHref's strip).
    'helm://files/C%3A%2Fa%0A.ts',
    'helm://files/C%3A%2Fa%00.ts',
    'helm://files/C%3A%2Fa%C2%85.ts',
    // Shares and device paths, in every spelling, double-encoded too.
    'helm://files/%5C%5Chost%5Cshare%5Cx.ts',
    'helm://files/%2F%2Fhost%2Fshare',
    'helm://files///host/share',
    'helm://files/%255C%255Chost%255Cshare',
    'helm://files/%5C%5C%3F%5CC%3A%5Cx',
    'helm://files/file%3A%2F%2Fhost%2Fshare',
    // Drive-relative, a scheme inside, broken escapes, and the shape rules.
    'helm://files/C%3Ax.ts',
    'helm://files/C:5',
    'helm://files/javascript%3Aalert(1)',
    'helm://files/https%3A%2F%2Fevil.example',
    'helm://files/%E0%A4%A',
    'helm://files/',
    'helm://files/%20',
    'helm://files/a.ts:0',
    'helm://files/a.ts?x=1',
    'helm://files/a.ts#L3',
    String.raw`helm://files/C:\x.ts`,
    'helm://files/a b.ts',
    // Pages: an id of letters, digits and dashes, at most 64.
    'helm://pages/',
    'helm://pages/a_b',
    'helm://pages/../x',
    'helm://pages/' + 'a'.repeat(65),
    'helm://pages/abc/',
    'helm://pages/abc?x',
    // Panel views: the three named, nothing else.
    'helm://panel/other',
    'helm://panel/',
    'helm://panel/Changes',
    'helm://panel/files/x',
    'helm://u@pages/abc',
    'helm://files:80/a.ts',
    'helm://settings/x',
    'HELM://pages/abc',
  ]) {
    assert.equal(parseHelmLink(bad), null, bad);
    assert.equal(classifyHref(bad), null, bad);
  }
  // What Helm writes reads back the same, whatever the path holds.
  for (const [path, line] of [[String.raw`C:\a b\(x)!.ts`, 3], ['C:/a:5', undefined], ['src/#hash.ts', 9]] as const) {
    const back = parseHelmLink(helmFileHref(path, line));
    assert.deepEqual(back, line === undefined ? { form: 'file', path } : { form: 'file', path, line }, path);
  }
  const attrs = new Map(targetAttrs({ kind: 'helm', href: 'helm://panel/files' }));
  assert.deepEqual(targetFromAttrs((n) => attrs.get(n) ?? null), { kind: 'helm', href: 'helm://panel/files' });
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

test('a bare relative word is text, while anything path-like or an in-page anchor still links', () => {
  for (const bare of ['income', 'Income#x', 'notes ', '%69ncome']) assert.equal(classifyHref(bare), null, bare);
  assert.deepEqual(classifyHref('docs/x'), { kind: 'url', href: 'docs/x' });
  assert.deepEqual(classifyHref('notes.txt'), { kind: 'url', href: 'notes.txt' });
  assert.deepEqual(classifyHref('Makefile'), { kind: 'url', href: 'Makefile' });
  assert.deepEqual(classifyHref('Note.md'), { kind: 'vault', note: 'Note' });
  assert.deepEqual(classifyHref('#a'), { kind: 'vault', note: '', heading: 'a' });
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

test('a click on a rendered link is read the same whatever the button, and always cancelled', () => {
  const id = '0b8f6a52-1c3d-4e5f-8a9b-0c1d2e3f4a5b';
  const attrs = new Map(targetAttrs({ kind: 'chat', href: `helm://chats/${id}/messages/9` }));
  const anchor = { getAttribute: (n: string) => attrs.get(n) ?? null };
  for (const button of [0, 1]) {
    let cancelled = false;
    const event = {
      target: { closest: (sel: string) => (sel === 'a[data-md-kind]' ? anchor : null) },
      currentTarget: { contains: () => true },
      preventDefault: () => {
        cancelled = true;
      },
      button,
    };
    assert.deepEqual(linkFromClick(event), { kind: 'chat', href: `helm://chats/${id}/messages/9` }, `button ${button}`);
    assert.ok(cancelled, `button ${button}: the webview must not navigate`);
  }
  // A click not on a link reads nothing and cancels nothing.
  let cancelled = false;
  const elsewhere = {
    target: { closest: () => null },
    currentTarget: { contains: () => true },
    preventDefault: () => {
      cancelled = true;
    },
  };
  assert.equal(linkFromClick(elsewhere), null);
  assert.equal(cancelled, false);
});
