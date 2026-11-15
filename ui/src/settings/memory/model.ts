// Settings → Memory: the shapes the host answers with (generated bindings, re-exported here) and
// the pure helpers the pane renders with. What a source's person field leaves empty is the home
// profile's material; the host refuses the same problems this file can see without a round trip.

import type { MemindexKind, MemindexScope, MemindexSettingsChanges, MemindexSource, MemindexSourceInput, MemindexState } from '../../api/generated/memindex/index.ts';

export type { MemindexKind, MemindexScope, MemindexSettingsChanges, MemindexSource, MemindexSourceInput, MemindexState };

export const KIND_OPTIONS: { value: MemindexKind; title: string; subtitle: string }[] = [
  { value: 'folder', title: 'Notes folder', subtitle: 'A folder of markdown notes, watched as they change' },
  { value: 'chat_logs', title: 'Chat logs', subtitle: 'Saved conversation logs, one file per week' },
  { value: 'helm_chats', title: 'Helm chats', subtitle: 'This computer’s Helm chats, one turn at a time' },
  { value: 'claude_code', title: 'Claude Code sessions', subtitle: 'Claude Code’s own session records on this computer' },
];

export const KIND_TITLES: Record<MemindexKind, string> = Object.fromEntries(KIND_OPTIONS.map((k) => [k.value, k.title])) as Record<MemindexKind, string>;

/** Kinds that read a folder the user names; the rest read what this computer keeps. */
export function kindHasPath(kind: MemindexKind): boolean {
  return kind === 'folder' || kind === 'chat_logs';
}

/** Kinds this computer can only have once. */
export function kindIsSingle(kind: MemindexKind): boolean {
  return kind === 'helm_chats' || kind === 'claude_code';
}

export const SCOPE_OPTIONS: { value: MemindexScope; title: string; subtitle: string }[] = [
  { value: 'me', title: 'Only me', subtitle: "Only this person's own searches find it" },
  { value: 'everyone', title: 'Everyone', subtitle: "Everyone's searches can find it, ranked below their own" },
];

/** A folder path that is at least a full path on this computer (the host also checks it exists). */
export function pathProblem(path: string): string | null {
  const p = path.trim();
  if (!p) return 'Choose a folder.';
  if (!/^([a-zA-Z]:[\\/]|\\\\)/.test(p)) return 'The folder must be a full path.';
  return null;
}

/** The globs text area as a list: one per line, trimmed, blank lines and repeats dropped. */
export function parseGlobs(text: string): string[] {
  const seen = new Set<string>();
  const out: string[] = [];
  for (const line of text.split(/\r?\n/)) {
    const g = line.trim();
    if (g && !seen.has(g)) {
      seen.add(g);
      out.push(g);
    }
  }
  return out;
}

/** A problem with the globs list the host would refuse too, or null. */
export function globsProblem(globs: readonly string[]): string | null {
  if (globs.length > 50) return 'List at most 50 patterns.';
  if (globs.some((g) => g.length > 200)) return 'One of the patterns is too long.';
  return null;
}

/** The embedding model name the host accepts; empty means its default model. */
export function modelProblem(name: string): string | null {
  const m = name.trim();
  if (!m) return null;
  if (m.length > 100) return 'The model name is too long.';
  if (!/^[A-Za-z0-9._:/-]+$/.test(m)) return 'The model name may hold letters, digits and . - _ : / only.';
  return null;
}

/** The nightly scan time, `HH:MM` like the host wants it. */
export function nightlyProblem(text: string): string | null {
  if (/^([01]\d|2[0-3]):[0-5]\d$/.test(text.trim())) return null;
  return 'The nightly scan time is HH:MM, like 03:30.';
}

/** What the pane says about the line to the hub, and how loudly. */
export function hubNotice(state: Pick<MemindexState, 'hub'>): { text: string; tone: 'ok' | 'bad' | 'idle' } {
  if (state.hub === 'ready') return { text: 'The hub is taking index updates.', tone: 'ok' };
  if (state.hub === 'too_old') return { text: 'The hub needs updating for search.', tone: 'bad' };
  return { text: 'Waiting for the hub: search keeps working with what it already has.', tone: 'idle' };
}

/** The line about chunks still waiting to be embedded, or null when there is nothing to say. */
export function pendingText(pending: number | null): string | null {
  if (pending === null || pending === 0) return null;
  return `${pending.toLocaleString('en-US')} chunk${pending === 1 ? '' : 's'} waiting to be embedded.`;
}

/** "24 docs here, 24 on the hub". */
export function docsText(docs: number, pushed: number): string {
  const fmt = (n: number) => n.toLocaleString('en-US');
  return `${fmt(docs)} doc${docs === 1 ? '' : 's'} here, ${fmt(pushed)} on the hub`;
}

/** When a source was last looked at, in plain words; `now` keeps it testable. */
export function scanText(lastScan: number, now: number): string {
  if (!lastScan) return 'not scanned yet';
  const mins = Math.max(0, Math.floor((now - lastScan) / 60_000));
  if (mins < 1) return 'scanned just now';
  if (mins < 60) return `scanned ${mins} min ago`;
  if (mins < 24 * 60) return `scanned ${Math.round(mins / 60)} h ago`;
  if (mins < 7 * 24 * 60) return `scanned ${Math.round(mins / (24 * 60))} days ago`;
  const d = new Date(lastScan);
  const sameYear = new Date(now).getFullYear() === d.getFullYear();
  return `scanned ${d.toLocaleDateString('en-GB', { day: 'numeric', month: 'short', ...(sameYear ? {} : { year: 'numeric' }) })}`;
}

/** What a source card can change without a round trip being refused; a draft needs a kind and, for folder kinds, a folder. */
export function draftProblem(draft: { kind: MemindexKind | null; path: string }): string | null {
  if (!draft.kind) return 'Say what kind of source this is.';
  if (kindHasPath(draft.kind)) return pathProblem(draft.path);
  return null;
}

/** A source card as the pane edits it; `id` empty means not saved yet. */
export interface SourceDraft {
  key: string;
  id: string;
  kind: MemindexKind | null;
  path: string;
  personId: string;
  scope: MemindexScope;
  include: string[];
  exclude: string[];
  enabled: boolean;
}

export function toDraft(s: MemindexSource): SourceDraft {
  return { key: s.id, id: s.id, kind: s.kind, path: s.path, personId: s.personId, scope: s.scope, include: s.include, exclude: s.exclude, enabled: s.enabled };
}

let fresh = 0;
/** A source the user is adding: a kind chosen now, the rest filled in on the card. */
export function newDraft(kind: MemindexKind): SourceDraft {
  fresh += 1;
  return { key: `new-${fresh}`, id: '', kind, path: '', personId: '', scope: 'me', include: [], exclude: [], enabled: true };
}

/** What `memindex_sources_save` takes for a draft (no id: the host gives it one). */
export function addPayload(d: SourceDraft): MemindexSourceInput {
  const hasPath = d.kind !== null && kindHasPath(d.kind);
  return {
    id: null,
    kind: d.kind,
    path: hasPath ? d.path.trim() : null,
    personId: d.personId || null,
    scope: d.scope,
    include: hasPath ? d.include : null,
    exclude: hasPath ? d.exclude : null,
    enabled: d.enabled,
  };
}

/** A settings change carrying only the fields the pane actually changed. */
export function settingsChanges(patch: Partial<Pick<MemindexSettingsChanges, 'enabled' | 'serverId' | 'model' | 'nightly'>>): MemindexSettingsChanges {
  return {
    enabled: patch.enabled ?? null,
    serverId: patch.serverId ?? null,
    model: patch.model ?? null,
    nightly: patch.nightly ?? null,
  };
}
