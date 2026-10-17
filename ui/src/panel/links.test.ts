import assert from 'node:assert/strict';
import { test } from 'node:test';
import { configureLinks, gateTarget, handleLink, REFUSED, registerLinkRoute } from './links.ts';

const r = String.raw;
const url = (href: string) => ({ kind: 'url' as const, href });

test('the "C:javascript:" family is refused at the gate, whatever the classifier said', () => {
  for (const href of ['C:javascript:alert(1)', 'c:JavaScript:x', 'C:x', 'D:vbscript:msgbox']) {
    assert.deepEqual(gateTarget(url(href)), { action: 'refuse', reason: REFUSED.driveRelative }, href);
  }
});

test('UNC and device paths are refused from the string, in every spelling and both kinds', () => {
  const spellings = [
    r`\\host\share\x.txt`,
    '//host/share/x.txt',
    r`/\host\share`,
    r`\\?\C:\x.txt`,
    r`\\.\pipe\p`,
    r`\??\C:\x`,
    'file://host/share/x.txt',
    'file:////host/share/x.txt',
    'file:///%5C%5Chost/share/x.txt',
    '%5c%5chost%5cshare',
  ];
  for (const s of spellings) {
    assert.deepEqual(gateTarget(url(s)), { action: 'refuse', reason: REFUSED.share }, `url ${s}`);
    assert.deepEqual(gateTarget({ kind: 'vault', note: s }), { action: 'refuse', reason: REFUSED.share }, `vault ${s}`);
  }
});

test('only http, https and mailto go external; other schemes are refused', () => {
  assert.deepEqual(gateTarget(url('https://example.com/a?b=1')), { action: 'external', url: 'https://example.com/a?b=1' });
  assert.deepEqual(gateTarget(url('http://example.com')), { action: 'external', url: 'http://example.com/' });
  assert.deepEqual(gateTarget(url('mailto:a@b.co')), { action: 'external', url: 'mailto:a@b.co' });
  for (const s of ['javascript:alert(1)', 'data:text/html,x', 'vbscript:x', 'ftp://x', 'tauri://localhost', 'obsidian://open?file=x', 'ms-settings:']) {
    assert.deepEqual(gateTarget(url(s)), { action: 'refuse', reason: REFUSED.scheme }, s);
  }
  assert.equal(gateTarget(url('https://user:pw@example.com')).action, 'refuse');
});

test('absolute drive paths and local file URLs go to the file viewer, with an optional line', () => {
  assert.deepEqual(gateTarget(url('C:/src/a.ts')), { action: 'file', path: 'C:/src/a.ts' });
  assert.deepEqual(gateTarget(url(r`C:\src\a.ts:12`)), { action: 'file', path: r`C:\src\a.ts`, line: 12 });
  assert.deepEqual(gateTarget(url('C:/src/a.ts:12:4')), { action: 'file', path: 'C:/src/a.ts', line: 12, column: 4 });
  assert.deepEqual(gateTarget(url('file:///C:/a%20b/c.ts:3')), { action: 'file', path: 'C:/a b/c.ts', line: 3 });
  assert.deepEqual(gateTarget(url('file://localhost/C:/x.txt')), { action: 'file', path: 'C:/x.txt' });
  assert.deepEqual(gateTarget(url('file://C:/x.txt:4')), { action: 'file', path: 'C:/x.txt', line: 4 });
  // An encoded colon stays part of the name.
  assert.deepEqual(gateTarget(url('file:///C:/a%3A5')), { action: 'file', path: 'C:/a:5' });
});

test('malformed or relative file links are refused', () => {
  assert.equal(gateTarget(url('file:///C:/x?y')).action, 'refuse');
  assert.equal(gateTarget(url('file:///C:/x#y')).action, 'refuse');
  assert.equal(gateTarget(url('file:///C:/%E0%A4%A')).action, 'refuse');
  assert.equal(gateTarget(url('file:///C:/a%00b')).action, 'refuse');
  assert.deepEqual(gateTarget(url('src/a.ts')), { action: 'refuse', reason: REFUSED.relative });
  assert.deepEqual(gateTarget(url('/rooted/a.ts')), { action: 'refuse', reason: REFUSED.relative });
  assert.deepEqual(gateTarget(url('file:///relative.txt')), { action: 'refuse', reason: REFUSED.relative });
  assert.equal(gateTarget(url('C:/a\u0007b')).action, 'refuse');
  assert.equal(gateTarget(url('C:/' + 'x'.repeat(9000))).action, 'refuse');
});

test('vault targets keep note, heading, block and vault; an empty note is an in-page anchor', () => {
  assert.deepEqual(gateTarget({ kind: 'vault', note: 'A/B', heading: 'H', vault: 'V' }), {
    action: 'vault', note: 'A/B', heading: 'H', vault: 'V',
  });
  assert.deepEqual(gateTarget({ kind: 'vault', note: '', heading: 'H' }), { action: 'anchor', heading: 'H', block: undefined });
  assert.equal(gateTarget({ kind: 'vault', note: '' }).action, 'refuse');
  assert.equal(gateTarget({ kind: 'vault', note: 'x', vault: '../evil' }).action, 'refuse');
  assert.deepEqual(gateTarget({ kind: 'vault', note: 'C:x' }), { action: 'refuse', reason: REFUSED.driveRelative });
});

test('anything that is not a known target is refused', () => {
  for (const t of [null, undefined, 1, 'x', {}, { kind: 'script', href: 'x' }, { kind: 'url' }]) {
    assert.equal(gateTarget(t).action, 'refuse', JSON.stringify(t));
  }
});

test('handleLink acts only after the gate, and a refusal opens nothing and shows a notice', async () => {
  const opened: string[] = [];
  const notices: string[] = [];
  const files: string[] = [];
  configureLinks({
    openExternal: async (u) => void opened.push(u),
    notify: (m) => void notices.push(m),
  });
  const off = registerLinkRoute('file', (req) => {
    files.push(req.path);
    return true;
  });
  try {
    assert.deepEqual(await handleLink(url('C:javascript:alert(1)'), { chatId: 'c1' }), {
      ok: false, reason: REFUSED.driveRelative,
    });
    assert.deepEqual(await handleLink(url(r`\\host\share\x.txt`), { chatId: 'c1' }), { ok: false, reason: REFUSED.share });
    assert.deepEqual(opened, []);
    assert.deepEqual(files, []);
    assert.deepEqual(notices, [REFUSED.driveRelative, REFUSED.share]);

    assert.deepEqual(await handleLink(url('https://example.com'), { chatId: 'c1' }), { ok: true, action: 'external' });
    assert.deepEqual(opened, ['https://example.com/']);
    assert.deepEqual(await handleLink(url('C:/a.ts:3'), { chatId: 'c1' }), { ok: true, action: 'file' });
    assert.deepEqual(files, ['C:/a.ts']);
  } finally {
    off();
  }
  // Without a registered route the link is refused with a notice, not dropped silently.
  const out = await handleLink(url('C:/a.ts'), { chatId: 'c1' });
  assert.equal(out.ok, false);
});

test('a failing host call becomes a refusal notice rather than an unhandled rejection', async () => {
  const notices: string[] = [];
  configureLinks({
    openExternal: async () => {
      throw new Error('refused by host');
    },
    notify: (m) => void notices.push(m),
  });
  assert.deepEqual(await handleLink(url('https://example.com'), { chatId: null }), { ok: false, reason: 'refused by host' });
  assert.deepEqual(notices, ['refused by host']);
});
