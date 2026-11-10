import { test } from 'node:test';
import assert from 'node:assert/strict';
import { allowChipState, allowRequestId, changeLines, hostIconFor, readable, splitPath, toolIdentity, toolKind, toolLabel, toolStatus, toolTarget, toolVerb } from './toolKinds.ts';
import { row } from '../dev/fixtures.ts';

test('tool kinds and MCP labels', () => {
  assert.equal(toolKind('Bash'), 'shell');
  assert.equal(toolKind('Grep'), 'read');
  assert.equal(toolKind('MultiEdit'), 'edit');
  assert.equal(toolKind('WebFetch'), 'web');
  assert.equal(toolKind('mcp__github__get_pr'), 'mcp');
  assert.equal(toolKind('Frobnicate'), 'generic');
  assert.equal(toolLabel('mcp__github__get_pr'), 'github · get_pr');
});

test('the target comes from the JSON args, even when the host cut them short', () => {
  const whole = row({ role: 'toolResult', details: { toolArgs: JSON.stringify({ command: 'npm test' }) } });
  assert.equal(toolTarget(whole), 'npm test');
  const cut = row({ role: 'toolResult', details: { toolArgs: '{"file_path":"src/a.ts","content":"lots of te' } });
  assert.equal(toolTarget(cut), 'src/a.ts');
});

test('status and change lines', () => {
  assert.equal(toolStatus(row({ role: 'toolResult', status: 'streaming' })), 'running');
  assert.equal(toolStatus(row({ role: 'toolResult', details: { isError: true } })), 'failed');
  const edit = row({ role: 'toolResult', details: { toolArgs: JSON.stringify({ old_string: 'a', new_string: 'b\nc' }) } });
  assert.deepEqual(changeLines(edit).lines.map((l) => l.sign + l.text), ['-a', '+b', '+c']);
});

test('tool identity comes from toolSource, falling back to the MCP name', () => {
  const host = row({ role: 'toolResult', toolName: 'mcp__helm__app_settings_get', details: { toolSource: { kind: 'host', server: 'Helm', tool: 'app_settings_get' } } });
  assert.deepEqual(toolIdentity(host), { source: 'host', server: 'Helm', tool: 'app_settings_get' });
  const mcp = row({ role: 'toolResult', toolName: 'mcp__ha__turn_on', details: { toolSource: { kind: 'mcp', server: 'Home Assistant', tool: 'turn_on' } } });
  assert.equal(toolIdentity(mcp).server, 'Home Assistant');
  // No toolSource: the name decides.
  assert.equal(toolIdentity(row({ role: 'toolResult', toolName: 'mcp__helm__app_chat_list' })).source, 'host');
  assert.equal(toolIdentity(row({ role: 'toolResult', toolName: 'mcp__ha__turn_on' })).source, 'mcp');
  assert.equal(toolIdentity(row({ role: 'toolResult', toolName: 'Bash' })).source, 'native');
});

test('verbs follow the status; failures and stops override', () => {
  const bash = row({ role: 'toolResult', toolName: 'Bash', status: 'streaming' });
  assert.equal(toolVerb(bash), 'Running');
  assert.equal(toolVerb(row({ role: 'toolResult', toolName: 'Bash' })), 'Ran');
  assert.equal(toolVerb(row({ role: 'toolResult', toolName: 'WebSearch' })), 'Searched web');
  assert.equal(toolVerb(row({ role: 'toolResult', toolName: 'Bash', details: { isError: true } })), 'Failed');
  assert.equal(toolVerb(row({ role: 'toolResult', toolName: 'Bash', status: 'interrupted' })), 'Stopped');
  assert.equal(toolVerb(row({ role: 'toolResult', toolName: 'Frobnicate', status: 'streaming' })), 'Running Frobnicate');
  assert.equal(toolVerb(row({ role: 'toolResult', toolName: 'Frobnicate' })), 'Frobnicate');
});

test('readable names, host icons and path splitting', () => {
  assert.equal(readable('app_settings_get'), 'Settings get');
  assert.equal(hostIconFor('app_schedule_list'), 'clock');
  assert.equal(hostIconFor('app_unknown_thing'), 'wrench');
  assert.deepEqual(splitPath('src/a/b.ts'), { name: 'b.ts', dir: 'src/a/' });
  assert.deepEqual(splitPath('C:' + String.fromCharCode(92) + 'x' + String.fromCharCode(92) + 'y.md'), { name: 'y.md', dir: 'C:' + String.fromCharCode(92) + 'x' + String.fromCharCode(92) });
});

test('an Allow request call names its request, and the chip reads waiting, allowed or refused', () => {
  const call = (text: string, tool = 'app_request_allow') => row({ role: 'toolResult', toolName: `mcp__helm__${tool}`, text, details: { toolSource: { kind: 'host', server: 'Helm', tool } } });
  assert.equal(allowRequestId(call('{"requestId":"r1","state":"pending"}')), 'r1');
  assert.equal(allowRequestId(call('{"requestId":"r2","state":"pending","summary":"cut off...')), 'r2');
  assert.equal(allowRequestId(call('nothing here')), null);
  assert.equal(allowRequestId(call('{"requestId":"r1"}', 'app_workflow')), null, 'only the Allow request tool');
  assert.equal(allowRequestId(row({ role: 'toolResult', toolName: 'Bash', text: '{"requestId":"r1"}' })), null);
  const want: Record<string, string | null> = { pending: 'waiting', allowed: 'allowed', used: 'allowed', denied: 'refused', expired: 'refused', other: null };
  for (const [state, chip] of Object.entries(want)) assert.equal(allowChipState(state), chip, state);
  assert.equal(allowChipState(undefined), null);
});

