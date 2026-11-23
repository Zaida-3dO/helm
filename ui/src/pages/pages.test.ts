import assert from 'node:assert/strict';
import { test } from 'node:test';
import { PageBridge } from './bridge.ts';
import {
  allowSend,
  checkHandoff,
  checkLink,
  checkState,
  composeHandoff,
  MARK,
  MAX_OVERVIEW_FALLBACK,
  MAX_PENDING,
  overviewFallback,
  PAGE_SANDBOX,
  pageSandbox,
  parseMessage,
} from './protocol.ts';
import { buildTheme, plainValue, THEME_VARIABLES } from './theme.ts';
import { remoteTabKey, validPagePath, validRemote } from './open.ts';
import { mergePages, tileLetter } from './list.ts';
import type { PinnedPage, RemotePages } from './api.ts';
import type { DeviceInfo, DevicesList } from '../devices/api.ts';

const CHANNEL = 'chan1';
const msg = (type: string, extra: Record<string, unknown> = {}) => ({ mark: MARK, channel: CHANNEL, type, ...extra });

test('what a frame may say: only well-formed messages on this channel are heard', () => {
  const rows: Array<[unknown, unknown]> = [
    [msg('ready'), { type: 'ready' }],
    [msg('request', { id: 3, method: 'setState', payload: { a: 1 } }), { type: 'request', id: 3, method: 'setState', payload: { a: 1 } }],
    [msg('error', { message: 'x'.repeat(2000) }), { type: 'error', message: 'x'.repeat(1000) }],
    [msg('networkBlocked', { uri: 'https://a.example/x' }), { type: 'networkBlocked', uri: 'https://a.example/x' }],
    [{ ...msg('ready'), channel: 'other' }, null],
    [{ ...msg('ready'), mark: 'nope' }, null],
    [msg('request', { id: 0, method: 'setState' }), null],
    [msg('request', { id: 1.5, method: 'setState' }), null],
    [msg('request', { id: 1, method: 7 }), null],
    [msg('request', { id: 1, method: 'x'.repeat(40) }), null],
    [msg('init'), null],
    [msg('response', { id: 1 }), null],
    ['ready', null],
    [null, null],
  ];
  for (const [data, want] of rows) assert.deepEqual(parseMessage(data, CHANNEL), want, JSON.stringify(data)?.slice(0, 80));
});

test('a message from a page is checked, worded with the page named, and its data kept as JSON', () => {
  const bad: unknown[] = [
    null,
    {},
    { text: '   ' },
    { text: 'x'.repeat(16_001) },
    { text: 'hi', title: 7 },
    { text: 'hi', title: 'x'.repeat(241) },
    { text: 'hi', overview: 7 },
    { text: 'hi', overview: 'x'.repeat(2_001) },
    { text: 'hi', data: { big: 'x'.repeat(70_000) } },
    { text: 'hi', data: 1n },
  ];
  for (const p of bad) assert.equal(checkHandoff(p).ok, false, JSON.stringify(p, (_, v) => (typeof v === 'bigint' ? 'big' : v))?.slice(0, 60));

  const ok = checkHandoff({ text: '  Approve step 2  ', title: ' Review ', overview: '  five parts  ', data: { step: 2 } });
  assert.ok(ok.ok);
  assert.deepEqual(ok.ok && ok.value, { text: 'Approve step 2', title: 'Review', overview: 'five parts', data: { step: 2 } });
  const text = composeHandoff(ok.ok ? ok.value : { text: '' }, 'Tracker');
  assert.ok(text.startsWith('Approve step 2\n\n[Sent from the page "Tracker" in Helm'));
  assert.ok(text.includes('not the user'), 'it never reads as the user’s own words');
  assert.ok(text.endsWith('```json\n{\n  "step": 2\n}\n```'));
  assert.equal(composeHandoff({ text: 'x' }, 'a "quoted"\nname').split('\n').length, 3, 'a page cannot break the marker line');
});

test('a card overview the page did not give falls back to the first lines, blank lines skipped and length capped', () => {
  assert.equal(overviewFallback('line one\n\nline two\nline three\nline four kept but unused'), 'line one\nline two\nline three');
  assert.equal(overviewFallback('only line'), 'only line');
  assert.equal(overviewFallback('   \n\n \n'), '');
  // A single long line is cut at the cap with an ellipsis, never past it.
  const long = 'x'.repeat(320);
  const cut = overviewFallback(long);
  assert.ok(cut.length <= MAX_OVERVIEW_FALLBACK + 1 && cut.endsWith('…'), cut.slice(-4));
});

test('a page may not send faster than once in two seconds or ten in ten minutes', () => {
  // (why, recent sends as ms offsets from the moment checked, allowed?)
  const rows: Array<[string, number[], boolean]> = [
    ['first send ever', [], true],
    ['2.5 s after the last', [2_500], true],
    ['1.9 s after the last', [1_900], false],
    // (sends two seconds apart, the longest the gap rule permits)
    ['a burst of 9, this is the 10th', [18_000, 16_000, 14_000, 12_000, 10_000, 8_000, 6_000, 4_000, 2_000], true],
    [
      'the window is full of ten',
      [20_000, 18_000, 16_000, 14_000, 12_000, 10_000, 8_000, 6_000, 4_000, 2_000],
      false,
    ],
    // Old sends leave the 10-minute window and no longer count.
    [
      'ten, the oldest 11 minutes ago',
      [660_000, 20_000, 18_000, 16_000, 14_000, 12_000, 10_000, 8_000, 6_000, 4_000],
      true,
    ],
  ];
  const now = 1_000_000;
  for (const [why, offsets, allowed] of rows) {
    const sent = offsets.map((o) => now - o);
    const r = allowSend(sent, now);
    assert.equal(r.ok, allowed, why);
    // The kept list drops what left the window and, on an allowed send, adds this one.
    const expected = sent.filter((t) => now - t < 10 * 60_000);
    assert.ok(
      allowed
        ? r.kept.length === expected.length + 1 && r.kept[r.kept.length - 1] === now
        : r.kept.length === expected.length,
      `${why}: kept=${r.kept.length}`,
    );
  }
});

test('links and saved state from a page: web links only, never Helm’s own hosts, state within its limit', () => {
  const links: Array<[unknown, boolean]> = [
    [{ url: 'https://example.com/a?b=1#c' }, true],
    [{ url: 'http://example.com' }, true],
    [{ url: 'file:///C:/Windows/win.ini' }, false],
    [{ url: 'javascript:alert(1)' }, false],
    [{ url: 'data:text/html,hi' }, false],
    [{ url: 'http://helmpage.localhost/abc/index.html' }, false],
    [{ url: 'http://helmfile.localhost/x' }, false],
    [{ url: 'http://ipc.localhost/' }, false],
    [{ url: 'http://127.0.0.1:1420/' }, false],
    [{ url: 'http://[::1]/' }, false],
    [{ url: 'https://user:pass@example.com/' }, false],
    [{ url: 'not a url' }, false],
    [{ url: 'https://example.com/' + 'a'.repeat(5000) }, false],
    [{}, false],
    [null, false],
  ];
  for (const [p, ok] of links) assert.equal(checkLink(p).ok, ok, JSON.stringify(p)?.slice(0, 80));

  const states: Array<[unknown, boolean]> = [
    [{ state: { a: [1, 2] }, revision: 0 }, true],
    [{ state: null, revision: 4 }, true],
    [{ state: 'x'.repeat(256 * 1024), revision: 0 }, false],
    [{ state: undefined, revision: 0 }, false],
    [{ state: 1, revision: -1 }, false],
    [{ state: 1, revision: 1.5 }, false],
    [{ state: 1 }, false],
    [null, false],
  ];
  for (const [p, ok] of states) assert.equal(checkState(p).ok, ok, JSON.stringify(p)?.slice(0, 60));
});

test('the sandbox gives a page scripts and nothing that could reach Helm or the disk', () => {
  assert.deepEqual(PAGE_SANDBOX.split(' '), ['allow-scripts']);
  assert.equal(pageSandbox(true), PAGE_SANDBOX);
  assert.equal(pageSandbox(false), '', 'no scripts: nothing allowed at all');
  for (const never of ['allow-same-origin', 'allow-downloads', 'allow-top-navigation', 'allow-popups', 'allow-forms', 'allow-modals']) {
    assert.ok(!PAGE_SANDBOX.includes(never), never);
  }
});

test('the theme handed to a page: the app’s tokens under the page’s names, plain values only', () => {
  const tokens: Record<string, string> = { '--bg': '#fff', '--text': ' #111 ', '--surface': 'url(http://evil/x)', '--muted': 'a;b', '--accent': '#09f', '--line-strong': '', '--font-ui': "'Segoe UI', system-ui", '--font-mono': 'x'.repeat(300) };
  const t = buildTheme((k) => tokens[k] ?? '', true, true);
  assert.deepEqual(t, {
    mode: 'dark',
    reducedMotion: true,
    variables: { '--helm-bg': '#fff', '--helm-text': '#111', '--helm-accent': '#09f', '--helm-font': "'Segoe UI', system-ui" },
  });
  assert.equal(buildTheme(() => '#000', false, false).mode, 'light');
  assert.equal(plainValue('expression(1)'), null);
  assert.equal(plainValue('@import x'), null);
  for (const name of Object.keys(THEME_VARIABLES)) assert.match(name, /^--helm-[a-z-]+$/);
});

interface Sent {
  [k: string]: unknown;
}

function bridge(over: { visible?: () => boolean; request?: (m: string, p: unknown) => Promise<unknown>; frame?: () => unknown; now?: () => number } = {}) {
  const frame = { id: 'frame' };
  const sent: Sent[] = [];
  const seen = { ready: 0, errors: [] as string[], blocked: 0, calls: [] as Array<[string, unknown]> };
  const b = new PageBridge({
    channel: CHANNEL,
    frame: over.frame ?? (() => frame),
    post: (m) => sent.push(m),
    visible: over.visible ?? (() => true),
    request:
      over.request ??
      ((m, p) => {
        seen.calls.push([m, p]);
        return Promise.resolve({ ok: m });
      }),
    onReady: () => seen.ready++,
    onError: (m) => seen.errors.push(m),
    onNetworkBlocked: () => seen.blocked++,
    now: over.now,
  });
  return { b, frame, sent, seen };
}

const tick = () => new Promise((r) => setTimeout(r, 0));

test('the bridge hears only its own frame, answers the three methods, and refuses the rest', async () => {
  const { b, frame, sent, seen } = bridge();
  const req = (id: number, method: string, payload: unknown = {}) => b.receive({ source: frame, data: msg('request', { id, method, payload }) });

  // Other windows, other channels and the app itself are not heard.
  b.receive({ source: { id: 'other' }, data: msg('ready') });
  b.receive({ source: null, data: msg('ready') });
  b.receive({ source: frame, data: { ...msg('ready'), channel: 'nope' } });
  assert.equal(seen.ready, 0);
  b.receive({ source: frame, data: msg('ready') });
  b.receive({ source: frame, data: msg('networkBlocked', { uri: 'https://x' }) });
  assert.deepEqual([seen.ready, seen.blocked], [1, 1]);

  // The three methods run; anything else is refused without running.
  req(1, 'setState', { state: 1, revision: 0 });
  req(2, 'sendMessage', { text: 'hi' });
  req(3, 'openLink', { url: 'https://a.example' });
  for (const method of ['api', 'chat_send', 'file_read', 'constructor', '__proto__']) req(9, method);
  await tick();
  assert.deepEqual(seen.calls.map((c) => c[0]), ['setState', 'sendMessage', 'openLink']);
  const answers = sent.filter((m) => m.type === 'response');
  assert.deepEqual(answers.filter((m) => m.id === 9).map((m) => m.error), Array(5).fill('Unsupported app request.'));
  assert.deepEqual(answers.filter((m) => m.id !== 9).map((m) => [m.id, m.result]), [[1, { ok: 'setState' }], [2, { ok: 'sendMessage' }], [3, { ok: 'openLink' }]]);
  assert.ok(answers.every((m) => m.mark === MARK && m.channel === CHANNEL));

  // Closing stops hearing and answering.
  b.close();
  req(4, 'setState');
  b.send('theme', {});
  await tick();
  assert.equal(sent.length, answers.length);
});

test('the bridge refuses requests while hidden, over the cap, or too large, and passes errors back', async () => {
  let visible = true;
  const waiting: Array<() => void> = [];
  const { b, frame, sent } = bridge({
    visible: () => visible,
    request: (m) => (m === 'openLink' ? new Promise((_, rej) => waiting.push(() => rej(new Error('Cancelled.')))) : Promise.reject(new Error('The app request failed.'))),
  });
  const req = (id: number, method = 'openLink', payload: unknown = {}) => b.receive({ source: frame, data: msg('request', { id, method, payload }) });
  const answer = (id: number) => sent.find((m) => m.type === 'response' && m.id === id);

  visible = false;
  req(1);
  assert.equal(answer(1)?.error, 'This app cannot request that action right now.');
  visible = true;

  req(2, 'openLink', { big: 'x'.repeat(301 * 1024) });
  assert.equal(answer(2)?.error, 'App request is too large.');
  req(3, 'setState');
  await tick();
  assert.equal(answer(3)?.error, 'The app request failed.', 'an op’s refusal reaches the page');

  for (let i = 0; i < MAX_PENDING; i++) req(10 + i);
  req(99);
  assert.equal(answer(99)?.error, 'Too many pending app requests.');
  waiting.forEach((w) => w());
  await tick();
  assert.equal(answer(10)?.error, 'Cancelled.');
  req(100, 'setState');
  await tick();
  assert.equal(answer(100)?.error, 'The app request failed.', 'room again once the waiting ones settle');
});

test('page errors reach the strip at most once a second; a missing frame hears nothing', () => {
  let now = 1000;
  const { b, frame, seen } = bridge({ now: () => now });
  const err = (m: string) => b.receive({ source: frame, data: msg('error', { message: m }) });
  err('a');
  err('b');
  now += 999;
  err('c');
  now += 2;
  err('d');
  assert.deepEqual(seen.errors, ['a', 'd']);

  const gone = bridge({ frame: () => null });
  gone.b.receive({ source: null, data: msg('ready') });
  gone.b.receive({ source: undefined, data: msg('ready') });
  assert.equal(gone.seen.ready, 0);
});

test('one Pages list: this PC first, other PCs as plain rows with a detail, deduped and filtered to the person', () => {
  const pin = (id: string, profileId: string, path: string): PinnedPage => ({ id, profileId, path, title: id, chatId: null, pinnedAt: 1 });
  const rem = (uid: string, name: string, pages: PinnedPage[], problem: { code: string; message: string } | null = null): RemotePages => ({ deviceUid: uid, name, pages, problem });
  const dev = (uid: string, name: string, profiles?: { profileId: string; name: string; personId?: string | null }[]): DeviceInfo => ({
    uid,
    name,
    person: null,
    online: true,
    lastSeenAt: null,
    run: true,
    isSelf: false,
    ...(profiles ? { profiles } : {}),
  });
  const l: DevicesList = { self: 'me', devices: [dev('me', 'Desk', [{ profileId: 'pa', name: 'Me', personId: 'per-1' }]), dev('lap', 'Laptop', [{ profileId: 'qa', name: 'Me there', personId: 'per-1' }, { profileId: 'qb', name: 'Other', personId: 'per-2' }])] };
  const shape = (rows: { page: PinnedPage; device: { uid: string; name: string } | null }[]) => rows.map((r) => `${r.page.id}${r.device ? `@${r.device.name}` : ''}`);

  // A local pin, then the other PC's in one list; a second pin of the same path on that PC is
  // dropped (the store keys pins by profile and path, and two profiles of one person can still
  // pin the same file), and a pin of another person's profile is left out.
  const m1 = mergePages(
    [pin('l1', 'pa', 'C:/p/a.html')],
    [rem('lap', 'Laptop', [pin('r1', 'qa', 'H:/p/b.html'), pin('r2', 'qa', 'H:/p/b.html'), pin('r3', 'qb', 'H:/p/c.html')]), rem('tab', 'Tablet', [pin('t1', '', 'H:/p/d.html')])],
    l,
    'per-1',
  );
  assert.deepEqual(shape(m1.rows), ['l1', 'r1@Laptop', 't1@Tablet'], 'legacy pins without a profile stay');
  assert.deepEqual(m1.problems, []);

  // A PC that could not be read is one muted line, never an empty group.
  const m2 = mergePages([], [rem('lap', 'Laptop', [], { code: 'offline', message: 'Laptop is offline.' })], l, 'per-1');
  assert.deepEqual(m2.rows, []);
  assert.deepEqual(m2.problems.map((p) => `${p.name}: ${p.message}`), ['Laptop: Laptop is offline.']);

  // A PC this list does not know (an older peer whose hello named no profiles) keeps its pins; a
  // known PC that does name its profiles still filters another person's.
  const m3 = mergePages([], [rem('old', 'Desk 2', [pin('o1', 'zz', 'H:/p/e.html')]), rem('lap', 'Laptop', [pin('r3', 'qb', 'H:/p/c.html')])], l, 'per-1');
  assert.deepEqual(shape(m3.rows), ['o1@Desk 2'], 'an older peer that says nothing about profiles keeps its pins');
  const m4 = mergePages([], [rem('lap', 'Laptop', [pin('r3', 'qb', 'H:/p/c.html')])], l, null);
  assert.deepEqual(shape(m4.rows), ['r3@Laptop'], 'every profile: nothing is filtered');

  // Devices are ordered by name, regardless of the order they were answered in.
  const m5 = mergePages([], [rem('z', 'attic', [pin('a1', '', 'H:/a.html')]), rem('a', 'Desk PC', [pin('b1', '', 'H:/b.html')]), rem('m', 'Laptop', [pin('c1', '', 'H:/c.html')])], null, null);
  assert.deepEqual(shape(m5.rows), ['a1@attic', 'b1@Desk PC', 'c1@Laptop'], 'names order regardless of case, like the device menu');
});

test('the letter tile stands in when a page has no icon of its own', () => {
  const rows: Array<[string, string]> = [
    ['Plan', 'P'],
    ['  planting calendar', 'P'],
    ['Budget 2026', 'B'],
    ['', ''],
    ['   ', ''],
    ['éclair', 'É'],
    ['\u{1f680} launch', '\u{1f680}'],
  ];
  for (const [title, want] of rows) assert.equal(tileLetter(title), want, JSON.stringify(title));
});

test('a page from another PC keeps its own tab key and survives a restore only whole', () => {
  assert.equal(remoteTabKey('dev-1', 'pg-1'), 'remote:dev-1:pg-1');
  assert.notEqual(remoteTabKey('dev-1', 'pg-1'), remoteTabKey('dev-2', 'pg-1'));
  const rows: Array<[unknown, boolean]> = [
    [{ deviceUid: 'dev-1', pageId: 'pg-1', title: 'Plan' }, true],
    [{ deviceUid: '', pageId: 'pg-1', title: 'Plan' }, false],
    [{ deviceUid: 'dev-1', pageId: 'pg-1' }, false],
    [{ deviceUid: 'dev-1', pageId: 7, title: 'Plan' }, false],
    [{ deviceUid: 'dev-1', pageId: 'pg-1', title: 'a\u0000b' }, false],
    [{ deviceUid: 'x'.repeat(201), pageId: 'pg-1', title: 'Plan' }, false],
    ['dev-1', false],
    [null, false],
  ];
  for (const [r, want] of rows) assert.equal(validRemote(r), want, JSON.stringify(r));
  // A local page still needs a complete local path; a remote one does not.
  assert.equal(validPagePath('C:/pages/a.html'), true);
  assert.equal(validPagePath('/home/pages/a.html'), false);
});
