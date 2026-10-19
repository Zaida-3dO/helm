// The agent's to-do list (TodoWrite) as a plan checklist. The
// call's arguments carry the whole list each time; they are cut at 4 KB by the host, so a list
// that does not parse whole is read item by item and a cut-off last item is dropped.

import type { MessageRow } from '../api/types.ts';

export type TodoState = 'pending' | 'in_progress' | 'completed';

export interface Todo {
  text: string;
  state: TodoState;
}

function asTodo(v: unknown): Todo | null {
  if (!v || typeof v !== 'object') return null;
  const o = v as Record<string, unknown>;
  const text = typeof o.content === 'string' ? o.content : typeof o.subject === 'string' ? o.subject : null;
  if (!text) return null;
  const state: TodoState = o.status === 'completed' || o.status === 'in_progress' ? o.status : 'pending';
  return { text, state };
}

/** The list in a TodoWrite call, or null when the row is not one or holds no readable item. */
export function parseTodos(row: MessageRow): Todo[] | null {
  if (row.toolName !== 'TodoWrite') return null;
  const raw: unknown = row.details?.toolArgs;
  let list: unknown = null;
  if (raw && typeof raw === 'object') list = (raw as Record<string, unknown>).todos;
  else if (typeof raw === 'string' && raw) {
    try {
      const v: unknown = JSON.parse(raw);
      list = v && typeof v === 'object' ? (v as Record<string, unknown>).todos : null;
    } catch {
      // Cut short: read each complete item object on its own.
      const items: unknown[] = [];
      for (const m of raw.matchAll(/\{[^{}]*\}/g)) {
        try {
          items.push(JSON.parse(m[0]));
        } catch {
          // a fragment that does not decode
        }
      }
      list = items;
    }
  }
  if (!Array.isArray(list)) return null;
  const todos = list.map(asTodo).filter((t): t is Todo => t !== null);
  return todos.length ? todos : null;
}

/** "3/5 done", the collapsed row's target (plan update). */
export function todoSummary(todos: Todo[]): string {
  return `${todos.filter((t) => t.state === 'completed').length}/${todos.length} done`;
}
