// Settings that other parts of the window read: the sidebar entries hidden, the skills hidden from
// the / menu.

import { useEffect, useState } from 'react';
import { useShell } from '../shell/context.ts';

/** A stored list of names as a clean list: strings only, trimmed, none empty, none repeated. */
export function parseNameList(v: unknown): string[] {
  if (!Array.isArray(v)) return [];
  const out: string[] = [];
  for (const x of v) {
    const n = typeof x === 'string' ? x.trim() : '';
    if (n && !out.includes(n)) out.push(n);
  }
  return out;
}

/** The sidebar entries Settings can hide, by the id stored in `hiddenSidebarItems`. */
export const SIDEBAR_ENTRIES: { id: string; title: string; description: string }[] = [
  { id: 'newChat', title: 'New chat', description: 'The New chat entry at the top of the sidebar.' },
  { id: 'search', title: 'Search chats', description: 'The Search chats entry.' },
  { id: 'attention', title: 'Needs attention', description: 'The Needs attention entry and its count.' },
];

/** `hiddenSkills`, live: re-read when the preference changes. */
export function useHiddenSkills(): string[] {
  const shell = useShell();
  const [hidden, setHidden] = useState<string[]>([]);
  useEffect(() => {
    const read = () =>
      shell
        .api<unknown>('preference_get', { key: 'hiddenSkills' })
        .then((v) => setHidden(parseNameList(v)))
        .catch(() => undefined);
    void read();
    return shell.bus.on('preferences', (e) => {
      if ('hiddenSkills' in ((e as { changed?: object }).changed ?? {})) void read();
    });
  }, [shell]);
  return hidden;
}
