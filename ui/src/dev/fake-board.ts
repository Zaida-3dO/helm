// The dev fake host's board reads (`board_read`, `board_cancel`): a small board with four columns,
// a fleet of holders, history, evidence and open loops, so the Tasks view and Ctrl+K's Board group
// can be looked at in a plain browser (open it with `?board=1`, which also saves a connection).
// Dev builds only.

import { ApiError } from '../api/client.ts';
import type { FakeOpHandler } from './fakeHost.ts';

const STATES: Record<string, string[]> = {
  backlog: ['on_deck', 'someday'],
  in_progress: ['executing', 'planning', 'in_review'],
  waiting: ['paused', 'blocked'],
  completed: ['merged', 'cancelled'],
};

interface FakeItem {
  id: string;
  title: string;
  state: string;
  column: string;
  priority: string;
  project: string;
  body: string;
}

function items(): FakeItem[] {
  const out: FakeItem[] = [];
  const hex = (n: number) => n.toString(16).padStart(8, '0');
  let n = 0;
  const counts: Record<string, number> = { backlog: 34, in_progress: 3, waiting: 2, completed: 4 };
  for (const [column, count] of Object.entries(counts)) {
    for (let i = 0; i < count; i++) {
      n++;
      const states = STATES[column]!;
      out.push({
        id: `${hex(0x5c9e2a00 + n)}-0f0b-4d6e-9d1f-0a7c1e3b9a10`,
        title: i === 0 && column === 'in_progress' ? 'Fix the merge queue' : `${column.replace('_', ' ')} task ${i + 1}`,
        state: states[i % states.length]!,
        column,
        priority: ['P0', 'P1', 'P2', 'P3'][i % 4]!,
        project: i % 2 === 0 ? 'fb-helm' : 'fb-site',
        body: i === 0 ? 'The merge queue stalls when two changes land at once.\n\nSteps: land two PRs together and watch the queue.' : '',
      });
    }
  }
  return out;
}

export function boardFakeOps(): { prefixes: string[]; handler: FakeOpHandler } {
  const all = items();
  const slim = (i: FakeItem) => ({ id: i.id, title: i.title, state: i.state, headline: null, kind: 'task', priority: i.priority, area: 'desktop-app', repo: 'helm', links: i.title === 'Fix the merge queue' ? [{ key: 'pr', url: 'https://github.com/example/helm/pull/12' }, { key: 'notes', url: 'javascript:alert(1)' }] : [] });
  const holders = (i: FakeItem) => (i.column === 'in_progress' ? [{ holderId: 'builder-one', holderType: 'agent', displayName: 'builder-one', role: 'builder', roleCustom: null, liveness: 'live', lastActive: new Date().toISOString() }] : []);
  const page = <T,>(list: T[], limit: number, cursor?: string, key: (x: T) => string = (x) => String((x as { id: string }).id)) => {
    const start = cursor ? list.findIndex((x) => key(x) === cursor) + 1 : 0;
    const part = list.slice(start, start + limit);
    return { part, next: start + limit < list.length ? key(part[part.length - 1]!) : null };
  };
  const find = (id: string) => all.find((i) => i.id.startsWith(String(id).toLowerCase()));

  const read = (a: Record<string, unknown>): unknown => {
    const action = String(a.action);
    const limit = Number(a.limit ?? 30);
    switch (action) {
      case 'projects':
        return { projects: [{ id: 'fb-helm', title: 'Helm', total: 12, finished: 4 }, { id: 'fb-site', title: 'Client Website', total: 5, finished: 1 }, { id: 'fb-done', title: 'Launch checklist', total: 3, finished: 3 }], complete: true };
      case 'ready':
        return { ready: true };
      case 'fleet':
        return { assignments: all.flatMap((i) => holders(i).map((h) => ({ ...h, id: `a-${i.id}`, itemId: i.id, machine: 'workbench', branch: 'work' }))), complete: true };
      case 'board_page': {
        const col = String(a.column);
        const mine = all.filter((i) => i.column === col && (!a.project || i.project === a.project));
        const { part, next } = page(mine, limit, a.cursor as string | undefined);
        const section = { entries: part.map((i) => ({ item: slim(i), column: col, assignments: holders(i), subtasks: null })), total: mine.length, nextCursor: next, withheld: false };
        return { board: { columns: { [col]: section }, notice: null } };
      }
      case 'item': {
        const i = find(String(a.id));
        if (!i) throw new ApiError('board_error', 'Agent Standup answered with an error (404); showing the last sync.');
        return { item: { ...slim(i), checkpointHeadline: i.column === 'in_progress' ? 'Reproduced the stall; fixing the lock order.' : null } };
      }
      case 'detail': {
        const i = find(String(a.id));
        if (!i) throw new ApiError('board_error', 'Agent Standup answered with an error (404); showing the last sync.');
        return { detail: { item: { ...slim(i), body: i.body } } };
      }
      case 'history': {
        const rows = Array.from({ length: 25 }, (_, k) => ({ id: String(100 - k), ts: new Date(Date.now() - k * 3_600_000).toISOString(), type: k % 3 === 0 ? 'checkpoint' : 'state_changed', headline: k % 3 === 0 ? `Checkpoint ${25 - k}` : null, body: k % 3 === 0 ? 'Progress noted.' : null }));
        const { part, next } = page(rows, limit, a.cursor as string | undefined);
        return { entries: part, total: rows.length, nextCursor: next };
      }
      case 'artifacts': {
        const rows = [{ id: 'ar1', kind: 'review', verdict: 'lgtm', ref: 'https://github.com/example/helm/pull/12', createdAt: new Date().toISOString(), body: 'Looks right.', bodyTruncated: true }];
        return { artifacts: rows, total: rows.length, nextCursor: null };
      }
      case 'loops': {
        const rows = [{ loopId: 'lp1', kind: 'work', status: 'open', openedAt: new Date().toISOString(), text: 'Check the lock order on Windows.', textTruncated: true }];
        return { loops: rows, total: rows.length, nextCursor: null };
      }
      case 'loop':
        return { loopId: String(a.loopId), text: 'Check the lock order on Windows. The full note is longer than the preview.' };
      case 'search': {
        const q = String(a.query).toLowerCase();
        return { matches: all.filter((i) => i.title.toLowerCase().includes(q)).slice(0, limit).map(slim) };
      }
    }
    throw new ApiError('bad_args', 'That Agent Standup request was not sent.');
  };

  const handler: FakeOpHandler = (op, a) => {
    if (op === 'board_cancel') return { cancelled: false };
    if (op === 'board_read') return { action: a.action, data: read(a) };
    throw new ApiError('refused', 'This operation is not available here.');
  };
  return { prefixes: ['board_read', 'board_cancel'], handler };
}
