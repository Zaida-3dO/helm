// Turn work blocks. A turn starts at a user row (a steer read mid-turn does not
// start one, and is always shown); the agent activity after it (tool
// results, intermediate notes, thinking) is its work. Once the turn has settled, the work folds
// behind one "worked for …" row. A live turn never folds. Errors, interruptions, retries and context
// rows are always shown, and so is the turn's final reply.

import type { MessageRow } from '../api/types.ts';

export type Item =
  | { kind: 'row'; row: MessageRow }
  | { kind: 'fold'; key: string; turnKey: string; rows: MessageRow[]; elapsedMs: number };

/** A steer read mid-turn belongs to the running turn; only other user rows open a new turn. */
function opensTurn(r: MessageRow): boolean {
  return r.role === 'user' && r.details?.mode !== 'steer';
}

function alwaysShown(r: MessageRow): boolean {
  if (r.role === 'user') return true;
  if (r.status === 'error' || r.status === 'interrupted') return true;
  if (r.role === 'context') return true;
  if (r.details?.retry) return true;
  return false;
}

/**
 * Lay out rows as items. `live` says whether the newest turn is still running; `expanded` lists the
 * turn keys the reader has opened.
 */
export function layout(rows: MessageRow[], live: boolean, expanded: ReadonlySet<string> = new Set()): Item[] {
  const items: Item[] = [];
  let i = 0;
  // Rows before the first user row (a page that starts mid-turn) are shown as they are.
  while (i < rows.length && !opensTurn(rows[i]!)) items.push({ kind: 'row', row: rows[i++]! });
  while (i < rows.length) {
    const user = rows[i++]!;
    items.push({ kind: 'row', row: user });
    const work: MessageRow[] = [];
    while (i < rows.length && !opensTurn(rows[i]!)) work.push(rows[i++]!);
    const isLast = i >= rows.length;
    const turnKey = user.sourceKey;
    if ((isLast && live) || work.length === 0 || expanded.has(turnKey)) {
      for (const r of work) items.push({ kind: 'row', row: r });
      continue;
    }
    // The final reply: the last assistant row with text.
    let reply = -1;
    for (let j = work.length - 1; j >= 0; j--) {
      if (work[j]!.role === 'assistant' && work[j]!.text.trim()) {
        reply = j;
        break;
      }
    }
    const folded: MessageRow[] = [];
    const shown: MessageRow[] = [];
    work.forEach((r, j) => (j === reply || alwaysShown(r) ? shown : folded).push(r));
    if (folded.length > 0) {
      const last = work[work.length - 1]!;
      items.push({
        kind: 'fold',
        key: `fold:${turnKey}`,
        turnKey,
        rows: folded,
        elapsedMs: Math.max(0, last.timestamp - user.timestamp),
      });
    }
    for (const r of shown) items.push({ kind: 'row', row: r });
  }
  return items;
}

export function formatElapsed(ms: number): string {
  const s = Math.round(ms / 1000);
  if (s < 60) return `${s}s`;
  const m = Math.floor(s / 60);
  if (m < 60) return `${m}m ${s % 60}s`;
  return `${Math.floor(m / 60)}h ${m % 60}m`;
}
