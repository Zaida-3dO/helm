// The Needs attention inbox as pure functions
// over the host's one attention summary (`host/crates/chats/src/attention.rs`): chats waiting on
// you, quiet chats the idle check thinks still need you (with its reason), chats that may be
// stuck, and chats you marked unread. A chat that merely finished sits in the summary's `finished`
// group for the finished notices, and is not listed here. Mark addressed and Snooze take a chat
// out until new activity.

import type { AttentionRow, AttentionSummary, MutedKind, MutedRow, TriageRow } from '../api/types.ts';

export type InboxGroup = 'needsYou' | 'flagged' | 'stalled' | 'unread';

export interface InboxRow extends AttentionRow {
  group: InboxGroup;
}

/** Waiting on you first, then quiet chats that may still need you, then possibly stuck, then chats
 *  you marked unread; each chat once. The summary's `finished` rows (a reply that ended unread) are
 *  not listed. `flagged: false` leaves the idle check's rows out (it was switched off
 *  and the host has not published yet). */
export function inboxRows(s: AttentionSummary | null, opts: { flagged?: boolean } = {}): InboxRow[] {
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
  if (opts.flagged !== false) add(s.flagged, 'flagged');
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

/**
 * The profile the Flagged tile, the inbox and Workspace status are scoped to. The host's summary
 * counts every chat on the PC (as do the taskbar badge, the tray and keep-awake, which stay that
 * way); the app shows one profile's: its own chats, those shared with it, and the chats nobody
 * owns when the home profile is shown. `activeId: null` means every profile.
 */
export interface AttentionScope {
  activeId: string | null;
  homeId: string | null;
  /** The chats the active profile shows (its own and those shared with it), from the host. */
  ids: ReadonlySet<string> | null;
}

/** An attention row once the host names the profile it belongs to ('' = nobody owns it). An older
 *  host leaves the field out, and the profile's chat list decides on its own. */
export type ProfiledAttentionRow = AttentionRow & { profileId?: string };

/** Whether one attention row belongs to the scope. */
export function attentionInScope(r: ProfiledAttentionRow, sc: AttentionScope): boolean {
  if (sc.activeId === null) return true;
  if (r.profileId === undefined) return sc.ids === null || sc.ids.has(r.chatId);
  if (r.profileId === '') return sc.activeId === sc.homeId;
  if (r.profileId === sc.activeId) return true;
  // Owned elsewhere: in scope when the active profile's own list includes the chat, i.e. it is
  // shared with it. With that list unread there is nothing to go by, so the row shows.
  return sc.ids === null || sc.ids.has(r.chatId);
}

/** The summary as one profile's: rows out of scope are left out and the counts are recounted from
 *  what remains (the host's counts cover the whole PC). With no active profile, unchanged. */
export function scopeAttention(s: AttentionSummary | null, sc: AttentionScope): AttentionSummary | null {
  if (!s || sc.activeId === null) return s;
  const keep = (rows: AttentionRow[]) => rows.filter((r) => attentionInScope(r, sc));
  const needsYou = keep(s.needsYou);
  const flagged = keep(s.flagged);
  const stalled = keep(s.stalled);
  const unread = keep(s.unread);
  const running = keep(s.running);
  const once = new Set([...needsYou, ...flagged, ...stalled, ...unread].map((r) => r.chatId));
  return { ...s, badge: once.size, busy: running.length, needsYou, flagged, stalled, unread, running, finished: keep(s.finished) };
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
  return g === 'needsYou' ? 'Waiting on you' : g === 'flagged' ? 'May still need you' : g === 'stalled' ? 'May be stuck' : 'Ready to read';
}

/** The idle check's flag on one chat, or null: the row the host lists for it among the quiet chats
 *  that may still need you. `enabled: false` hides it at once when the check was just switched off. */
export function flagFor(s: AttentionSummary | null, chatId: string, enabled = true): AttentionRow | null {
  if (!s || !enabled) return null;
  return s.flagged.find((r) => r.chatId === chatId) ?? null;
}

/** What the idle check judged about one listed chat: its priority chip, or null (an older verdict has none). */
export function priorityFor(s: AttentionSummary | null, chatId: string): 'normal' | 'high' | null {
  const p = s?.triage?.find((r) => r.chatId === chatId && !r.failed)?.priority;
  return p === 'high' || p === 'normal' ? p : null;
}

/** What the idle check judged about one listed chat: a stall offers Resume; anything else, including a verdict from before kinds, is a decision. */
export function triageKindFor(s: AttentionSummary | null, chatId: string): 'decision' | 'stalled' {
  return s?.triage?.find((r) => r.chatId === chatId && !r.failed)?.kind === 'stalled' ? 'stalled' : 'decision';
}

/** The short message Resume sends to a stalled chat. */
export const RESUME_TEXT = 'Please carry on with the work you were doing. It looks like it stopped moving.';

/** Chats whose idle check gave up, each with a Retry button; not counted in the badge, and left out while the check is off. */
export function failedChecks(s: AttentionSummary | null, enabled = true): TriageRow[] {
  return s && enabled ? (s.triage ?? []).filter((r) => r.failed) : [];
}

/** Muted chats holding a flag, question, approval or plan: the inbox's collapsed "Muted" group. They raise no notice and are not counted. */
export function mutedRows(s: AttentionSummary | null): MutedRow[] {
  return s?.muted ?? [];
}

/** The word on a muted row. */
export function mutedKindLabel(k: MutedKind): string {
  return k === 'approval' ? 'Approval' : k === 'question' ? 'Question' : k === 'plan' ? 'Plan' : 'Flag';
}

/** The longest quiet time the idle check can wait, in minutes. */
export const TRIAGE_MAX_DELAY_MINUTES = 1440;

/** The alert delay as typed: a whole number of minutes from 0 to 1,440, or null when it is not. */
export function parseDelayMinutes(text: string): number | null {
  const t = text.trim();
  if (!/^\d{1,4}$/.test(t)) return null;
  const n = Number(t);
  return n <= TRIAGE_MAX_DELAY_MINUTES ? n : null;
}

/** What Settings holds for the idle check. */
export interface TriageSettings {
  enabled: boolean;
  delayMinutes: number;
  /** The person's own wording for the decision; empty uses the built-in one. */
  prompt: string;
}
