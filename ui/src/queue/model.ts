// The queue's row model: plain-English strings for why a chat waits and how long it has
// waited, which actions the queue will take for a row, and the chip's count. The build
// queue's parts live here too, same reason. Pure, so the table test covers it without a host.

import type { BuildQueueEntry, QueueEntry, QueueSummary } from '../api/generated/index.ts';

/** Why a start waits, in one short line. Unknown reasons show as themselves. */
export const REASON_LABEL: Record<string, string> = {
  local_cap: 'Waiting for the local model',
  claude_capacity: 'Claude allowance used up',
  at_limit: 'Too many chats running',
  manual: 'Parked by you',
};

/** What kind of start it is, in a couple of words for the row's tag. */
export const CLASS_LABEL: Record<string, string> = {
  interactive: 'a person',
  normal: 'work',
  background: 'a timer',
};

export function reasonLabel(reason: string): string {
  return REASON_LABEL[reason] ?? reason;
}

/** "Waiting for the local model · 2 waiting ahead" — the line a row shows for its wait.
 * For a Claude-window wait with a known reset it names the time instead, when given. */
export function waitLine(entry: QueueEntry, ahead: number, untilText: string | null): string {
  const base = reasonLabel(entry.reason);
  if (entry.reason === 'claude_capacity' && untilText) return `${base} · opens again ${untilText}`;
  if (entry.reason === 'manual') return base;
  if (ahead > 0) return `${base} · ${ahead} waiting ahead`;
  return base;
}

/** How many starts stand ahead of this one in its line (the list is front to back). */
export function aheadOf(entries: readonly QueueEntry[], chatId: string): number {
  const i = entries.findIndex((e) => e.chatId === chatId);
  if (i <= 0) return 0;
  const mine = entries[i]!;
  let n = 0;
  for (const e of entries.slice(0, i)) {
    if (sameLine(e, mine)) n += 1;
  }
  return n;
}

/** Two entries wait on the same line: the same local server, or the Claude windows together.
 * A hand-parked chat has no line. */
export function sameLine(a: QueueEntry, b: QueueEntry): boolean {
  if (a.reason === 'manual' || b.reason === 'manual') return false;
  return a.serverId === b.serverId;
}

/** The actions a row offers. Mirrors the host's refusals so a dead click never shows.
 *  `drag` is the pointer way of the up/down pair: a hand-parked chat has no line to drag
 *  along, so its row cannot be picked up. */
export function actionsFor(entry: QueueEntry): { top: boolean; upDown: boolean; drag: boolean; release: boolean; cancel: boolean } {
  const line = entry.reason !== 'manual';
  return {
    top: line,
    upDown: line,
    drag: line,
    release: entry.reason === 'manual',
    cancel: true,
  };
}

/** The chip's badge: every chat whose turn start waits. */
export function queueCount(summary: QueueSummary | null): number {
  return summary ? summary.entries.length : 0;
}

/** One line per local server: "Strata: 3 working, 4 waiting". Names come from the caller;
 * an unknown server id is shown as it is. */
export function serverLine(working: number, queued: number): string {
  const w = `${working} working`;
  const q = queued === 1 ? '1 waiting' : `${queued} waiting`;
  return `${w}, ${q}`;
}

/** The Claude line for the panel and the Settings summary. */
export function claudeLine(summary: QueueSummary, now: number): string {
  if (summary.claudeFull) {
    const until = summary.claudeUntil != null ? untilText(summary.claudeUntil, now) : null;
    return until ? `Claude's allowance is used up — new chats start again ${until}.` : "Claude's allowance is used up — new chats start again when the window reopens.";
  }
  if (summary.claudeQueued > 0) {
    return `${summary.claudeQueued} waiting for a place to open.`;
  }
  return 'Nothing is waiting on Claude.';
}

/** A clock time for a reset that is not soon; "in N min" when it is. */
export function untilText(until: number, now: number): string {
  const mins = Math.max(1, Math.round((until - now) / 60_000));
  if (mins < 90) return `in ${mins} min`;
  const d = new Date(until);
  const hh = String(d.getHours()).padStart(2, '0');
  const mm = String(d.getMinutes()).padStart(2, '0');
  return `at ${hh}:${mm}`;
}

// ————— The build queue: features to run one after another. —————

/** What a build entry's state means in one word. Unknown states show as stored. */
export const BUILD_STATE_LABEL: Record<string, string> = {
  waiting: 'Waiting',
  running: 'Running',
  paused: 'Paused',
  done: 'Done',
};

export function buildStateLabel(state: string): string {
  return BUILD_STATE_LABEL[state] ?? state;
}

/** One line under an entry's title: what to work on — the brief, shortened to fit, or the
 * board item when there is no brief — and which model it runs on. */
export function buildLine(entry: BuildQueueEntry): string {
  const brief = entry.brief?.trim();
  const work = brief ? clip(brief, 120) : `Board item ${entry.itemId ?? ''}`.trim();
  return `${work} · ${entry.model ? `on ${entry.model}` : 'default model'}`;
}

function clip(text: string, max: number): string {
  return text.length > max ? `${text.slice(0, max - 1)}…` : text;
}

/** The actions a row offers; mirrors the host's refusals so a dead click never shows.
 *  Only entries still in line (waiting, paused) can be picked up or moved — a running one
 *  is mid-build and a done one is history. A done row's chat stays openable; Remove takes
 *  any row out of the list. */
export function buildActionsFor(entry: BuildQueueEntry): { open: boolean; pause: boolean; resume: boolean; reorder: boolean; remove: boolean } {
  return {
    open: entry.chatId != null,
    pause: entry.state === 'waiting' || entry.state === 'running',
    resume: entry.state === 'paused',
    reorder: entry.state === 'waiting' || entry.state === 'paused',
    // every listed entry can be taken out of the list: that is how done rows leave it
    remove: entry.state !== 'removed',
  };
}
