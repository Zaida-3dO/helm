import { test } from 'node:test';
import assert from 'node:assert/strict';
import { changeLines, toolKind, toolLabel, toolStatus, toolTarget } from './toolKinds.ts';
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
