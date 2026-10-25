// Settings → Keyboard: which commands the pane lists, in what order.

export interface KeyRow {
  id: string;
  title: string;
  chord: string | null;
  custom: boolean;
  scope?: 'app' | 'global';
}

/**
 * The rows to show: app-scope commands only (the global hotkey is set in Settings → Shell), sorted
 * by title, matching the search text in the title or the chord (case-insensitive).
 */
export function keyboardRows<T extends KeyRow>(list: readonly T[], query: string): T[] {
  const q = query.trim().toLowerCase();
  return list
    .filter((b) => (b.scope ?? 'app') === 'app')
    .filter((b) => !q || b.title.toLowerCase().includes(q) || (b.chord ?? '').toLowerCase().includes(q))
    .sort((a, b) => a.title.localeCompare(b.title));
}

/** The ids sharing a chord with another command, for a warning mark on their rows. */
export function conflictedIds(conflicts: readonly { ids: string[] }[]): Set<string> {
  return new Set(conflicts.flatMap((c) => c.ids));
}

/** The sections the shortcuts are listed under, in display order. */
export const KEY_SECTIONS = ['Workspace', 'Conversation', 'Composer', 'Window'] as const;
export type KeySection = (typeof KEY_SECTIONS)[number];

/** Which section a command belongs to, from its id. Anything unknown lands under Window. */
export function sectionOf(id: string): KeySection {
  if (id.startsWith('composer.')) return 'Composer';
  if (/^(chat\.(new|next|previous)$|sidebar\.|project\.|panel\.|terminal\.|search\.)/.test(id)) return 'Workspace';
  if (id.startsWith('chat.')) return 'Conversation';
  return 'Window';
}

/** The rows split into their sections (empty sections left out), each already in the order given. */
export function groupKeyboardRows<T extends { id: string }>(rows: readonly T[]): { section: KeySection; rows: T[] }[] {
  return KEY_SECTIONS.map((section) => ({ section, rows: rows.filter((r) => sectionOf(r.id) === section) })).filter((g) => g.rows.length > 0);
}
