// The Files slot's folder tree as plain data: which rows are shown for the folders opened so far,
// and what each arrow key, Enter and Home/End does. No React and no DOM.

import type { DirEntry } from '../../api/panel.ts';
import { joinPath, pathKey } from './paths.ts';

export type Listing =
  | { status: 'loading' }
  | { status: 'error'; message: string }
  | { status: 'ready'; entries: DirEntry[]; truncated: boolean; modified?: number };

export type Row =
  | { type: 'entry'; path: string; name: string; kind: DirEntry['kind']; depth: number; open: boolean }
  | { type: 'note'; id: string; depth: number; text: string; retry?: string };

/**
 * The rows under `root`, folders open as recorded in `expanded` (lower-cased paths). A folder that
 * is open but not listed yet shows a loading line; a failed one an error line with a retry.
 */
export function treeRows(root: string, expanded: ReadonlySet<string>, listings: ReadonlyMap<string, Listing>, showHidden: boolean): Row[] {
  const out: Row[] = [];
  const walk = (dir: string, depth: number) => {
    const listing = listings.get(pathKey(dir));
    if (!listing || listing.status === 'loading') {
      out.push({ type: 'note', id: `${pathKey(dir)}#loading`, depth, text: 'Loading…' });
      return;
    }
    if (listing.status === 'error') {
      out.push({ type: 'note', id: `${pathKey(dir)}#error`, depth, text: 'Couldn’t list this folder', retry: dir });
      return;
    }
    const shown = listing.entries.filter((e) => showHidden || !e.name.startsWith('.'));
    if (shown.length === 0) {
      out.push({ type: 'note', id: `${pathKey(dir)}#empty`, depth, text: 'This folder is empty' });
      return;
    }
    for (const e of shown) {
      const path = joinPath(dir, e.name);
      const open = e.kind === 'dir' && expanded.has(pathKey(path));
      out.push({ type: 'entry', path, name: e.name, kind: e.kind, depth, open });
      if (open) walk(path, depth + 1);
    }
    if (listing.truncated) out.push({ type: 'note', id: `${pathKey(dir)}#more`, depth, text: 'Listing limited to 1,000 entries' });
  };
  walk(root, 0);
  return out;
}

export type TreeAction =
  | { type: 'focus'; index: number }
  | { type: 'toggle'; path: string; open: boolean }
  | { type: 'open'; path: string };

/**
 * What a key does on the row at `index`: Up/Down and Home/End move between entry rows; Right opens
 * a folder, or steps into an open one; Left closes an open folder, or steps out to its parent; Enter
 * opens a file in a tab and toggles a folder. Anything else (or nothing to do) is null.
 */
export function treeKey(rows: readonly Row[], index: number, key: string): TreeAction | null {
  const here = rows[index];
  if (!here || here.type !== 'entry') return null;
  const entries = rows.flatMap((r, i) => (r.type === 'entry' ? [i] : []));
  const at = entries.indexOf(index);
  switch (key) {
    case 'ArrowDown':
      return at < entries.length - 1 ? { type: 'focus', index: entries[at + 1] as number } : null;
    case 'ArrowUp':
      return at > 0 ? { type: 'focus', index: entries[at - 1] as number } : null;
    case 'Home':
      return at > 0 ? { type: 'focus', index: entries[0] as number } : null;
    case 'End':
      return at < entries.length - 1 ? { type: 'focus', index: entries[entries.length - 1] as number } : null;
    case 'ArrowRight': {
      if (here.kind !== 'dir') return null;
      if (!here.open) return { type: 'toggle', path: here.path, open: true };
      const next = rows[index + 1];
      return next?.type === 'entry' ? { type: 'focus', index: index + 1 } : null;
    }
    case 'ArrowLeft': {
      if (here.kind === 'dir' && here.open) return { type: 'toggle', path: here.path, open: false };
      for (let i = index - 1; i >= 0; i--) {
        const r = rows[i] as Row;
        if (r.type === 'entry' && r.depth < here.depth) return { type: 'focus', index: i };
      }
      return null;
    }
    case 'Enter':
      if (here.kind === 'dir') return { type: 'toggle', path: here.path, open: !here.open };
      return here.kind === 'file' ? { type: 'open', path: here.path } : null;
    default:
      return null;
  }
}
