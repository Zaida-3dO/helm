// Turn work blocks. A turn opens only at a message the person sent (typed, voice, Telegram or an
// action button): a steer they read mid-turn stays inside the running turn, and nothing else ever
// splits one — messages from other chats, saved schedules and notes the app wrote for the agent
// keep the current turn going, exactly as the agent experiences them.
//
// A turn shows as: the person's message, then every message meant for the reader, in order — notes
// the agent wrote, the person's own mid-turn steers, relayed user actions, the subagent cards,
// errors, interruptions, retries, context rows, the cards an agent puts in the chat (a gallery of
// pictures, an option board) — and the turn's final reply with its recap. Under all of that sits
// ONE collapsed work group holding the turn's whole working life: every tool call (failed calls
// fold like passed ones), the blank messages tool calls are saved under, thinking blocks with no
// text of their own, the compact cards of messages from other chats, schedules or the app, and the
// agent's short aside notes ("(Correct)") with their thinking. Those internal rows gathered from
// between the visible messages all fold into that one group, wherever they arrived, so the reader
// sees the thread of what was said without a row of loose tool calls breaking it up. A note's own
// thinking stays with its row, folded by its own toggle: collapsed, but always reachable.
// Consecutive Agent calls are drawn as one group of cards.
//
// Live, that bottom group is the working indicator: its header names what the agent is doing now
// and counts the work folded under it, and the transcript does not jump at settle — only the
// header's "what it is doing now" word changes.
//
// A declared progress line (an app_progress call) sits under that group as the turn's newest
// visible thing. A newer line, a real agent message or the reply folds the old one into the group
// as a thought-style row; when the person speaks mid-turn, the line that was current at that
// moment stays in the flow above their message, as the mark of where the agent was.
//
// The chat's first message is never folded, whoever sent it: an opener written by another chat, a
// schedule or the app stands in the flow as its card, so the reader can always see what the chat
// was asked to do. Only the first one: later messages of that kind fold as above.
//
// A row with nothing to show — the empty message each tool call is saved under — is never drawn at
// all, closed or opened: hidden tool activity takes no space rather than leaving an empty gap.

import type { MessageRow } from '../api/types.ts';
import { isCardRow, progressText } from './cards.ts';
import { buildRecap, type Recap } from './recap.ts';
import { agentNote } from './agentNote.ts';
import { isHelmNote, isQuietReply, isUserAction } from './origin.ts';
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
      /** The turn is still running: the header carries the working line. */
      live: boolean;
      /** While live: a few words on what the agent is doing now. */
      working: string;
    }
  // Agent calls that follow one another, drawn as one header over a grid of subagent cards.
  | { kind: 'agents'; key: string; rows: MessageRow[]; flags: RowFlags[] }
  // A declared progress line (an app_progress call): one short line on what the agent is doing.
  // The newest one stands under the turn's work group, the newest thing in the turn; `pinned`
  // marks one kept in the flow above the person's mid-turn message, as the mark of where the
  // agent was when they spoke.
  | { kind: 'progress'; key: string; row: MessageRow; text: string; pinned?: boolean }
  // Where a roll-over stack's sessions meet in one transcript: a thin divider, no content.
  | { kind: 'divider'; key: string; sessionId: string; title: string; rolledOverAt: number };

/** The expanded-set key that opens every group of a turn at once (a jump into folded work). */
export function revealKey(turnKey: string): string {
  return `turn:${turnKey}`;
}

/** The group header's words: what it holds, how long it took, and (live) what it is doing now. */
export function foldLabel(g: { tools: number; thoughts: number; messages: number; elapsedMs: number; working?: string }): string {
  const parts: string[] = [];
  if (g.working) parts.push(g.working);
  if (g.tools > 0) parts.push(`${g.tools} tool call${g.tools === 1 ? '' : 's'}`);
  if (g.thoughts > 0) parts.push(`${g.thoughts} thought${g.thoughts === 1 ? '' : 's'}`);
  if (g.messages > 0) parts.push(`${g.messages} message${g.messages === 1 ? '' : 's'}`);
  if (g.elapsedMs > 0) parts.push(`worked for ${formatElapsed(g.elapsedMs)}`);
  return parts.join(' · ');
}

/**
 * Only a message from the person opens a turn (typed, voice, Telegram, or a click on a page's
 * button). A steer they read mid-turn belongs to the running turn, and an agent's or the app's
 * message never opens one: it keeps the turn it landed in, the way it reads to the agent.
 */
export function opensTurn(r: MessageRow): boolean {
  if (r.role !== 'user' || r.details?.mode === 'steer') return false;
  return !isHelmNote(r) && !agentNote(r);
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
 * speak for themselves (an error, a retry, a stop) still show. A note the app wrote for the
 * agent alone is never drawn either.
 */
function undrawn(r: MessageRow): boolean {
  return (isBlank(r) && !alwaysShown(r)) || isQuietReply(r) || isHelmNote(r);
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
 * An aside note: its whole text is one parenthesised or bracketed run ("(Correct)", "[skipped]"),
 * or it carries no letter or digit at all. Shape decides, not length: a short aside folds into the
 * work group with its thinking, while a note that speaks to the reader stays in the flow.
 */
function isStubNote(r: MessageRow): boolean {
  if (r.role !== 'assistant' || alwaysShown(r) || isQuietReply(r)) return false;
  const t = r.text.trim();
  if (!t) return false;
  return /^[([][\s\S]*[)\]]$/.test(t) || !/[\p{L}\p{N}]/u.test(t);
}

/**
 * A row that belongs in the turn's work group rather than standing in the flow on its own: tool
 * calls (passed and failed alike), the blank messages tool calls are saved under, thinking blocks
 * with no text of their own, short aside notes, and the compact cards of messages that came from
 * another chat, a schedule or the app. The person's own rows (a typed or action-button send,
 * Telegram, a mid-turn steer, a relayed user action), gallery and option cards, subagent calls,
 * notes, context rows, stopped calls and failures stay visible.
 */
function groupable(r: MessageRow): boolean {
  if (r.role === 'user') return !!agentNote(r) && !isHelmNote(r) && !isUserAction(r);
  if (r.role === 'toolResult') return r.status !== 'interrupted' && !isCardRow(r) && !isAgentRow(r);
  if (r.role === 'assistant') return !alwaysShown(r) && !isQuietReply(r) && (isBlank(r) || (!!r.thinking.trim() && !isNote(r)) || isStubNote(r));
  return false;
}

/**
 * The chat's first message when it did not come from the person (another chat, a schedule or the
 * app wrote it): the first message row, provided the agent had not yet answered or worked. Null
 * when the person opened the chat, or when this page does not begin at the chat's start.
 */
function chatOpener(rows: MessageRow[]): MessageRow | undefined {
  for (const r of rows) {
    if (r.role === 'assistant' || r.role === 'toolResult') return undefined;
    if (r.role === 'user') return groupable(r) ? r : undefined;
  }
  return undefined;
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
 * the group keys the reader has opened (a turn's `revealKey` opens its group); `dividers` names
 * the older session at each roll-over boundary (keyed by that session's id) so the meeting of two
 * sessions gets a divider row. `startsChat` says the rows begin at the chat's own beginning (no
 * older page is waiting to load), so its first message, if it did not come from the person, is
 * kept out of the work group.
 */
export function layout(
  rows: MessageRow[],
  live: boolean,
  expanded: ReadonlySet<string> = new Set(),
  dividers: ReadonlyMap<string, { title: string; rolledOverAt: number }> = new Map(),
  startsChat = true,
): Item[] {
  const items: Item[] = [];
  const opener = startsChat ? chatOpener(rows) : undefined;
  const folds = (r: MessageRow): boolean => r !== opener && groupable(r);
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
  while (i < rows.length) {
    // Rows before the first message from the person (a chat started by another chat or a
    // schedule, or a page that starts mid-turn) make a turn of their own with no opener row.
    const user = opensTurn(rows[i]!) ? rows[i] : undefined;
    const first = rows[i]!;
    boundary(first);
    if (user) {
      i++;
      items.push({ kind: 'row', row: user });
    }
    const work: MessageRow[] = [];
    while (i < rows.length && !opensTurn(rows[i]!)) work.push(rows[i++]!);
    const isLast = i >= rows.length;
    const turnKey = (user ?? first).sourceKey;
    const revealed = expanded.has(revealKey(turnKey));
    // The final reply: the last assistant row with text that is not just an aside.
    let reply = -1;
    for (let j = work.length - 1; j >= 0; j--) {
      const r = work[j]!;
      if (r.role === 'assistant' && r.text.trim() && !isQuietReply(r) && !isStubNote(r)) {
        reply = j;
        break;
      }
    }
    const settled = !(isLast && live);
    const hadTools = work.some((r) => r.role === 'toolResult');
    // Declared progress lines (app_progress calls): the newest stands under the group as the
    // turn's live line. A message the person sends mid-turn keeps the line that was current at
    // that moment above it, as the mark of where the agent was; every other progress row folds
    // into the group with the rest of the work. The newest line stops being the live one the
    // moment a real agent message follows it — a note or the reply takes over from it.
    let lastUnpinned = -1;
    let latestProg = -1;
    const pinned = new Set<number>();
    for (let j = 0; j < work.length; j++) {
      const r = work[j]!;
      if (progressText(r)) {
        latestProg = j;
        lastUnpinned = j;
      } else if (r.role === 'user' && !folds(r) && !isHelmNote(r) && lastUnpinned >= 0) {
        pinned.add(lastUnpinned);
        lastUnpinned = -1;
      } else if (isNote(r) && !isStubNote(r) && !isQuietReply(r)) {
        // A real agent message replaces the line before it: nothing is left to pin above a steer.
        lastUnpinned = -1;
      }
    }
    // Only a running turn keeps its newest line under the group; once it settles (with or
    // without a reply) the line folds into the group like the rest.
    const currentProg = !settled && latestProg >= 0 && latestProg > reply && !pinned.has(latestProg) ? latestProg : -1;
    // One work group per turn, under the whole turn: every groupable row of it in time order,
    // however scattered it arrived among the visible rows. A run of nothing but the blank
    // messages tool calls hide under is no work to show: no header over those alone.
    const group: number[] = [];
    for (let j = 0; j < work.length; j++) {
      if (folds(work[j]!) && j !== currentProg && !pinned.has(j)) group.push(j);
    }
    const hasWork = group.some((j) => !isBlank(work[j]!));
    const groupKey = hasWork ? `cg:${turnKey}:${work[group[0]!]!.id}` : '';
    const groupOpen = hasWork && (revealed || expanded.has(groupKey));
    const recap = settled && reply >= 0 ? recapFor(work[reply]!, work) : null;
    // Live, the group's header is the working indicator: a few words on the newest internal row.
    let working = '';
    if (!settled && hasWork) {
      const last = work[group[group.length - 1]!]!;
      if (last.role === 'toolResult' && last.status === 'streaming') working = last.details?.toolSummary || last.toolName || 'Working';
      else if (last.role === 'assistant' && !last.text.trim() && last.thinking.trim()) working = 'Thinking';
      else working = 'Working';
    }
    // The visible rows first, in time order; the group follows them as one item.
    let prevShown: MessageRow | undefined;
    const nextShown = (j: number) => {
      for (let k = j + 1; k < work.length; k++) {
        if (!folds(work[k]!) && !undrawn(work[k]!) && !pinned.has(k) && k !== currentProg) return work[k]!;
      }
      return undefined;
    };
    for (let j = 0; j < work.length; j++) {
      const r = work[j]!;
      // A progress line kept in the flow: it marks where the agent was, just above the
      // message the person sent at that moment.
      if (pinned.has(j)) {
        items.push({ kind: 'progress', key: `prog:${r.id}`, row: r, text: progressText(r), pinned: true });
        continue;
      }
      if (j === currentProg || folds(r) || undrawn(r)) continue;
      const next = nextShown(j);
      items.push({
        kind: 'row',
        row: r,
        ...(recap && j === reply ? { recap } : {}),
        // Live, the reply being written after the last tool already wears its final look, so
        // settling changes nothing about it.
        final: j === reply && hadTools,
        afterTool: prevShown?.role === 'toolResult',
        indent: false,
        afterFold: false,
        beforeTool: next !== undefined && next.role === 'toolResult' && !isAgentRow(next),
      });
      prevShown = r;
    }
    if (hasWork) {
      const grp = group.map((j) => work[j]!);
      items.push({
        kind: 'fold',
        key: groupKey,
        turnKey,
        rows: grp,
        elapsedMs: Math.max(0, grp[grp.length - 1]!.timestamp - grp[0]!.timestamp),
        failed: grp.filter((r) => r.role === 'toolResult' && toolStatus(r) === 'failed').length,
        tools: grp.filter((r) => !isBlank(r) && r.role === 'toolResult').length,
        thoughts: grp.filter((r) => !isBlank(r) && r.role === 'assistant' && r.thinking.trim() !== '').length,
        messages: grp.filter((r) => !isBlank(r) && r.role === 'user').length,
        open: groupOpen,
        live: !settled,
        working,
      });
      if (groupOpen) {
        // Opened: the group's rows follow the header, in time order, under its rule.
        let prevGroup: MessageRow | undefined;
        for (let k = 0; k < grp.length; k++) {
          const r = grp[k]!;
          if (undrawn(r)) continue;
          let nextGroup: MessageRow | undefined;
          for (let m = k + 1; m < grp.length; m++) {
            if (!undrawn(grp[m]!)) {
              nextGroup = grp[m]!;
              break;
            }
          }
          items.push({
            kind: 'row',
            row: r,
            indent: true,
            afterTool: prevGroup?.role === 'toolResult',
            beforeTool: nextGroup !== undefined && nextGroup.role === 'toolResult' && !isAgentRow(nextGroup),
          });
          prevGroup = r;
        }
      }
    }
    // The turn's live progress line under everything: the newest visible thing, until a newer
    // progress line, a real message or the reply takes its place.
    if (currentProg >= 0) {
      const r = work[currentProg]!;
      items.push({ kind: 'progress', key: `prog:${r.id}`, row: r, text: progressText(r) });
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
  // A single muted line, capped short by the tool.
  if (item.kind === 'progress') return 36;
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
