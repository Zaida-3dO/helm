import { test } from 'node:test';
import assert from 'node:assert/strict';
import { runningAgentKey, runningAgentSet } from './runningAgents.ts';
import { thread } from '../dev/fixtures.ts';
import type { Subagent } from '../api/generated/index.ts';

const agent = (toolCallId: string, status: Subagent['status']) => ({ toolCallId, status }) as Subagent;

test('only running subagents count, and the key does not depend on order', () => {
  const a = thread({ subagents: [agent('b', 'running'), agent('a', 'running'), agent('c', 'completed')] });
  const b = thread({ subagents: [agent('a', 'running'), agent('b', 'running')] });
  assert.equal(runningAgentKey(a), 'a,b');
  assert.equal(runningAgentKey(a), runningAgentKey(b));
  assert.deepEqual([...runningAgentSet(runningAgentKey(a))], ['a', 'b']);
});

test('no thread or no subagents is an empty set', () => {
  assert.equal(runningAgentKey(null), '');
  assert.equal(runningAgentSet('').size, 0);
  assert.equal(runningAgentKey(thread({})), '');
});
