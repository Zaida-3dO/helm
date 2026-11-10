// Profiles as plain data: which profile a chat belongs to, what the sidebar shows for the active
// choice, and the words the chip, the popover and the household rows use. No React, so the rules
// are tested under node:test.

import type { PeerChat, PeerMachine, PeerPerson, Profile, ProfilesView } from '../api/profiles.ts';

/** The colours Settings → Profiles offers, in the order the host hands them out. */
export const PROFILE_COLOURS = ['#6d8cff', '#ff8a65', '#4db6ac', '#ba68c8', '#ffd54f', '#81c784', '#f06292', '#90a4ae'] as const;

/** The rows the profile rules need: a chat id and the chat it was started from. */
export interface LabelledRow {
  chatId: string;
  parentChatId: string | null;
}

/**
 * The profile a chat belongs to: its own label, else the label of the chat it was started from,
 * else the home profile (a chat nobody labelled is the home profile's). `null` before the first
 * profile exists.
 */
export function profileOfChat(chatId: string, byId: ReadonlyMap<string, LabelledRow>, labels: ReadonlyMap<string, string>, homeId: string | null): string | null {
  const seen = new Set<string>();
  let cur: string | null = chatId;
  while (cur && !seen.has(cur)) {
    seen.add(cur);
    const own = labels.get(cur);
    if (own) return own;
    cur = byId.get(cur)?.parentChatId ?? null;
  }
  return homeId;
}

/** The rows the active choice shows: every row for All profiles (`activeId` null), else that profile's. */
export function rowsForProfile<R extends LabelledRow>(rows: readonly R[], activeId: string | null, homeId: string | null, labels: ReadonlyMap<string, string>): readonly R[] {
  if (activeId === null || homeId === null) return rows;
  const byId = new Map(rows.map((r) => [r.chatId, r]));
  return rows.filter((r) => profileOfChat(r.chatId, byId, labels, homeId) === activeId);
}

/** "Max" for the plan name the binary reported ("max"); nothing when it did not report one. */
export function planText(plan: string | null | undefined): string | null {
  const p = plan?.trim();
  if (!p) return null;
  return p.charAt(0).toUpperCase() + p.slice(1);
}

/** The letter in a profile's disc. */
export function initialOf(name: string): string {
  const c = [...name.trim()][0];
  return c ? c.toUpperCase() : '?';
}

/** The chip's words: "Name · Plan" for the home profile (plan only when known), "All profiles", else the name. */
export function chipText(view: ProfilesView | null, plan: string | null): string {
  if (!view || view.homeId === null) return 'Who uses this Helm?';
  if (view.activeId === null) return 'All profiles';
  const p = view.profiles.find((x) => x.id === view.activeId);
  if (!p) return 'All profiles';
  return p.isHome && plan ? `${p.name} · ${plan}` : p.name;
}

/** The person a profile stands for on the hub, when it is linked and is not this install's own person. */
export function personFor(profile: Profile, people: readonly PeerPerson[] | null): PeerPerson | null {
  if (!profile.hubPersonId || !people) return null;
  return people.find((p) => p.id === profile.hubPersonId) ?? null;
}

/** One machine of another person whose chats the sidebar can show. */
export interface HouseholdTarget {
  person: PeerPerson;
  machine: PeerMachine;
  /** "Name · machine". */
  title: string;
  key: string;
}

/**
 * The machines whose shared chats the sidebar lists: all of other people's for All profiles, and
 * for a profile only the machines of the person it is linked to (a local profile shows none).
 */
export function householdTargets(view: ProfilesView | null, people: readonly PeerPerson[] | null): HouseholdTarget[] {
  if (!view || !people) return [];
  const active = view.activeId === null ? null : (view.profiles.find((p) => p.id === view.activeId) ?? null);
  if (view.activeId !== null && !active) return [];
  const wanted = people.filter((p) => !p.you && (active === null || p.id === active.hubPersonId));
  return wanted.flatMap((person) =>
    person.machines.map((machine) => ({ person, machine, title: `${person.name} · ${machine.name}`, key: `${person.id}/${machine.id}` })),
  );
}

/** A last-seen time as epoch milliseconds, or null when there is none or it cannot be read. */
export function lastSeenMs(v: string | number | null | undefined): number | null {
  if (v === null || v === undefined || v === '') return null;
  const ms = typeof v === 'number' ? v : Date.parse(v);
  return Number.isFinite(ms) ? ms : null;
}

/** "offline since 14:02" / "offline" for a machine that has not connected. */
export function offlineText(m: PeerMachine, now: number): string {
  if (m.online) return 'online';
  const seen = lastSeenMs(m.lastSeen);
  if (seen === null) return 'offline';
  const sameDay = new Date(seen).toDateString() === new Date(now).toDateString();
  const when = new Date(seen);
  return `offline since ${sameDay ? when.toLocaleTimeString(undefined, { hour: '2-digit', minute: '2-digit' }) : when.toLocaleDateString()}`;
}

/** Newest first, chats that need the person first of all. */
export function orderPeerChats(chats: readonly PeerChat[]): PeerChat[] {
  return [...chats].sort((a, b) => Number(!!b.needsAttention) - Number(!!a.needsAttention) || b.updatedAt - a.updatedAt);
}

/** What a chat's profile line says in the profile view: where its chats run. */
export const SIGN_IN_NOTE = "Chats here run on this computer's Claude sign-in, whichever profile they are labelled with.";
