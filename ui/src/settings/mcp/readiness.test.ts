import assert from 'node:assert/strict';
import { test } from 'node:test';
import type { McpPreset, McpServer } from '../../api/mcp.ts';
import { GOOGLE_PRESET, SIGN_IN_WINDOW_MS, blockState, clientSaved, emailLooksValid, watchState, type GoogleAccountList } from './google.ts';
import { connectLabel } from './oauth.ts';
import { blankServer, draftFromPreset, isPlainRemoteHttp, pageReadiness, serverReadiness, toSaveable } from './readiness.ts';

const hub: McpPreset = {
  id: 'helm_hub',
  name: 'Helm hub memory',
  description: '',
  transport: 'http',
  urlPlaceholder: 'http://your-hub.example/mcp',
  tokenHeader: 'Authorization',
  tokenPrefix: 'Bearer ',
  suggestPrivateHttp: true,
  expectTools: ['memory_search'],
  guidance: '',
};

test('readiness: off, native, token missing, failed, connected, idle', () => {
  const d = draftFromPreset(hub, []);
  d.transport.url = 'http://10.0.0.2/mcp';
  assert.equal(serverReadiness({ ...d, enabled: false }, undefined, [hub]).state, 'off');
  const native = serverReadiness(d, { serverId: d.id, state: 'native', at: 1 }, [hub]);
  assert.equal(native.state, 'inactive');
  assert.match(native.reason, /natively/);
  const noToken = serverReadiness(d, undefined, [hub]);
  assert.deepEqual([noToken.state, noToken.actionLabel], ['inactive', 'Add token']);
  const saved: McpServer = { ...d, transport: { ...d.transport, headers: { Authorization: { kind: 'literal', secretId: `lit_${'a'.repeat(32)}`, prefix: 'Bearer ' } } } };
  const failed = serverReadiness(saved, { serverId: d.id, state: 'failed', error: 'HTTP 401', at: 1 }, [hub]);
  assert.deepEqual([failed.state, failed.reason, failed.actionLabel], ['inactive', 'HTTP 401', 'Edit']);
  assert.equal(serverReadiness(saved, { serverId: d.id, state: 'connected', toolCount: 3, at: 1 }, [hub]).reason, 'Connected · 3 tools');
  assert.equal(serverReadiness(saved, undefined, [hub]).state, 'ready');
});

test('a browser-signed-in server asks for the browser, never for a token field', () => {
  const s: McpServer = { id: 'crm', name: 'CRM', enabled: true, allowPrivateHttp: false, transport: { type: 'http', url: 'https://crm.example/mcp', oauth: {} } };
  const noSignIn = serverReadiness(s, undefined, [hub]);
  assert.deepEqual([noSignIn.state, noSignIn.reason, noSignIn.actionLabel], ['inactive', 'Needs you to connect in your browser.', 'Connect']);
  // The host's own sign-in-needed report, once a sign-in existed: ask for it again.
  const stale = serverReadiness(s, { serverId: 'crm', state: 'signin_required', at: 1 }, [hub], { connected: true });
  assert.deepEqual([stale.state, stale.reason, stale.actionLabel], ['inactive', 'Needs you to sign in again.', 'Sign in again']);
  // Connected and signed in: the usual readiness lines take over.
  const ready = serverReadiness(s, { serverId: 'crm', state: 'connected', toolCount: 2, at: 1 }, [hub], { connected: true, expiresAt: 9_999_999_999 });
  assert.deepEqual([ready.state, ready.reason], ['ready', 'Connected · 2 tools']);
  // A signed-in server whose connection fails for another reason reads as a failure, not a sign-in ask.
  const down = serverReadiness(s, { serverId: 'crm', state: 'failed', error: 'unreachable', at: 1 }, [hub], { connected: true });
  assert.deepEqual([down.state, down.reason, down.actionLabel], ['inactive', 'unreachable', 'Edit']);
  // Off still wins, and a stdio server is never read as browser-signed-in.
  assert.equal(serverReadiness({ ...s, enabled: false }, undefined, [hub]).state, 'off');
  assert.equal(serverReadiness({ ...s, transport: { type: 'stdio', command: 'x' } }, undefined, [hub]).state, 'ready');
  // The row button follows the same rules.
  assert.equal(connectLabel(undefined, 'idle'), 'Connect');
  assert.equal(connectLabel({ connected: true }, 'signin_required'), 'Sign in again');
  assert.equal(connectLabel({ connected: true }, 'connected'), undefined);
});

test('the empty page names a reason and where to act', () => {
  const r = pageReadiness([]);
  assert.ok(r && r.reason.length > 0 && r.actionLabel);
  assert.equal(pageReadiness([blankServer([])]), null);
});

test('preset drafts never carry an address or token, and ids do not collide', () => {
  const d = draftFromPreset(hub, ['helm_hub']);
  assert.equal(d.id, 'helm_hub_2');
  assert.equal(d.transport.url, '');
  assert.equal(d.transport.headers?.Authorization?.value, '');
  assert.equal(d.allowPrivateHttp, false, 'the opt-in is never pre-ticked');
});

test('toSaveable keeps only the chosen transport and drops blank entries', () => {
  const d = draftFromPreset(hub, []);
  d.transport.url = ' http://10.0.0.2/mcp ';
  d.allowPrivateHttp = true;
  const s = toSaveable(d);
  assert.equal(s.transport.url, 'http://10.0.0.2/mcp');
  assert.equal(s.transport.headers, undefined, 'an empty new token is left out');
  d.transport.headers = { Authorization: { kind: 'literal', value: 'tok', prefix: 'Bearer ' }, ' ': { kind: 'environment', value: 'X' } };
  assert.deepEqual(toSaveable(d).transport.headers, { Authorization: { kind: 'literal', value: 'tok', prefix: 'Bearer ' } });
  d.transport.headers['X-Helm-Person'] = { kind: 'chatPerson' };
  assert.deepEqual(toSaveable(d).transport.headers?.['X-Helm-Person'], { kind: 'chatPerson' }, "the chat's-person header survives a save");
  const stdio = toSaveable({ ...d, transport: { ...d.transport, type: 'stdio', command: 'node.exe ' } });
  assert.deepEqual(stdio.transport, { type: 'stdio', command: 'node.exe' });
  assert.equal(stdio.allowPrivateHttp, false);
});

test('the unencrypted warning shows for plain http off this computer only', () => {
  assert.equal(isPlainRemoteHttp('http://10.0.0.2/mcp'), true);
  assert.equal(isPlainRemoteHttp('http://hub.lan/mcp'), true);
  assert.equal(isPlainRemoteHttp('http://127.0.0.1:5/mcp'), false);
  assert.equal(isPlainRemoteHttp('http://localhost/mcp'), false);
  assert.equal(isPlainRemoteHttp('https://x.example/mcp'), false);
});

const google: McpPreset = {
  id: GOOGLE_PRESET,
  name: 'Google accounts (Gmail)',
  description: '',
  transport: 'stdio',
  urlPlaceholder: '',
  tokenHeader: '',
  tokenPrefix: '',
  suggestPrivateHttp: false,
  expectTools: [],
  guidance: '',
  command: 'uvx.exe',
  args: ['workspace-mcp', '--tools', 'gmail', '--permissions', 'gmail:drafts'],
  secretEnv: ['GOOGLE_OAUTH_CLIENT_ID', 'GOOGLE_OAUTH_CLIENT_SECRET'],
};

test('the Google preset drafts a program that needs its client, and the accounts block leads with what to fix', () => {
  const d = draftFromPreset(google, []);
  assert.equal(d.preset, GOOGLE_PRESET);
  assert.equal(d.transport.type, 'stdio');
  assert.equal(d.transport.command, 'uvx.exe');
  assert.deepEqual(d.transport.args?.slice(-2), ['--permissions', 'gmail:drafts'], 'new servers read and draft, never send');
  assert.deepEqual(Object.keys(d.transport.env ?? {}), ['GOOGLE_OAUTH_CLIENT_ID', 'GOOGLE_OAUTH_CLIENT_SECRET']);
  assert.equal(toSaveable(d).transport.env, undefined, 'empty secrets are left out, never saved blank');
  assert.equal(clientSaved(d), false);

  const need = serverReadiness(toSaveable(d), undefined, [google]);
  assert.deepEqual([need.state, need.reason, need.actionLabel], ['inactive', 'Needs its client id and secret.', 'Edit']);
  const secret = { kind: 'literal' as const, secretId: `lit_${'a'.repeat(32)}` };
  const saved: McpServer = { ...d, transport: { ...d.transport, env: { GOOGLE_OAUTH_CLIENT_ID: secret, GOOGLE_OAUTH_CLIENT_SECRET: secret } } };
  assert.equal(clientSaved(saved), true);
  assert.equal(serverReadiness(saved, undefined, [google]).state, 'ready');

  const list = (over: Partial<GoogleAccountList> = {}): GoogleAccountList => ({ accounts: [], uvFound: true, signingIn: false, ...over });
  assert.equal(blockState(saved, null).kind, 'loading');
  const noUv = blockState(saved, list({ uvFound: false }));
  assert.ok(noUv.kind === 'no-uv' && /Install uv/.test(noUv.message), 'uv missing comes first');
  assert.equal(blockState(d, list({ uvFound: false })).kind, 'no-uv');
  assert.equal(blockState(d, list()).kind, 'no-client');
  assert.equal(blockState(saved, list()).kind, 'empty');
  assert.equal(blockState(saved, list({ accounts: [{ email: 'a@x.com' }], permission: 'drafts' })).kind, 'ready');
});

test('a started sign-in is watched until the address appears or three minutes pass', () => {
  const w = { startedAt: 1_000, email: ' A@X.com ' };
  const none: { email: string }[] = [];
  assert.equal(watchState(w, 1_000, none), 'waiting');
  assert.equal(watchState(w, 1_000 + SIGN_IN_WINDOW_MS - 1, [{ email: 'b@y.org' }]), 'waiting');
  assert.equal(watchState(w, 1_000 + SIGN_IN_WINDOW_MS, none), 'timeout');
  assert.equal(watchState(w, 5_000, [{ email: 'b@y.org' }, { email: 'a@x.com' }]), 'added', 'case and spaces do not matter');
  assert.equal(watchState(w, 1_000 + SIGN_IN_WINDOW_MS * 2, [{ email: 'a@x.com' }]), 'added', 'an address that arrived in time is still added');
});

test('addresses are checked loosely before the host checks them', () => {
  for (const [s, ok] of [
    ['a@x.com', true],
    [' first.last+tag@mail.example.org ', true],
    ['', false],
    ['a@x', false],
    ['a b@x.com', false],
    ['@x.com', false],
    ['a@@x.com', false],
  ] as const) {
    assert.equal(emailLooksValid(s), ok, s);
  }
});
