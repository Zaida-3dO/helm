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
