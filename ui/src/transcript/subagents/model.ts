// Subagent cards and roster: which subagent a row shows, what
// its lines say, and the roster's order. Pure, so it is tested under node:test.

import type { Subagent, SubagentStatus } from '../../api/generated/index.ts';
import type { MessageRow, ThreadState } from '../../api/types.ts';

/** The native tools that start a subagent (Claude Code 2.1.x calls it Agent, earlier Task). */
export function isAgentRow(row: MessageRow): boolean {
  return row.role === 'toolResult' && (row.toolName === 'Agent' || row.toolName === 'Task');
}

/**
 * The subagent behind an Agent tool row: the live one on the thread while it runs; once settled,
 * the state saved on the row (it has the longer report and survives a restart), else the live one.
 */
export function subagentFor(row: MessageRow, thread: ThreadState | null | undefined): Subagent | null {
  if (!isAgentRow(row) || !row.toolCallId) return null;
  const live = thread?.subagents?.find((s) => s.toolCallId === row.toolCallId);
  if (live?.status === 'running') return live;
  return row.details?.subagent ?? live ?? null;
}

export const STATUS_LABEL: Record<SubagentStatus, string> = {
  running: 'Working',
  completed: 'Completed',
  failed: 'Failed',
  stopped: 'Stopped',
};

const STEP_VERB: Record<string, string> = {
  Bash: 'Running',
  PowerShell: 'Running',
  Read: 'Reading',
  Write: 'Writing',
  Edit: 'Editing',
  MultiEdit: 'Editing',
  NotebookEdit: 'Editing',
  Grep: 'Searching',
  Glob: 'Searching',
  LS: 'Listing',
  WebFetch: 'Fetching',
  WebSearch: 'Searching web',
  TodoWrite: 'Updating plan',
  Agent: 'Delegating',
  Task: 'Delegating',
  Skill: 'Using skill',
};

/** The newest step as one line: "Running npm test", "Reading src/App.tsx", or narration. */
export function stepLine(s: Subagent): string | null {
  if (!s.stepTool) return s.step ?? null;
  const mcp = /^mcp__(.+?)__(.+)$/.exec(s.stepTool);
  const verb = mcp ? `${mcp[1]} · ${mcp[2]}` : (STEP_VERB[s.stepTool] ?? s.stepTool);
  return s.step ? `${verb} ${s.step}` : verb;
}

/** The first non-empty line of a report, bounded. */
export function reportPreview(report: string | null | undefined, max = 160): string | null {
  if (!report) return null;
  const line = report
    .split(/\r?\n/)
    .map((l) => l.replace(/^[#>*\-\s]+/, '').trim())
    .find((l) => l.length > 0);
  if (!line) return null;
  return line.length > max ? `${line.slice(0, max - 1)}…` : line;
}

/** The card's second line: the error, the current step, or the result. */
export function previewLine(s: Subagent): string {
  if (s.status === 'running') return stepLine(s) ?? 'Starting agent';
  if (s.status === 'failed') return s.error || reportPreview(s.report) || 'The agent failed.';
  if (s.status === 'stopped') return s.error || reportPreview(s.report) || 'Stopped before it finished.';
  return reportPreview(s.report) ?? 'Finished without a report.';
}

/** The card's title: the agent's own name, with its definition when that adds something. */
export function titleOf(s: Subagent): { name: string; type: string | null } {
  const name = s.name || s.description || s.agentType || 'Subagent';
  const type = s.agentType && s.agentType !== name && s.agentType !== 'general-purpose' ? s.agentType : null;
  return { name, type };
}

/** Elapsed for a running agent, the recorded duration for a settled one. */
export function elapsedMs(s: Subagent, now: number): number {
  if (s.status === 'running') return Math.max(0, now - s.startedAt);
  if (s.durationMs != null) return s.durationMs;
  return Math.max(0, (s.endedAt ?? now) - s.startedAt);
}

/** "1.2k tokens", "980 tokens". */
export function formatTokens(n: number | null | undefined): string | null {
  if (n == null) return null;
  return n >= 1000 ? `${(n / 1000).toFixed(n >= 10_000 ? 0 : 1)}k tokens` : `${n} tokens`;
}

export const ROSTER_MAX = 30;

/** "Subagents across this chat": running first (oldest first), then settled (newest first). */
export function rosterOrder(list: readonly Subagent[]): Subagent[] {
  const running = list.filter((s) => s.status === 'running').sort((a, b) => a.startedAt - b.startedAt);
  const settled = list.filter((s) => s.status !== 'running').sort((a, b) => (b.endedAt ?? b.startedAt) - (a.endedAt ?? a.startedAt));
  return [...running, ...settled].slice(0, ROSTER_MAX);
}

/** The dock chip's words: "2 subagents running", or the settled total once none runs. */
export function chipLabel(thread: Pick<ThreadState, 'subagents' | 'subagentsRunning'>): string | null {
  const total = thread.subagents?.length ?? 0;
  if (total === 0) return null;
  const running = thread.subagentsRunning ?? thread.subagents.filter((s) => s.status === 'running').length;
  if (running > 0) return `${running} ${running === 1 ? 'subagent' : 'subagents'} running`;
  return `${total} ${total === 1 ? 'subagent' : 'subagents'}`;
}
