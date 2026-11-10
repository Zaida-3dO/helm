// The checklist a turn keeps. The agent writes it with the todo tool (every call carries the whole
// list) or, in newer Claude Code versions, with task calls that create and update one step at a
// time. Either way the turn's latest list is what the live card shows while it runs and what the
// recap keeps when it ends.

import type { MessageRow } from '../api/types.ts';
import { parseTodos, todoState, type Todo, type TodoState } from './todos.ts';
import { toolArgs } from './toolKinds.ts';

/** The rows of the newest turn: what follows the last user message that did not arrive mid-turn. */
export function newestTurn(rows: readonly MessageRow[]): readonly MessageRow[] {
  let u = rows.length - 1;
  while (u >= 0 && !(rows[u]!.role === 'user' && rows[u]!.details?.mode !== 'steer')) u--;
  return u < 0 ? [] : rows.slice(u + 1);
}

function text(v: unknown): string | null {
  return typeof v === 'string' && v.trim() ? v.trim() : null;
}

/**
 * The latest checklist among a turn's rows, or null when the agent kept none. A todo call replaces
 * the list; a task call adds a step or changes one (status `deleted` removes it).
 */
export function turnChecklist(work: readonly MessageRow[]): Todo[] | null {
  let whole: Todo[] | null = null;
  const tasks = new Map<string, Todo>();
  for (const r of work) {
    if (r.role !== 'toolResult') continue;
    if (r.toolName === 'TodoWrite') {
      const t = parseTodos(r);
      if (t) {
        whole = t;
        tasks.clear();
      }
    } else if (r.toolName === 'TaskCreate') {
      const a = toolArgs(r);
      const subject = text(a.subject) ?? text(a.content);
      if (!subject) continue;
      // The result names the step ("Task #3 created"); until it arrives the next free number stands in.
      const id = /#(\d+)/.exec(r.text)?.[1] ?? String(tasks.size + 1);
      tasks.set(id, { text: subject, state: todoState(a.status) });
    } else if (r.toolName === 'TaskUpdate') {
      const a = toolArgs(r);
      const id = a.taskId === undefined ? null : String(a.taskId);
      const task = id === null ? undefined : tasks.get(id);
      if (!id || !task) continue;
      if (a.status === 'deleted') tasks.delete(id);
      else tasks.set(id, { text: text(a.subject) ?? task.text, state: a.status === undefined ? task.state : todoState(a.status) });
    }
  }
  if (tasks.size > 0) return [...tasks.values()];
  return whole;
}

/** How many steps are done, for "5 of 6 steps". */
export function stepsDone(todos: readonly Todo[]): number {
  return todos.filter((t) => t.state === 'completed').length;
}

/** The steps that just became done between two renderings of a list, so the card can tick them once. */
export function tickedIndexes(before: readonly TodoState[] | null, after: readonly Todo[]): Set<number> {
  const out = new Set<number>();
  if (!before) return out;
  after.forEach((t, i) => {
    if (t.state === 'completed' && before[i] !== undefined && before[i] !== 'completed') out.add(i);
  });
  return out;
}
