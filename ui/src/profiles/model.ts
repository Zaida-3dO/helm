// Profiles as plain data: which chats a profile shows (its own and those shared with it), the
// words the profile item uses, and how a last-seen time reads. No React, so the rules are tested
// under node:test.

import type { ProfilesView } from '../api/profiles.ts';

/** The colours Settings → Profiles offers, in the order the host hands them out. */
export const PROFILE_COLOURS = ['#6d8cff', '#ff8a65', '#4db6ac', '#ba68c8', '#ffd54f', '#81c784', '#f06292', '#90a4ae'] as const;

/**
 * The rows the profile rules need: a chat id and the chat it was started from, and, when the host
 * says so on the row, the profile that owns the chat and the profiles it is shared with.
 */
export interface IdentityRow {
  chatId: string;
  parentChatId: string | null;
  profileId?: string;
  sharedWith?: readonly string[];
}

/** Whether a record puts a chat in a profile's list (owned or shared); null when it does not say. */
export function belongsTo(c: { profileId?: string; sharedWith?: readonly string[] }, profileId: string): boolean | null {
  if (c.profileId === undefined) return null;
  return c.profileId === profileId || (c.sharedWith?.includes(profileId) ?? false);
}

/**
 * Whether the active profile shows a row. The row's own owner and shares decide when the host
 * sent them; otherwise the chat is in `ids` (the profile's own and shared chats, read from the
 * host), or follows the chat it was started from. Until `ids` is read there is nothing to go by,
 * so the row shows. With no active profile every row shows.
 */
export function inProfile(row: IdentityRow, byId: ReadonlyMap<string, IdentityRow>, ids: ReadonlySet<string> | null, activeId: string | null): boolean {
  if (activeId === null) return true;
  const own = belongsTo(row, activeId);
  if (own !== null) return own;
  if (ids === null) return true;
  const seen = new Set<string>();
  let cur: IdentityRow | undefined = row;
  while (cur && !seen.has(cur.chatId)) {
    seen.add(cur.chatId);
    if (ids.has(cur.chatId)) return true;
    cur = cur.parentChatId ? byId.get(cur.parentChatId) : undefined;
  }
  return false;
}

/** The rows the active profile shows: its own and those shared with it. */
export function rowsForProfile<R extends IdentityRow>(rows: readonly R[], activeId: string | null, ids: ReadonlySet<string> | null): readonly R[] {
  if (activeId === null) return rows;
  const byId = new Map(rows.map((r) => [r.chatId, r]));
  return rows.filter((r) => inProfile(r, byId, ids, activeId));
}

/**
 * The profile that owns a chat shared with the active one, when the row says so; null for the
 * active profile's own chats and when the row does not say.
 */
export function sharedFrom(row: IdentityRow, activeId: string | null): string | null {
  if (activeId === null || row.profileId === undefined || row.profileId === activeId) return null;
  return row.sharedWith?.includes(activeId) ? row.profileId : null;
}

/** The profiles a chat is shared with: a share made in this window wins over what the row says. */
export function sharedWithOf(row: IdentityRow | undefined, local: ReadonlySet<string> | undefined): Set<string> {
  return new Set(local ?? row?.sharedWith ?? []);
}

/** The letter in a profile's disc. */
export function initialOf(name: string): string {
  const c = [...name.trim()][0];
  return c ? c.toUpperCase() : '?';
}

/** The profile item's words: the active profile's name, or the question while the name is unset. */
export function itemText(view: ProfilesView | null): string {
  if (!view || view.homeId === null) return 'Who uses this Helm?';
  const p = view.profiles.find((x) => x.id === view.activeId) ?? view.profiles.find((x) => x.id === view.homeId);
  if (!p || p.needsName === true) return 'Who uses this Helm?';
  return p.name;
}

/** Whether the first-run question is still to be answered. */
export function needsFirstRun(view: ProfilesView | null): boolean {
  if (!view) return false;
  if (view.homeId === null) return true;
  return view.profiles.some((p) => p.isHome && p.needsName === true);
}

/** "3 chats · 1 running" under a profile's name on the profile screen. */
export function tileCounts(p: { chats: number; running: number }): string {
  return `${p.chats} ${p.chats === 1 ? 'chat' : 'chats'} · ${p.running} running`;
}

/** The tile after `from` in an arrow-key direction, wrapping; `from` -1 enters at the first (or last). */
export function stepTile(count: number, from: number, dir: -1 | 1): number {
  if (count <= 0) return -1;
  if (from < 0) return dir > 0 ? 0 : count - 1;
  return (from + dir + count) % count;
}

/** A last-seen time as epoch milliseconds, or null when there is none or it cannot be read. */
export function lastSeenMs(v: string | number | null | undefined): number | null {
  if (v === null || v === undefined || v === '') return null;
  const ms = typeof v === 'number' ? v : Date.parse(v);
  return Number.isFinite(ms) ? ms : null;
}
