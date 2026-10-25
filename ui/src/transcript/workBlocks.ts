// Turn work blocks. A turn starts at a user row (a steer read mid-turn does not
// start one, and is always shown); the agent activity after it is its work. Only the tool calls
// and the thinking fold behind one "worked for ..." row, and only when the turn did real work (a
// tool call or thinking): a plain reply to "Hello" shows as it is. The notes the agent wrote between
// tool calls, the subagent cards, errors, interruptions, retries, context rows and the turn's final
// reply stay in the flow. Consecutive Agent calls are drawn as one group of cards.
//
// A live turn folds as it goes: only its newest activity stays open, either the running tool or the
// text written since the last tool, so at settle nothing but that last piece folds and the
// transcript does not jump.

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
  /** An assistant row whose thinking sits behind the closed "worked for" row. */
  hideThinking?: boolean;
  /** In an opened turn, a note straight above a tool row: its bottom padding is tightened. */
  beforeTool?: boolean;
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
      /** What the hidden part holds: tool calls and thinking blocks, not the final reply's thinking. */
      steps: number;
      /** The reader has opened this turn, so its work is listed after the row. */
      open: boolean;
    }
  // Agent calls that follow one another, drawn as one header over a grid of subagent cards.
  | { kind: 'agents'; key: string; rows: MessageRow[]; flags: RowFlags[] };

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

/** A note: what the agent wrote between tool calls (and the reply itself). */
function isNote(r: MessageRow): boolean {
  return r.role === 'assistant' && r.text.trim() !== '';
}

/** Agent calls that follow one another become one item. */
function groupAgents(items: Item[]): Item[] {
  const out: Item[] = [];
  for (const it of items) {
    const prev = out[out.length - 1];
    if (it.kind !== 'row' || !isAgentRow(it.row)) {
      out.push(it);
      continue;
    }
    const { kind: _kind, row, ...flags } = it;
    if (prev?.kind === 'agents') {
      prev.rows.push(row);
      prev.flags.push(flags);
    } else out.push({ kind: 'agents', key: `agents:${row.id}`, rows: [row], flags: [flags] });
  }
  return out;
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
    const push = (r: MessageRow, j: number, indent: boolean, hideThinking = false) => {
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
        hideThinking,
        beforeTool: isOpen && work[j + 1]?.role === 'toolResult' && !isAgentRow(work[j + 1]!),
      });
    };
    // Nothing to fold away: the turn is empty, or did no work beyond replying.
    if (work.length === 0 || (!hadTools && !hadThinking)) {
      work.forEach((r, j) => push(r, j, false));
      continue;
    }
    // Tool calls and thinking fold; notes, subagent cards and the always-shown rows stay.
    const keep = (r: MessageRow) => alwaysShown(r) || isAgentRow(r) || isNote(r);
    const foldedIdx: number[] = [];
    const shownIdx: number[] = [];
    work.forEach((r, j) => (open(j) || keep(r) ? shownIdx : foldedIdx).push(j));
    // A kept note's own thinking goes behind the fold as well; the part still being written stays.
    const thoughtIdx = (!hadTools && foldedIdx.length === 0 ? [] : shownIdx).filter((j) => work[j]!.role === 'assistant' && work[j]!.thinking.trim() !== '' && (settled || j < openFrom));
    // Live, a fold of nothing visible (the empty message a tool call is saved under) would only
    // put "Worked for 0s" above the first tool: wait for real work to fold.
    if (!settled && thoughtIdx.length === 0 && foldedIdx.every((j) => isBlank(work[j]!))) {
      work.forEach((r, j) => push(r, j, false));
      continue;
    }
    if (foldedIdx.length > 0 || thoughtIdx.length > 0) {
      // Timed from the run's first output, not the prompt: starting the worker, Claude Code and its
      // session hooks is not work the agent did on this turn.
      const first = work[0]!;
      const last = work[work.length - 1]!;
      items.push({
        kind: 'fold',
        key: `fold:${turnKey}`,
        turnKey,
        rows: [...foldedIdx, ...thoughtIdx].sort((a, b) => a - b).map((j) => work[j]!),
        steps: foldedIdx.length + thoughtIdx.filter((j) => j !== reply).length,
        elapsedMs: Math.max(0, last.timestamp - first.timestamp),
        failed: foldedIdx.filter((j) => toolStatus(work[j]!) === 'failed' && work[j]!.role === 'toolResult').length,
        open: isOpen,
      });
    }
    if (isOpen) {
      const folded = new Set(foldedIdx);
      work.forEach((r, j) => push(r, j, folded.has(j)));
    } else {
      const hidden = new Set(thoughtIdx);
      shownIdx.forEach((j) => push(work[j]!, j, false, hidden.has(j)));
    }
  }
  return groupAgents(items);
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
  // A header and two cards a line.
  if (item.kind === 'agents') return 44 + 96 * Math.ceil(item.rows.length / 2);
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
