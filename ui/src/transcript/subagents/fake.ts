// Dev fake host only: a prompt containing "subagent" dispatches three background subagents, the
// way Claude Code 2.1.x does (the Agent call returns at once; the agents work on, and the turn can
// end before they do). One completes with a report, one fails, one keeps working for a minute.

import type { Subagent } from '../../api/generated/index.ts';
import type { MessageRow, ThreadState } from '../../api/types.ts';

interface FakeChatLike {
  thread: ThreadState;
}

export interface FakeSubagentHooks {
  addRow: (c: never, p: Partial<MessageRow> & Pick<MessageRow, 'role'>) => MessageRow;
  updateRow: (c: never, r: MessageRow, p: Partial<MessageRow>) => MessageRow;
  emitThread: (c: never) => void;
  uid: () => string;
}

interface Plan {
  name: string;
  agentType: string;
  assignment: string;
  steps: [string | null, string][];
  end: 'completed' | 'failed' | 'running';
  report?: string;
  error?: string;
}

const PLANS: Plan[] = [
  {
    name: 'Review the diff',
    agentType: 'reviewer',
    assignment: 'Review the diff on this branch against the brief.\nList defects only, with file and line.',
    steps: [
      ['Bash', 'git diff origin/main --stat'],
      ['Read', 'ui/src/transcript/ToolCard.tsx'],
      [null, 'The card mounts once per tool row; checking the slot.'],
      ['Grep', 'SubagentCard'],
    ],
    end: 'completed',
    report: 'No defects found.\n\nThe card mounts once under each Agent row, the roster chip sits in the dock, and both read the live thread first.',
  },
  {
    name: 'Run the UI tests',
    agentType: 'general-purpose',
    assignment: 'Run npm test -w ui and report failures.',
    steps: [
      ['Bash', 'npm test -w ui'],
      [null, 'Two suites are still running.'],
    ],
    end: 'failed',
    error: 'Rate limited by the API; retry later.',
  },
  {
    name: 'Survey the sidebar code',
    agentType: 'Explore',
    assignment: 'Find where sidebar rows draw their status indicator.',
    steps: [
      ['Glob', 'ui/src/sidebar/**/*.tsx'],
      ['Read', 'ui/src/sidebar/Sidebar.tsx'],
      ['Grep', 'indicator'],
      [null, 'Reading the row component next.'],
      ['Read', 'ui/src/sidebar/Row.tsx'],
      ['Grep', 'WorkingFace'],
    ],
    end: 'running',
  },
];

/** Starts the three agents; returns at once, as the real Agent call does. */
export function runFakeSubagents(c: FakeChatLike, h: FakeSubagentHooks, stepMs = 900): void {
  const chat = c as never;
  const setAgent = (id: string, f: (s: Subagent) => Subagent) => {
    const subagents = c.thread.subagents.map((s) => (s.toolCallId === id ? f(s) : s));
    c.thread = { ...c.thread, subagents, subagentsRunning: subagents.filter((s) => s.status === 'running').length };
    h.emitThread(chat);
  };
  PLANS.forEach((plan, i) => {
    const toolCallId = h.uid();
    const startedAt = Date.now();
    const row = h.addRow(chat, {
      role: 'toolResult',
      toolName: 'Agent',
      toolCallId,
      status: 'complete',
      text: 'Async agent launched successfully.',
      details: { toolArgs: JSON.stringify({ description: plan.name, subagent_type: plan.agentType, prompt: plan.assignment }) },
    });
    const agent: Subagent = {
      toolCallId,
      agentType: plan.agentType,
      name: plan.name,
      description: plan.name,
      assignment: plan.assignment,
      status: 'running',
      stepTool: null,
      step: null,
      startedAt,
      endedAt: null,
      durationMs: null,
      tokens: null,
      toolUses: null,
      error: null,
      report: null,
      reportTruncated: false,
    };
    const subagents = [...c.thread.subagents, agent];
    c.thread = {
      ...c.thread,
      subagents,
      subagentsRunning: subagents.filter((s) => s.status === 'running').length,
      backgroundTasks: [...c.thread.backgroundTasks, { id: `bg-${toolCallId}`, title: plan.name, command: null, kind: 'agent', startedAt }],
    };
    h.emitThread(chat);
    let n = 0;
    const tick = () => {
      const st = plan.steps[n];
      if (st) {
        n++;
        setAgent(toolCallId, (s) => ({ ...s, stepTool: st[0], step: st[1], tokens: 800 + n * 1400, toolUses: st[0] ? n : s.toolUses }));
        setTimeout(tick, stepMs * (1 + i * 0.4));
        return;
      }
      if (plan.end === 'running') {
        n = 0; // keep working: loop the steps for a while
        if (Date.now() - startedAt < 60_000) setTimeout(tick, stepMs * 2);
        return;
      }
      const endedAt = Date.now();
      const settled: Partial<Subagent> = {
        status: plan.end,
        endedAt,
        durationMs: endedAt - startedAt,
        report: plan.report ?? null,
        error: plan.error ?? null,
      };
      setAgent(toolCallId, (s) => ({ ...s, ...settled }));
      c.thread = { ...c.thread, backgroundTasks: c.thread.backgroundTasks.filter((t) => t.id !== `bg-${toolCallId}`) };
      h.emitThread(chat);
      const saved = c.thread.subagents.find((s) => s.toolCallId === toolCallId);
      if (saved) h.updateRow(chat, row, { details: { ...row.details, subagent: saved } });
    };
    setTimeout(tick, stepMs / 2 + i * 300);
  });
}
