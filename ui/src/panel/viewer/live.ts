// Following a file on disk: when a change is worth a re-read, what the viewer last showed, and the
// small pieces of text and timing the "updated" dot and the tab badges share. Pure apart from the
// one table of last-shown times; the polling itself lives with the viewer and the tab strip.

import type { FileStamp, FileView } from '../../api/panel.ts';
import { pathKey } from './paths.ts';

/** How often the file on screen is checked for an edit. */
export const LIVE_POLL_MS = 1000;
/** How often the other file tabs are checked, for their badge. */
export const BACKGROUND_POLL_MS = 2500;
/** Most files checked in one go. */
export const MAX_WATCHED = 8;
/** How long the "updated" dot stays after a change. */
export const UPDATED_DOT_MS = 5000;

/** The modified time each path's viewer last showed (by lower-cased path), for tab badges. */
const shown = new Map<string, number>();

export function noteShown(path: string, modified: number | undefined): void {
  if (modified === undefined) shown.delete(pathKey(path));
  else shown.set(pathKey(path), modified);
}

export function shownModified(path: string): number | undefined {
  return shown.get(pathKey(path));
}

/** A file counts as changed only when its time differs from one we already knew; a missing file is not a change. */
export function changedSince(before: number | undefined, now: FileStamp | null | undefined): boolean {
  return before !== undefined && now != null && now.modified !== before;
}

/** The first `max` items, for a poll that may only watch a few files at once. */
export function watchedSlice<T>(items: readonly T[], max: number = MAX_WATCHED): T[] {
  return items.slice(0, max);
}

/** True when a fresh read shows the same thing as the last one, so nothing needs to be redrawn. */
export function sameContent(a: FileView | null, b: FileView): boolean {
  if (!a) return false;
  return (
    a.kind === b.kind &&
    a.size === b.size &&
    a.truncated === b.truncated &&
    a.url === b.url &&
    a.text === b.text &&
    a.bytes === b.bytes &&
    JSON.stringify(a.entries ?? null) === JSON.stringify(b.entries ?? null)
  );
}

/** "updated 3s ago" for the dot; under a second reads "updated just now". */
export function updatedLabel(ms: number): string {
  const s = Math.floor(Math.max(0, ms) / 1000);
  return s < 1 ? 'updated just now' : `updated ${s}s ago`;
}
