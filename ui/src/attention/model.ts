// The Needs attention inbox as pure functions
// over the host's one attention summary (`host/crates/chats/src/attention.rs`). There is no AI
// triage yet, so the inbox lists what the summary already knows: chats waiting on you, chats that
// may be stuck, and unread results. Mark addressed and Snooze take a chat out until new activity.

import type { AttentionRow, AttentionSummary } from '../api/types.ts';

export type InboxGroup = 'needsYou' | 'stalled' | 'unread';

export interface InboxRow extends AttentionRow {
  group: InboxGroup;
}

/** Waiting on you first, then possibly stuck, then unread results; each chat once. */
export function inboxRows(s: AttentionSummary | null): InboxRow[] {
  if (!s) return [];
  const out: InboxRow[] = [];
  const seen = new Set<string>();
  const add = (rows: AttentionRow[], group: InboxGroup) => {
    for (const r of rows) {
      if (seen.has(r.chatId)) continue;
      seen.add(r.chatId);
      out.push({ ...r, group });
    }
  };
  add(s.needsYou, 'needsYou');
  add(s.stalled, 'stalled');
  add(s.unread, 'unread');
  return out;
}

/** The sidebar entry's count: the host's badge (each chat once), never fewer than the rows shown. */
export function inboxCount(s: AttentionSummary | null): number {
  if (!s) return 0;
  return Math.max(s.badge, inboxRows(s).length);
}

/** "N" or "N+" when the host holds more than the rows it sent (8 per group). */
export function countText(s: AttentionSummary | null): string {
  const n = inboxCount(s);
  return n > inboxRows(s).length ? `${n}+` : String(n);
}

/** Ctrl+Alt+A: the inbox chat after the open one, wrapping; the first when the open one is not listed. */
export function nextAttention(rows: readonly InboxRow[], current: string | null): string | null {
  if (rows.length === 0) return null;
  const i = current ? rows.findIndex((r) => r.chatId === current) : -1;
  return rows[(i + 1) % rows.length]!.chatId;
}

export interface SnoozeChoice {
  id: string;
  label: string;
  until: number;
  /** When it comes back, for the menu's right-hand hint. */
  when: string;
}

const MIN = 60_000;

function at915(d: Date): Date {
  const x = new Date(d);
  x.setHours(9, 15, 0, 0);
  return x;
}

/** The snooze menu: 20 minutes, 1 hour, 3 hours, tomorrow at 9:15, and Monday at 9:15
 *  only when that is not tomorrow. */
export function snoozeChoices(now: Date): SnoozeChoice[] {
  const t = now.getTime();
  const tomorrow = at915(new Date(now.getFullYear(), now.getMonth(), now.getDate() + 1));
  const toMonday = (8 - now.getDay()) % 7 || 7;
  const monday = at915(new Date(now.getFullYear(), now.getMonth(), now.getDate() + toMonday));
  const list: { id: string; label: string; until: number }[] = [
    { id: '20m', label: '20 minutes', until: t + 20 * MIN },
    { id: '1h', label: '1 hour', until: t + 60 * MIN },
    { id: '3h', label: '3 hours', until: t + 180 * MIN },
    { id: 'tomorrow', label: 'Tomorrow', until: tomorrow.getTime() },
  ];
  if (monday.getTime() !== tomorrow.getTime()) list.push({ id: 'monday', label: 'Monday', until: monday.getTime() });
  return list.map((c) => ({ ...c, when: whenText(new Date(c.until), now) }));
}

const two = (n: number) => String(n).padStart(2, '0');
const DAYS = ['Sun', 'Mon', 'Tue', 'Wed', 'Thu', 'Fri', 'Sat'];
const MONTHS = ['Jan', 'Feb', 'Mar', 'Apr', 'May', 'Jun', 'Jul', 'Aug', 'Sep', 'Oct', 'Nov', 'Dec'];

/** The time only when it is today, else the short weekday, month, day and time. */
export function whenText(d: Date, now: Date): string {
  const time = `${two(d.getHours())}:${two(d.getMinutes())}`;
  const sameDay = d.getFullYear() === now.getFullYear() && d.getMonth() === now.getMonth() && d.getDate() === now.getDate();
  return sameDay ? time : `${DAYS[d.getDay()]} ${MONTHS[d.getMonth()]} ${d.getDate()}, ${time}`;
}

/** The label above a card's reason. */
export function groupLabel(g: InboxGroup): string {
  return g === 'needsYou' ? 'Waiting on you' : g === 'stalled' ? 'May be stuck' : 'Ready to read';
}
