// Turn work blocks. A turn starts at a user row (a steer read mid-turn does not
// start one, and is always shown); the agent activity after it is its work. Every consecutive run
// of tool calls, thinking blocks and message cards folds behind its own compact row — "12 tool
// calls", expandable — so the reader never sees a row of loose tool calls, and nothing hides a
// failed call differently from a passed one. Notes the agent wrote between the calls, the subagent
// cards, errors, interruptions, retries, context rows and the turn's final reply stay in the flow,
// and so do the cards an agent puts in the chat (a gallery of pictures, an option board) and
// messages the person sent themselves (a typed steer, an action button): those speak to the user,
// so they are never folded. A note's own thinking stays with its row, folded by its own toggle:
// collapsed, but always reachable. Consecutive Agent calls are drawn as one group of cards.
//
// A live turn folds as it goes: only its newest activity stays open, either the running tool or the
// text written since the last tool, so at settle nothing but that last piece folds and the
// transcript does not jump.
//
// A row with nothing to show — the empty message each tool call is saved under — is never drawn at
// all, closed or opened: hidden tool activity takes no space rather than leaving an empty gap.

import type { MessageRow } from '../api/types.ts';
import { isCardRow } from './cards.ts';
import { buildRecap, type Recap } from './recap.ts';
import { agentNote } from './agentNote.ts';
import { isHelmNote, isQuietReply } from './origin.ts';
import { isAgentRow } from './subagents/model.ts';
import { toolStatus } from './toolKinds.ts';
import { ESTIMATE, estimateHeight, type EstimateInput } from './virtual.ts';

/** How a row sits in its turn, for spacing and the final-answer hairline. */
export interface RowFlags {
  /** The turn's final answer, after work (tool calls) in the same turn. */
  final?: boolean;
  /** The row directly after a tool row. */
  afterTool?: boolean;
  /** An opened work group's row, drawn under a left rule. */
  indent?: boolean;
  /** The row directly after a closed group row, whose rule already separates it. */
  afterFold?: boolean;
  /** In an opened turn, a note straight above a tool row: its bottom padding is tightened. */
  beforeTool?: boolean;
  /** The newest notice, and the transcript's last row: the only notice row that is a live region. */
  noticeLive?: boolean;
  /** On a settled turn's final reply: the recap drawn under it. */
  recap?: Recap;
}

export type Item =
  | ({ kind: 'row'; row: MessageRow } & RowFlags)
  | {
      kind: 'fold';
      key: string;
      turnKey: string;
      rows: MessageRow[];
      /** The run's own span, first output to last. */
      elapsedMs: number;
      /** How many of the grouped tool calls failed. */
      failed: number;
      /** What the group holds, counted for its label. */
      tools: number;
      thoughts: number;
      messages: number;
      /** The reader has opened this group, so its rows follow the header. */
      open: boolean;
    }
  // Agent calls that follow one another, drawn as one header over a grid of subagent cards.
  | { kind: 'agents'; key: string; rows: MessageRow[]; flags: RowFlags[] }
  // Where a roll-over stack's sessions meet in one transcript: a thin divider, no content.
  | { kind: 'divider'; key: string; sessionId: string; title: string; rolledOverAt: number };

/** The expanded-set key that opens every group of a turn at once (a jump into folded work). */
export function revealKey(turnKey: string): string {
  return `turn:${turnKey}`;
}

/** The group header's words: what the run holds and how long it took. */
export function foldLabel(g: { tools: number; thoughts: number; messages: number; elapsedMs: number }): string {
  const parts: string[] = [];
  if (g.tools > 0) parts.push(`${g.tools} tool call${g.tools === 1 ? '' : 's'}`);
  if (g.thoughts > 0) parts.push(`${g.thoughts} thought${g.thoughts === 1 ? '' : 's'}`);
  if (g.messages > 0) parts.push(`${g.messages} message${g.messages === 1 ? '' : 's'}`);
  if (g.elapsedMs > 0) parts.push(`worked for ${formatElapsed(g.elapsedMs)}`);
  return parts.join(' · ');
}

/** A steer read mid-turn belongs to the running turn; only other user rows open a new turn. */
function opensTurn(r: MessageRow): boolean {
  return r.role === 'user' && r.details?.mode !== 'steer';
}

/** A notice row: what a refusal or a give-back leaves in the transcript (the default kind). */
function isNoticeRow(r: MessageRow): boolean {
  return r.role === 'context' && (r.details?.contextKind ?? 'notice') === 'notice';
}

/** An assistant row with nothing to show (the host saves each tool call's message as one). */
function isBlank(r: MessageRow): boolean {
  return r.role === 'assistant' && !r.text.trim() && !r.thinking.trim();
}

/**
 * A row the transcript never draws. A blank assistant row has no content once its tool call sits
 * in a group, and an empty row would only stand between the messages as a tall gap; rows that
 * speak for themselves (an error, a retry, a stop) still show.
 */
function undrawn(r: MessageRow): boolean {
  return (isBlank(r) && !alwaysShown(r)) || isQuietReply(r);
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

/**
 * A row that belongs in a work group rather than standing in the flow on its own: tool calls
 * (passed and failed alike), the blank messages tool calls are saved under, thinking blocks with
 * no text of their own, and the compact cards of messages that came from another chat, a schedule
 * or the app. The person's own rows (a typed or action-button send, Telegram), gallery and option
 * cards, subagent calls, notes, context rows, stopped calls and failures stay visible.
 */
function groupable(r: MessageRow): boolean {
  if (r.role === 'user') return !isHelmNote(r) && !!agentNote(r);
  if (r.role === 'toolResult') return r.status !== 'interrupted' && !isCardRow(r) && !isAgentRow(r);
  if (r.role === 'assistant') return !alwaysShown(r) && !isQuietReply(r) && (isBlank(r) || (!!r.thinking.trim() && !isNote(r)));
  return false;
}

const recaps = new WeakMap<MessageRow, { rows: number; recap: Recap | null }>();

/** A settled turn's recap, remembered per final reply so a long transcript is not re-scanned on every update. */
function recapFor(reply: MessageRow, work: MessageRow[]): Recap | null {
  const kept = recaps.get(reply);
  if (kept && kept.rows === work.length) return kept.recap;
  const recap = reply.status === 'complete' ? buildRecap(work) : null;
  recaps.set(reply, { rows: work.length, recap });
  return recap;
}

/**
 * Agent calls that follow one another become one item. A closed group row sitting between two
 * Agent calls does not split them: it moves ahead of the merged cards, as it did before the plain
 * call folded away. An opened group does split them — its rows are on screen in between.
 */
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
      continue;
    }
    const before = out[out.length - 2];
    if (prev?.kind === 'fold' && !prev.open && before?.kind === 'agents') {
      const fold = out.splice(out.length - 1, 1)[0]!;
      const ai = out.length - 1;
      out.splice(ai, 0, fold);
      const merged = out[ai + 1]!;
      if (merged.kind === 'agents') {
        merged.rows.push(row);
        merged.flags.push(flags);
      }
      continue;
    }
    out.push({ kind: 'agents', key: `agents:${row.id}`, rows: [row], flags: [flags] });
  }
  return out;
}

/**
 * Lay out rows as items. `live` says whether the newest turn is still running; `expanded` lists
 * the group keys the reader has opened (plus a turn's `revealKey`, which opens every group in it);
 * `dividers` names the older session at each roll-over boundary (keyed by that session's id) so
 * the meeting of two sessions gets a divider row.
 */
export function layout(
  rows: MessageRow[],
  live: boolean,
  expanded: ReadonlySet<string> = new Set(),
  dividers: ReadonlyMap<string, { title: string; rolledOverAt: number }> = new Map(),
): Item[] {
  const items: Item[] = [];
  let i = 0;
  // A page read across a roll-over stack: where the rows change session, put up a divider naming
  // the older session the reader meets going up.
  let lastChatId = '';
  const boundary = (r: MessageRow): void => {
    if (lastChatId !== '' && r.chatId !== lastChatId) {
      const meta = dividers.get(lastChatId);
      items.push({
        kind: 'divider',
        key: `div:${lastChatId}`,
        sessionId: lastChatId,
        title: meta?.title ?? '',
        rolledOverAt: meta?.rolledOverAt ?? 0,
      });
    }
    lastChatId = r.chatId;
  };
  // Rows before the first user row (a page that starts mid-turn) are shown as they are,
  // except the empty ones, which are never drawn.
  while (i < rows.length && !opensTurn(rows[i]!)) {
    const r = rows[i++]!;
    boundary(r);
    if (!undrawn(r)) items.push({ kind: 'row', row: r });
  }
  while (i < rows.length) {
    const user = rows[i++]!;
    boundary(user);
    if (!isHelmNote(user)) items.push({ kind: 'row', row: user });
    const work: MessageRow[] = [];
    while (i < rows.length && !opensTurn(rows[i]!)) work.push(rows[i++]!);
    const isLast = i >= rows.length;
    const turnKey = user.sourceKey;
    const revealed = expanded.has(revealKey(turnKey));
    // The final reply: the last assistant row with text.
    let reply = -1;
    for (let j = work.length - 1; j >= 0; j--) {
      if (work[j]!.role === 'assistant' && work[j]!.text.trim() && !isQuietReply(work[j]!)) {
        reply = j;
        break;
      }
    }
    const settled = !(isLast && live);
    const hadTools = work.some((r) => r.role === 'toolResult');
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
    const recap = settled && reply >= 0 ? recapFor(work[reply]!, work) : null;
    // Consecutive groupable runs become one group each; anything visible splits a run. While live
    // the open part stays out of every group, so a run folds up to the newest activity only.
    const runs: number[][] = [];
    let run: number[] = [];
    const flushRun = (): void => {
      // A run of nothing but the blank messages tool calls hide under is no work to show: while
      // live it would only put a header over the first call before anything real folds.
      if (run.some((j) => !isBlank(work[j]!))) runs.push(run);
      run = [];
    };
    for (let j = 0; j < work.length; j++) {
      if (open(j) || !groupable(work[j]!)) flushRun();
      else run.push(j);
    }
    flushRun();
    const groupOf = new Map<number, number>();
    runs.forEach((g, gi) => g.forEach((j) => groupOf.set(j, gi)));
    const groupKey = (gi: number) => `cg:${turnKey}:${work[runs[gi]![0]!]!.id}`;
    const groupOpen = (gi: number) => revealed || expanded.has(groupKey(gi));
    // A work row is on screen when its group is open or it is in the live open part.
    const drawn = (j: number) => open(j) || (groupOf.has(j) && groupOpen(groupOf.get(j)!));
    // The next row that will actually be drawn, skipping the blank messages tool calls hide under.
    const nextDrawn = (j: number) => {
      for (let k = j + 1; k < work.length; k++) if (!undrawn(work[k]!) && drawn(k)) return work[k]!;
      return undefined;
    };
    const push = (r: MessageRow, j: number, indent: boolean) => {
      if (undrawn(r)) return;
      const prev = j > 0 ? work[j - 1] : user;
      const last = items[items.length - 1];
      const next = nextDrawn(j);
      items.push({
        kind: 'row',
        row: r,
        ...(recap && j === reply ? { recap } : {}),
        // Live, the reply being written after the last tool already wears its final look, so
        // settling changes nothing about it.
        final: j === reply && hadTools && open(j),
        afterTool: prev?.role === 'toolResult',
        indent,
        afterFold: last?.kind === 'fold' && !last.open,
        beforeTool: next !== undefined && next.role === 'toolResult' && !isAgentRow(next),
      });
    };
    if (runs.length === 0) {
      work.forEach((r, j) => push(r, j, false));
      continue;
    }
    for (let j = 0; j < work.length; j++) {
      const gi = groupOf.get(j);
      if (gi === undefined) {
        push(work[j]!, j, false);
        continue;
      }
      const g = runs[gi]!;
      if (g[0] === j) {
        const grp = g.map((k) => work[k]!);
        const visible = grp.filter((r) => !isBlank(r));
        items.push({
          kind: 'fold',
          key: groupKey(gi),
          turnKey,
          rows: grp,
          elapsedMs: Math.max(0, grp[grp.length - 1]!.timestamp - grp[0]!.timestamp),
          failed: grp.filter((r) => r.role === 'toolResult' && toolStatus(r) === 'failed').length,
          tools: visible.filter((r) => r.role === 'toolResult').length,
          thoughts: visible.filter((r) => r.role === 'assistant' && r.thinking.trim() !== '').length,
          messages: visible.filter((r) => r.role === 'user').length,
          open: groupOpen(gi),
        });
      }
      if (groupOpen(gi)) push(work[j]!, j, true);
    }
  }
  const laid = groupAgents(items);
  // Only the newest notice is a live region for screen readers, and only while it is still the
  // transcript's last row. The list mounts rows as the reader scrolls, and a live region is read
  // out when it is inserted: making every old notice live would announce the chat's whole history
  // of refusals over again.
  const last = laid[laid.length - 1];
  if (last?.kind === 'row' && isNoticeRow(last.row)) last.noticeLive = true;
  return laid;
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
  if (item.kind === 'divider') return 34;
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
