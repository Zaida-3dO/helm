import assert from 'node:assert/strict';
import { test } from 'node:test';
import type { ChatMcpProblem, McpPreset, McpServer } from '../../api/mcp.ts';
import { CALENDAR_HELP, CALENDAR_OPTIONS, CONSOLE_STEPS, GOOGLE_PRESET, SIGN_IN_WINDOW_MS, blockState, calendarInArgs, clientLooksSet, watchState, type GoogleAccountList } from './google.ts';
import { canDisconnect, connectLabel } from './oauth.ts';
import { BROKER_PRESET, blankServer, brokerMissing, chatMcpNoticeText, copyImportable, draftFromPreset, isBrokerServer, isPlainRemoteHttp, pageReadiness, renameBrokerBrowser, serverReadiness, toSaveable, withProfileValue } from './readiness.ts';

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

test('the browser preset starts at the broker entry with its variables filled in as rows', () => {
  const broker: McpPreset = {
    id: BROKER_PRESET,
    name: 'My browser',
    description: '',
    transport: 'stdio',
    urlPlaceholder: '',
    tokenHeader: '',
    tokenPrefix: '',
    suggestPrivateHttp: false,
    expectTools: ['browser_act'],
    guidance: '',
    command: 'node.exe',
    args: ['C:\\bb\\dist\\src\\bin\\broker-tool.js'],
    secretEnv: ['BROKER_ATTACHED_TOKEN'],
    draftEnv: ['BROKER_ATTACHED_BROWSERS', 'BROKER_BROWSER_BRAVE_PATH', 'BROKER_ATTACHED_EXTENSION_ID', 'BROKER_ATTACHED_ACT_SITES', 'BROKER_ATTACHED_TOKEN'],
  };
  const d = draftFromPreset(broker, []);
  assert.equal(d.id, BROKER_PRESET);
  assert.equal(d.preset, BROKER_PRESET);
  assert.equal(d.transport.command, 'node.exe', 'node directly, never a package runner');
  assert.ok(d.transport.args?.[0]?.endsWith('broker-tool.js'));
  assert.deepEqual(
    Object.keys(d.transport.env ?? {}).sort(),
    ['BROKER_ATTACHED_ACT_SITES', 'BROKER_ATTACHED_BROWSERS', 'BROKER_ATTACHED_EXTENSION_ID', 'BROKER_ATTACHED_TOKEN', 'BROKER_BROWSER_BRAVE_PATH'],
    'each variable the broker reads starts as a row (the token once, not twice)',
  );
  assert.equal(d.transport.env?.BROKER_ATTACHED_BROWSERS?.value, 'brave', 'the browser name is filled in with the default from the start');
  const r = serverReadiness(d, undefined, [broker]);
  assert.deepEqual([r.state, r.reason, r.actionLabel], ['inactive', "Needs your browser's extension id.", 'Edit']);
  // Everything a broker server must have, and no token: the token is optional (it only skips
  // the connect page on reconnect), so a server without one reads as ready.
  const env = (over: Record<string, { kind: 'literal'; value: string }> = {}): McpServer['transport']['env'] => ({
    BROKER_ATTACHED_BROWSERS: { kind: 'literal', value: 'brave' },
    BROKER_BROWSER_BRAVE_PATH: { kind: 'literal', value: 'C:\\Program Files\\BraveSoftware\\Brave-Browser\\Application\\brave.exe' },
    BROKER_ATTACHED_EXTENSION_ID: { kind: 'literal', value: 'a'.repeat(32) },
    BROKER_ATTACHED_ACT_SITES: { kind: 'literal', value: 'shop.example' },
    ...over,
  });
  const filled: McpServer = { ...d, transport: { ...d.transport, env: env() } };
  assert.equal(brokerMissing(filled), null);
  assert.equal(serverReadiness(filled, undefined, [broker]).state, 'ready', 'only the optional token is missing');
  // The reason line names whichever of the three the form has left empty.
  assert.equal(brokerMissing({ ...filled, transport: { ...filled.transport, env: env({ BROKER_ATTACHED_BROWSERS: { kind: 'literal', value: '' } }) } }), "Needs your browser's name.");
  assert.equal(brokerMissing({ ...filled, transport: { ...filled.transport, env: env({ BROKER_ATTACHED_EXTENSION_ID: { kind: 'literal', value: '' } }) } }), "Needs your browser's extension id.");
  assert.equal(brokerMissing({ ...filled, transport: { ...filled.transport, env: env({ BROKER_BROWSER_BRAVE_PATH: { kind: 'literal', value: '' } }) } }), "Needs your browser's program path.");
  // Renaming after a reload: the saved name came back sealed (no value), so the one program row
  // is the one that follows the new name; a typed name moves its own row; a half-typed bad name
  // moves nothing.
  const sealed = { BROKER_ATTACHED_BROWSERS: { kind: 'literal', secretId: 's1' }, BROKER_BROWSER_OLD_PATH: { kind: 'literal', secretId: 's2' } } as McpServer['transport']['env'];
  const moved = renameBrokerBrowser(sealed!, '', 'b');
  assert.deepEqual(Object.keys(moved).sort(), ['BROKER_ATTACHED_BROWSERS', 'BROKER_BROWSER_B_PATH']);
  assert.equal(moved.BROKER_BROWSER_B_PATH?.secretId, 's2', 'the saved path value travels with the row');
  assert.deepEqual(Object.keys(renameBrokerBrowser(env()!, 'brave', 'my-edge')).sort(), ['BROKER_ATTACHED_ACT_SITES', 'BROKER_ATTACHED_BROWSERS', 'BROKER_ATTACHED_EXTENSION_ID', 'BROKER_BROWSER_MY_EDGE_PATH']);
  assert.ok('BROKER_BROWSER_BRAVE_PATH' in renameBrokerBrowser(env()!, 'brave', 'Not A Name'), 'a name that is not a browser name moves no row');
  // The doctor row belongs to the browser server only.
  assert.ok(isBrokerServer(d));
  assert.ok(!isBrokerServer(draftFromPreset(hub, [])));
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

test('a browser sign-in save keeps the marker and only the client values that are filled', () => {
  const base: McpServer = { id: 'crm', name: 'CRM', enabled: true, allowPrivateHttp: false, transport: { type: 'http', url: 'https://crm.example/mcp' } };
  assert.equal(toSaveable(base).transport.oauth, undefined, 'no browser sign-in unless asked for');
  const marker = toSaveable({ ...base, transport: { ...base.transport, oauth: {} } });
  assert.deepEqual(marker.transport.oauth, {}, 'the empty marker survives: register automatically');
  const withClient = toSaveable({
    ...base,
    transport: { ...base.transport, oauth: { clientId: ' cid ', scope: '  ', clientSecret: { kind: 'literal', value: '  ' } } },
  });
  assert.deepEqual(withClient.transport.oauth, { clientId: 'cid' }, 'blank client parts drop away');
  const kept = toSaveable({ ...base, transport: { ...base.transport, oauth: { clientId: 'cid', clientSecret: { kind: 'literal', secretId: `lit_${'b'.repeat(32)}` } } } });
  assert.deepEqual(kept.transport.oauth, { clientId: 'cid', clientSecret: { kind: 'literal', secretId: `lit_${'b'.repeat(32)}` } }, 'a saved secret rides back by id, never re-sent');
  // The box keeps the saved id while it is edited: a typed value replaces the saved secret, and a
  // box typed into and cleared again keeps it.
  const secretId = `lit_${'b'.repeat(32)}`;
  const withSecret = (value: string) => toSaveable({ ...base, transport: { ...base.transport, oauth: { clientSecret: { kind: 'literal', value, secretId } } } }).transport.oauth;
  assert.deepEqual(withSecret('typed'), { clientSecret: { kind: 'literal', value: 'typed' } }, 'a typed value replaces the saved one');
  assert.deepEqual(withSecret(''), { clientSecret: { kind: 'literal', secretId } }, 'a cleared box keeps the saved one');
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
  // The sign-in client is one per profile, kept by the host: the row never asks for environment
  // values, and it says Helm serves the server itself — no process, no connection.
  const saved = toSaveable(d);
  const plain = serverReadiness(saved, undefined, [google]);
  assert.deepEqual([plain.state, plain.reason], ['ready', 'Built in: served by Helm.']);
  // Only a server that carries its own client env keeps the bridge program (and its connection story).
  const legacy: McpServer = {
    ...saved,
    transport: {
      ...saved.transport,
      env: {
        GOOGLE_OAUTH_CLIENT_ID: { kind: 'environment', value: 'MY_ID' },
        GOOGLE_OAUTH_CLIENT_SECRET: { kind: 'environment', value: 'MY_SECRET' },
      },
    },
  };
  assert.match(serverReadiness(legacy, undefined, [google]).reason, /connects when a chat/, 'a bridge server keeps the usual row');
  assert.equal(serverReadiness(legacy, { serverId: saved.id, state: 'failed', error: 'uvx missing', at: 1 }, [google]).reason, 'uvx missing');
  assert.equal(serverReadiness({ ...legacy, enabled: false }, undefined, [google]).state, 'off', 'off still wins');
  // A calendar level in the arguments means a bridge runs for the calendar tools, so the row
  // follows its connection like any program server's; only no level (or off) stays built in.
  const withCal: McpServer = { ...saved, transport: { ...saved.transport, args: [...(saved.transport.args ?? []), 'calendar:readonly'] } };
  assert.equal(calendarInArgs(withCal), 'readonly');
  assert.equal(calendarInArgs(saved), 'off');
  assert.equal(calendarInArgs({ ...saved, transport: { ...saved.transport, args: ['workspace-mcp', '--permissions=calendar:full'] } }), 'edit_events');
  assert.equal(calendarInArgs({ ...saved, transport: { ...saved.transport, args: ['workspace-mcp', '--permissions', 'calendar:organize'] } }), null, 'a level the picker does not know is not guessed');
  assert.match(serverReadiness(withCal, undefined, [google]).reason, /connects when a chat/, 'a calendar server keeps the usual row');
  assert.equal(serverReadiness(withCal, { serverId: saved.id, state: 'connected', toolCount: 12, at: 1 }, [google]).reason, 'Connected · 12 tools');
  // The picker offers exactly the levels the host maps, each with its help line.
  assert.deepEqual(CALENDAR_OPTIONS.map((o) => o.value), ['off', 'readonly', 'edit_events']);
  for (const o of CALENDAR_OPTIONS) assert.ok(CALENDAR_HELP[o.value].length > 0, o.value);

  const list = (over: Partial<GoogleAccountList> = {}): GoogleAccountList => ({ accounts: [], uvFound: true, signingIn: false, hostServed: false, ...over });
  assert.equal(blockState(null, null).kind, 'loading');
  assert.equal(blockState(list(), null).kind, 'loading', 'the client state is still unknown');
  const noUv = blockState(list({ uvFound: false }), false);
  assert.ok(noUv.kind === 'no-uv' && /Install uv/.test(noUv.message), 'uv missing comes first for a bridge server');
  assert.equal(blockState(list({ uvFound: false }), true).kind, 'no-uv');
  assert.equal(blockState(list({ uvFound: false, hostServed: true }), false).kind, 'no-client', 'a host-served server needs no uv');
  assert.equal(blockState(list(), false).kind, 'no-client');
  assert.equal(blockState(list(), true).kind, 'empty');
  assert.equal(blockState(list({ accounts: [{ email: 'a@x.com', needsSignIn: [] }], permission: 'drafts' }), true).kind, 'ready');
});

test('the setup checklist is the six console steps, each with its console page', () => {
  assert.equal(CONSOLE_STEPS.length, 6);
  const urls = CONSOLE_STEPS.map((s) => {
    assert.ok(s.label.length > 0);
    assert.match(s.url, /^https:\/\/console\.cloud\.google\.com\//);
    return s.url;
  });
  assert.match(urls.join(','), /projectcreate/);
  assert.match(urls.join(','), /gmail\.googleapis\.com/);
  assert.match(urls[urls.length - 1] ?? '', /clients\/create/, 'the client step is last');
});

test('the client form enables Save only once both values are filled', () => {
  assert.equal(clientLooksSet('id', 'sec'), true);
  assert.equal(clientLooksSet('  id  ', ' sec '), true, 'spaces are trimmed before the check');
  assert.equal(clientLooksSet('', 'sec'), false);
  assert.equal(clientLooksSet('id', '   '), false);
});

test('a started sign-in is watched until a new address arrives, the host lets go, or the window ends', () => {
  const w = { startedAt: 1_000, known: ['old@x.com'], sawSigningIn: true };
  const old = { email: 'old@x.com', needsSignIn: [] };
  const watch = (over: Partial<GoogleAccountList> = {}): GoogleAccountList => ({ accounts: [old], uvFound: true, signingIn: true, hostServed: true, ...over });
  assert.equal(watchState(w, 1_000, watch({ accounts: [] })), 'waiting', 'no accounts is not a new one');
  assert.equal(watchState(w, 1_000, watch()), 'waiting', 'an address that was already there is not new');
  assert.equal(watchState(w, 1_000 + SIGN_IN_WINDOW_MS - 1, watch()), 'waiting');
  assert.equal(watchState(w, 1_000 + SIGN_IN_WINDOW_MS, watch()), 'timeout', 'the window matches the host listener (5 min)');
  assert.equal(watchState(w, 5_000, watch({ accounts: [old, { email: 'NEW@gmail.com', needsSignIn: [] }] })), 'added', 'a new address is added');
  assert.equal(watchState(w, 5_000, watch({ accounts: [old, { email: 'OLD@X.COM', needsSignIn: [] }] })), 'waiting', 'case and repeats do not make an address new');
  assert.equal(watchState(w, 1_000 + SIGN_IN_WINDOW_MS * 2, watch({ accounts: [{ email: 'new@gmail.com', needsSignIn: [] }] })), 'added', 'an address that arrived in time is still added');
  // Cancel or Deny at Google ends the host's listener: the wait ends with it, not at the window.
  assert.equal(watchState(w, 5_000, watch({ signingIn: false })), 'refused', 'the listener let go with no new address');
  // A watch that never saw the listener open (a host restart mid-wait) still ends at the window.
  assert.equal(watchState({ startedAt: 1_000, known: [], sawSigningIn: false }, 1_000, watch({ signingIn: false, accounts: [] })), 'waiting', 'the first quiet poll is not yet a refusal');
  assert.equal(watchState({ startedAt: 1_000, known: [], sawSigningIn: false }, 1_000 + SIGN_IN_WINDOW_MS, watch({ signingIn: false, accounts: [] })), 'timeout');
  // Signing an account in again ends with its mark gone (renewed), or still there (refused:
  // cancelled, or some boxes left unticked). While the host's listener is open it keeps waiting.
  const again = { ...w, again: 'Old@X.com' };
  const marked = { email: 'old@x.com', needsSignIn: ['calendar'] };
  assert.equal(watchState(again, 5_000, watch({ accounts: [marked] })), 'waiting', 'still open');
  assert.equal(watchState(again, 5_000, watch({ accounts: [old], signingIn: false })), 'renewed', 'the mark is gone');
  assert.equal(watchState(again, 5_000, watch({ accounts: [marked], signingIn: false })), 'refused', 'still marked');
  assert.equal(watchState(w, 5_000, watch({ accounts: [marked], signingIn: false })), 'refused', 'a plain add never reads as renewed');
});

const problem = (name: string, state: ChatMcpProblem['state'], reason: string): ChatMcpProblem => ({
  serverId: name.toLowerCase().replace(/\s+/g, '_'),
  name,
  state,
  reason,
});

test('the chat notice names the broken server, keeps the reason words, and points at Settings', () => {
  assert.equal(chatMcpNoticeText([]), null, 'nothing wrong, nothing said');
  assert.equal(
    chatMcpNoticeText([problem('Local files', 'failed', 'Connection refused')]),
    'Local files is not connected on this PC: Connection refused. Fix it in Settings → MCP.',
  );
  // The host's words already end in a full stop: no double period.
  assert.equal(
    chatMcpNoticeText([problem('Example CRM', 'signin_required', 'Needs you to sign in.')]),
    'Example CRM is not connected on this PC: Needs you to sign in. Fix it in Settings → MCP.',
  );
  // More than one broken server: still one line, carried by the first name.
  assert.equal(
    chatMcpNoticeText([problem('Local files', 'failed', 'The last connection failed.'), problem('Example CRM', 'signin_required', 'Needs you to sign in.')]),
    'Local files and 1 more are not connected on this PC: The last connection failed. Fix it in Settings → MCP.',
  );
});

test('a copied server keeps its shape but not its secret, and an id already kept is skipped', () => {
  const remote: McpServer[] = [
    {
      id: 'agent_standup',
      name: 'Agent Standup',
      enabled: true,
      preset: 'agent_standup',
      allowPrivateHttp: false,
      transport: {
        type: 'http',
        url: 'https://board.example/mcp',
        headers: {
          Authorization: { kind: 'literal', value: 'never-send', secretId: `lit_${'a'.repeat(32)}`, prefix: 'Bearer ' },
          'X-Person': { kind: 'chatPerson' },
        },
      },
    },
    {
      id: 'files',
      name: 'Files',
      enabled: true,
      allowPrivateHttp: false,
      transport: {
        type: 'stdio',
        command: 'node.exe',
        env: {
          KEEP: { kind: 'environment', value: 'KEEP' },
          TOKPATH: { kind: 'file', value: 'C:/secrets/tok.txt' },
        },
      },
    },
    {
      id: 'crm',
      name: 'Example CRM',
      enabled: true,
      allowPrivateHttp: false,
      transport: { type: 'http', url: 'https://crm.example/mcp' },
    },
  ];
  const out = copyImportable(remote, ['crm']);
  assert.deepEqual(
    out.map((s) => s.id),
    ['agent_standup', 'files'],
    'an id this PC already has is not offered, never overwritten',
  );
  assert.deepEqual(out[0]!.transport.headers?.Authorization, { kind: 'literal', prefix: 'Bearer ' }, 'the token crosses as an empty slot, with its prefix');
  assert.deepEqual(out[0]!.transport.headers?.['X-Person'], { kind: 'chatPerson' }, 'the chat-person header carries as it stands');
  assert.deepEqual(out[1]!.transport.env?.KEEP, { kind: 'environment', value: 'KEEP' }, 'the variable name carries');
  assert.deepEqual(out[1]!.transport.env?.TOKPATH, { kind: 'literal' }, 'a credential file on that PC crosses as an empty secret slot');
});

test('a secret can have its own value per profile: typed, kept by id, saved, never blank, never copied', () => {
  const id = `lit_${'b'.repeat(32)}`;
  const shared: McpServer['transport']['headers'] = { Authorization: { kind: 'literal', secretId: `lit_${'a'.repeat(32)}`, prefix: 'Bearer ' } };
  let ref = withProfileValue(shared!.Authorization!, 'pa', 'typed');
  ref = withProfileValue({ ...ref, profiles: { ...ref.profiles, pb: { secretId: id } } }, 'pc', '');
  const draft = { ...blankServer([]), transport: { type: 'http' as const, url: 'https://x.example/mcp', headers: { Authorization: ref } } };
  assert.deepEqual(
    toSaveable(draft).transport.headers?.Authorization?.profiles,
    { pa: { value: 'typed' }, pb: { secretId: id } },
    'a typed value is sent, a saved one rides back as its id, an empty entry is dropped',
  );
  const only = { ...draft, transport: { ...draft.transport, headers: { Authorization: { kind: 'literal' as const, profiles: { pa: { value: 'x' } } } } } };
  assert.deepEqual(toSaveable(only).transport.headers?.Authorization, { kind: 'literal', profiles: { pa: { value: 'x' } } }, 'profile values alone keep the secret');
  assert.deepEqual(withProfileValue(ref, 'pa', null).profiles?.pa, undefined, 'removing one leaves the rest');
  assert.equal(withProfileValue(withProfileValue({ kind: 'literal' }, 'pa', 'x'), 'pa', null).profiles, undefined, 'removing the last drops the map');
  assert.deepEqual(copyImportable([{ ...draft, id: 'other' }], [])[0]!.transport.headers?.Authorization, { kind: 'literal', prefix: 'Bearer ' }, 'a copy to another PC carries none of it');
});
