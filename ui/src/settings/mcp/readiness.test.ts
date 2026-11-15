import assert from 'node:assert/strict';
import { test } from 'node:test';
import type { McpPreset, McpServer } from '../../api/mcp.ts';
import { CONSOLE_STEPS, GOOGLE_PRESET, SIGN_IN_WINDOW_MS, blockState, clientLooksSet, watchState, type GoogleAccountList } from './google.ts';
import { canDisconnect, connectLabel } from './oauth.ts';
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
  assert.equal(canDisconnect({ connected: true }), true);
  assert.equal(canDisconnect({ connected: false }), false);
  assert.equal(canDisconnect(undefined), false);
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

test('the Google preset drafts a program, its row ignores the environment, and the dialog leads with what to fix', () => {
  const d = draftFromPreset(google, []);
  assert.equal(d.preset, GOOGLE_PRESET);
  assert.equal(d.transport.type, 'stdio');
  assert.equal(d.transport.command, 'uvx.exe');
  assert.deepEqual(d.transport.args?.slice(-2), ['--permissions', 'gmail:drafts'], 'new servers read and draft, never send');
  assert.deepEqual(toSaveable(d).transport.env, undefined, 'no environment secrets are drafted for the Google preset');
  // The sign-in client is one per profile, kept by the host: the row never asks for environment values.
  assert.equal(serverReadiness(toSaveable(d), undefined, [google]).state, 'ready');

  const list = (over: Partial<GoogleAccountList> = {}): GoogleAccountList => ({ accounts: [], uvFound: true, signingIn: false, ...over });
  assert.equal(blockState(null, null).kind, 'loading');
  assert.equal(blockState(list(), null).kind, 'loading', 'the client state is still unknown');
  const noUv = blockState(list({ uvFound: false }), false);
  assert.ok(noUv.kind === 'no-uv' && /Install uv/.test(noUv.message), 'uv missing comes first');
  assert.equal(blockState(list({ uvFound: false }), true).kind, 'no-uv');
  assert.equal(blockState(list(), false).kind, 'no-client');
  assert.equal(blockState(list(), true).kind, 'empty');
  assert.equal(blockState(list({ accounts: [{ email: 'a@x.com' }], permission: 'drafts' }), true).kind, 'ready');
});

test('the setup checklist is the six console steps, each with its console page', () => {
  assert.equal(CONSOLE_STEPS.length, 6);
  for (const s of CONSOLE_STEPS) {
    assert.ok(s.label.length > 0);
    assert.match(s.url, /^https:\/\/console\.cloud\.google\.com\//);
  }
  assert.match(CONSOLE_STEPS[0].url, /projectcreate/);
  assert.match(CONSOLE_STEPS[1].url, /gmail\.googleapis\.com/);
  assert.match(CONSOLE_STEPS[5].url, /clients\/create/);
});

test('the client form enables Save only once both values are filled', () => {
  assert.equal(clientLooksSet('id', 'sec'), true);
  assert.equal(clientLooksSet('  id  ', ' sec '), true, 'spaces are trimmed before the check');
  assert.equal(clientLooksSet('', 'sec'), false);
  assert.equal(clientLooksSet('id', '   '), false);
});

test('a started sign-in is watched until a new address appears or three minutes pass', () => {
  const w = { startedAt: 1_000, known: ['old@x.com'] };
  const none = [{ email: 'old@x.com' }];
  assert.equal(watchState(w, 1_000, []), 'waiting', 'no accounts is not a new one');
  assert.equal(watchState(w, 1_000, none), 'waiting', 'an address that was already there is not new');
  assert.equal(watchState(w, 1_000 + SIGN_IN_WINDOW_MS - 1, none), 'waiting');
  assert.equal(watchState(w, 1_000 + SIGN_IN_WINDOW_MS, none), 'timeout');
  assert.equal(watchState(w, 5_000, [...none, { email: 'NEW@gmail.com' }]), 'added', 'a new address is added');
  assert.equal(watchState(w, 5_000, [{ email: 'old@x.com' }, { email: 'OLD@X.COM' }]), 'waiting', 'case and repeats do not make an address new');
  assert.equal(watchState(w, 1_000 + SIGN_IN_WINDOW_MS * 2, [{ email: 'new@gmail.com' }]), 'added', 'an address that arrived in time is still added');
});
