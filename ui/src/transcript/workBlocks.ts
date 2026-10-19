// Turn work blocks. A turn starts at a user row (a steer read mid-turn does not
// start one, and is always shown); the agent activity after it (tool results, intermediate notes,
// thinking) is its work, and it folds behind one "worked for …" row, but only when the turn did
// real work (a tool call or thinking): a plain reply to "Hello" shows as it is. Errors,
// interruptions, retries and context rows are always shown, and so is the turn's final reply.
//
// A live turn folds as it goes: only its newest activity stays open, either the running tool or the text written since
// the last tool, so at settle nothing but that last piece folds and the transcript does not
// jump. An Agent call whose subagent is still running stays open after the fold, even once the
// turn has settled, and folds when the subagent finishes.

import type { MessageRow } from '../api/types.ts';
import { isAgentRow } from './subagents/model.ts';
import { toolStatus } from './toolKinds.ts';
import { ESTIMATE, estimateHeight, type EstimateInput } from './virtual.ts';

/** How a row sits in its turn, for spacing and the final-answer hairline. */
export interface RowFlags {
  /** The turn's final answer, after work (tool calls) in the same turn. */
  final?: boolean;
  /** The row directly after a tool row. */
  afterTool?: boolean;
  /** An opened turn's folded work, drawn behind a left rule. */
  indent?: boolean;
  /** The row directly after a closed "worked for" row, whose rule already separates it. */
  afterFold?: boolean;
}

export type Item =
  | ({ kind: 'row'; row: MessageRow } & RowFlags)
  | {
      kind: 'fold';
      key: string;
      turnKey: string;
      rows: MessageRow[];
      elapsedMs: number;
      /** How many of the folded tool calls failed. */
      failed: number;
      /** The reader has opened this turn, so its work is listed after the row. */
      open: boolean;
    };

/** A steer read mid-turn belongs to the running turn; only other user rows open a new turn. */
function opensTurn(r: MessageRow): boolean {
  return r.role === 'user' && r.details?.mode !== 'steer';
}

/** An assistant row with nothing to show (the host saves each tool call's message as one). */
function isBlank(r: MessageRow): boolean {
  return r.role === 'assistant' && !r.text.trim() && !r.thinking.trim();
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
 * turn keys the reader has opened; `runningAgents` holds the tool call ids of Agent calls whose
 * subagent is still running.
 */
export function layout(rows: MessageRow[], live: boolean, expanded: ReadonlySet<string> = new Set(), runningAgents: ReadonlySet<string> = new Set()): Item[] {
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
    const isOpen = expanded.has(turnKey);
    // The final reply: the last assistant row with text.
    let reply = -1;
    for (let j = work.length - 1; j >= 0; j--) {
      if (work[j]!.role === 'assistant' && work[j]!.text.trim()) {
        reply = j;
        break;
      }
    }
    const settled = !(isLast && live);
    const hadTools = work.some((r) => r.role === 'toolResult');
    const hadThinking = work.some((r) => r.role === 'assistant' && r.thinking.trim() !== '');
    // While live, the open part is the newest activity: the running tool if a tool is the newest
    // row, else everything written since the last tool. Settled, it is the final reply.
    let openFrom = work.length;
    if (!settled) {
      let lastTool = -1;
      for (let j = work.length - 1; j >= 0; j--) {
        if (work[j]!.role === 'toolResult') {
          lastTool = j;
          break;
        }
      }
      openFrom = lastTool < 0 ? work.length - 1 : lastTool === work.length - 1 ? lastTool : lastTool + 1;
    }
    const open = (j: number) => (settled ? j === reply : j >= openFrom);
    const push = (r: MessageRow, j: number, indent: boolean) => {
      const prev = j > 0 ? work[j - 1] : user;
      const last = items[items.length - 1];
      items.push({
        kind: 'row',
        row: r,
        // Live, the reply being written after the last tool already wears its final look, so
        // settling changes nothing about it.
        final: j === reply && hadTools && open(j),
        afterTool: prev?.role === 'toolResult',
        indent,
        afterFold: last?.kind === 'fold' && !last.open,
      });
    };
    // Nothing to fold away: the turn is empty, or did no work beyond replying.
    if (work.length === 0 || (!hadTools && !hadThinking)) {
      work.forEach((r, j) => push(r, j, false));
      continue;
    }
    const keep = (r: MessageRow) => alwaysShown(r) || (isAgentRow(r) && r.toolCallId !== null && runningAgents.has(r.toolCallId));
    const foldedIdx: number[] = [];
    const shownIdx: number[] = [];
    work.forEach((r, j) => (open(j) || keep(r) ? shownIdx : foldedIdx).push(j));
    // Live, a fold of nothing visible (the empty message a tool call is saved under) would only
    // put "Worked for 0s" above the first tool: wait for real work to fold.
    if (!settled && foldedIdx.every((j) => isBlank(work[j]!))) {
      work.forEach((r, j) => push(r, j, false));
      continue;
    }
    if (foldedIdx.length > 0) {
      // Timed from the run's first output, not the prompt: starting the worker, Claude Code and its
      // session hooks is not work the agent did on this turn.
      const first = work[0]!;
      const last = work[work.length - 1]!;
      items.push({
        kind: 'fold',
        key: `fold:${turnKey}`,
        turnKey,
        rows: foldedIdx.map((j) => work[j]!),
        elapsedMs: Math.max(0, last.timestamp - first.timestamp),
        failed: foldedIdx.filter((j) => toolStatus(work[j]!) === 'failed' && work[j]!.role === 'toolResult').length,
        open: isOpen,
      });
    }
    if (isOpen) {
      const folded = new Set(foldedIdx);
      work.forEach((r, j) => push(r, j, folded.has(j)));
    } else {
      shownIdx.forEach((j) => push(work[j]!, j, false));
    }
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

const estimates = new WeakMap<MessageRow, number>();

/**
 * A height guess for an item the transcript has not measured yet (virtual.ts), by kind and text
 * length. Remembered per row object, so a long transcript is not re-scanned on every render.
 */
export function estimateItem(item: Item | undefined): number {
  if (!item) return ESTIMATE;
  if (item.kind === 'fold') return estimateHeight({ kind: 'fold', chars: 0, lines: 0 });
  const row = item.row;
  let h = estimates.get(row);
  if (h === undefined) {
    const kind: EstimateInput['kind'] = row.role === 'user' || row.role === 'toolResult' || row.role === 'context' ? row.role : 'assistant';
    let lines = 0;
    for (let i = row.text.indexOf('\n'); i >= 0; i = row.text.indexOf('\n', i + 1)) lines++;
    h = estimateHeight({ kind, chars: row.text.length, lines });
    estimates.set(row, h);
  }
  return h;
}
