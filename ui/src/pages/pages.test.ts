import assert from 'node:assert/strict';
import { test } from 'node:test';
import { PageBridge } from './bridge.ts';
import {
  checkHandoff,
  checkLink,
  checkState,
  composeHandoff,
  handoffTitle,
  MARK,
  MAX_PENDING,
  PAGE_SANDBOX,
  parseMessage,
} from './protocol.ts';
import { buildTheme, plainValue, THEME_VARIABLES } from './theme.ts';

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
    { text: 'hi', data: { big: 'x'.repeat(70_000) } },
    { text: 'hi', data: 1n },
  ];
  for (const p of bad) assert.equal(checkHandoff(p).ok, false, JSON.stringify(p, (_, v) => (typeof v === 'bigint' ? 'big' : v))?.slice(0, 60));

  const ok = checkHandoff({ text: '  Approve step 2  ', title: ' Review ', data: { step: 2 } });
  assert.ok(ok.ok);
  assert.deepEqual(ok.ok && ok.value, { text: 'Approve step 2', title: 'Review', data: { step: 2 } });
  const text = composeHandoff(ok.ok ? ok.value : { text: '' }, 'Tracker');
  assert.ok(text.startsWith('Approve step 2\n\n[Sent from the page "Tracker" in Helm'));
  assert.ok(text.includes('not the user'), 'it never reads as the user’s own words');
  assert.ok(text.endsWith('```json\n{\n  "step": 2\n}\n```'));
  assert.equal(composeHandoff({ text: 'x' }, 'a "quoted"\nname').split('\n').length, 3, 'a page cannot break the marker line');
  assert.equal(handoffTitle({ text: 'x' }, 'Tracker'), 'From Tracker');
  assert.equal(handoffTitle({ text: 'x', title: 'Mine' }, 'Tracker'), 'Mine');
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

test('the sandbox gives a page scripts and downloads and nothing that could reach Helm', () => {
  assert.deepEqual(PAGE_SANDBOX.split(' ').sort(), ['allow-downloads', 'allow-scripts']);
  for (const never of ['allow-same-origin', 'allow-top-navigation', 'allow-popups', 'allow-forms', 'allow-modals']) {
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
