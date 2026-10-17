import assert from 'node:assert/strict';
import { test } from 'node:test';
import type { McpPreset, McpServer } from '../../api/mcp.ts';
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
