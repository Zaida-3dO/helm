import { test } from 'node:test';
import assert from 'node:assert/strict';
import type { Subagent } from '../../api/generated/index.ts';
import type { MessageRow } from '../../api/types.ts';
import { EventBus } from '../../api/events.ts';
import { row, thread } from '../../dev/fixtures.ts';
import { SubagentCounts, countsFor } from './counts.ts';
import { chipLabel, elapsedMs, formatTokens, isAgentRow, previewLine, reportPreview, rosterOrder, stepLine, subagentFor, titleOf } from './model.ts';

function agent(p: Partial<Subagent> = {}): Subagent {
  return {
    toolCallId: 'sa_1',
    agentType: 'general-purpose',
    name: 'Echo alpha',
    description: 'Echo alpha',
    assignment: 'Run echo alpha.\nReport it.',
    status: 'running',
    stepTool: null,
    step: null,
    startedAt: 1_000,
    endedAt: null,
    durationMs: null,
    tokens: null,
    toolUses: null,
    error: null,
    report: null,
    reportTruncated: false,
    ...p,
  };
}

const agentRow = (p: Partial<MessageRow> = {}) => row({ toolName: 'Agent', toolCallId: 'sa_1', ...p, role: p.role ?? 'toolResult' });

test('only Agent and Task tool rows carry a subagent', () => {
  assert.equal(isAgentRow(agentRow()), true);
  assert.equal(isAgentRow(agentRow({ toolName: 'Task' })), true);
  assert.equal(isAgentRow(agentRow({ toolName: 'Bash' })), false);
  assert.equal(isAgentRow(row({ role: 'assistant', toolName: 'Agent' })), false);
});

test('a running agent comes from the thread; a settled one prefers the saved row', () => {
  const live = agent({ step: 'echo alpha', stepTool: 'Bash' });
  const t = thread({ subagents: [live], subagentsRunning: 1 });
  assert.equal(subagentFor(agentRow(), t), live);
  assert.equal(subagentFor(agentRow({ toolCallId: 'other' }), t), null);
  // While it runs, the live state wins over any copy on the row.
  assert.equal(subagentFor(agentRow({ details: { subagent: agent({ status: 'stopped' }) } }), t), live);
  // Settled: the row's copy has the longer report and survives a restart.
  const saved = agent({ status: 'completed', report: 'x'.repeat(5000) });
  const settledLive = agent({ status: 'completed', report: 'x'.repeat(2048), reportTruncated: true });
  const withSaved = agentRow({ details: { subagent: saved } });
  assert.equal(subagentFor(withSaved, thread({ subagents: [settledLive] })), saved);
  assert.equal(subagentFor(agentRow(), thread({ subagents: [settledLive] })), settledLive);
  // After a restart the thread knows nothing; the row still does.
  assert.equal(subagentFor(withSaved, null), saved);
  assert.equal(subagentFor(agentRow(), null), null);
});

test('the preview line: the step while running, the error or the result once settled', () => {
  assert.equal(previewLine(agent()), 'Starting agent');
  assert.equal(previewLine(agent({ stepTool: 'Bash', step: 'npm test' })), 'Running npm test');
  assert.equal(previewLine(agent({ stepTool: 'Read', step: 'src/App.tsx' })), 'Reading src/App.tsx');
  assert.equal(previewLine(agent({ step: 'Looking at the tests.' })), 'Looking at the tests.');
  assert.equal(stepLine(agent({ stepTool: 'mcp__github__get_pr', step: '42' })), 'github · get_pr 42');
  assert.equal(stepLine(agent({ stepTool: 'Frobnicate', step: null })), 'Frobnicate');
  assert.equal(previewLine(agent({ status: 'completed', report: '\n## Summary\nAll green.' })), 'Summary');
  assert.equal(previewLine(agent({ status: 'completed' })), 'Finished without a report.');
  assert.equal(previewLine(agent({ status: 'failed', error: 'Rate limited' })), 'Rate limited');
  assert.equal(previewLine(agent({ status: 'stopped' })), 'Stopped before it finished.');
  assert.equal(reportPreview('y'.repeat(500))?.length, 160);
});

test('titles, elapsed time and tokens', () => {
  assert.deepEqual(titleOf(agent()), { name: 'Echo alpha', type: null });
  assert.deepEqual(titleOf(agent({ agentType: 'reviewer' })), { name: 'Echo alpha', type: 'reviewer' });
  assert.deepEqual(titleOf(agent({ name: '', description: '', agentType: 'reviewer' })), { name: 'reviewer', type: null });
  assert.equal(elapsedMs(agent(), 4_000), 3_000);
  assert.equal(elapsedMs(agent({ status: 'completed', durationMs: 900, endedAt: 9_000 }), 99_000), 900);
  assert.equal(elapsedMs(agent({ status: 'stopped', endedAt: 6_000 }), 99_000), 5_000);
  assert.equal(formatTokens(980), '980 tokens');
  assert.equal(formatTokens(1500), '1.5k tokens');
  assert.equal(formatTokens(23_400), '23k tokens');
  assert.equal(formatTokens(null), null);
});

test('the roster lists running agents first, then the newest settled, at most 30', () => {
  const list = [
    agent({ toolCallId: 'done-old', status: 'completed', endedAt: 2_000 }),
    agent({ toolCallId: 'run-late', startedAt: 5_000 }),
    agent({ toolCallId: 'done-new', status: 'failed', endedAt: 8_000 }),
    agent({ toolCallId: 'run-early', startedAt: 1_000 }),
  ];
  assert.deepEqual(
    rosterOrder(list).map((s) => s.toolCallId),
    ['run-early', 'run-late', 'done-new', 'done-old'],
  );
  const many = Array.from({ length: 40 }, (_, i) => agent({ toolCallId: `a${i}`, startedAt: i }));
  assert.equal(rosterOrder(many).length, 30);
});

test('the dock chip counts running agents, then the settled total', () => {
  assert.equal(chipLabel({ subagents: [], subagentsRunning: 0 }), null);
  assert.equal(chipLabel({ subagents: [agent(), agent({ toolCallId: 'b' })], subagentsRunning: 2 }), '2 subagents running');
  assert.equal(chipLabel({ subagents: [agent()], subagentsRunning: 1 }), '1 subagent running');
  assert.equal(chipLabel({ subagents: [agent({ status: 'completed' }), agent({ status: 'failed' })], subagentsRunning: 0 }), '2 subagents');
});

test('running counts per chat follow thread events (the sidebar field), newest revision wins', () => {
  const bus = new EventBus();
  const counts = countsFor(bus);
  assert.equal(countsFor(bus), counts, 'one tracker per bus');
  let changes = 0;
  counts.subscribe(() => changes++);
  bus.dispatch({ kind: 'thread', chatId: 'c1', thread: thread({ chatId: 'c1', subagentsRunning: 2, revision: 5 }) });
  assert.equal(counts.running('c1'), 2);
  assert.equal(counts.running('c2'), 0);
  // An older revision arriving late does not win.
  bus.dispatch({ kind: 'thread', chatId: 'c1', thread: thread({ chatId: 'c1', subagentsRunning: 0, revision: 4 }) });
  assert.equal(counts.running('c1'), 2);
  bus.dispatch({ kind: 'thread', chatId: 'c1', thread: thread({ chatId: 'c1', subagentsRunning: 0, revision: 6 }) });
  assert.equal(counts.running('c1'), 0);
  assert.equal(changes, 2);
  const solo = new SubagentCounts();
  solo.apply(thread({ chatId: 'c3', subagentsRunning: 1, revision: 1 }));
  solo.forget('c3');
  assert.equal(solo.running('c3'), 0);
});
