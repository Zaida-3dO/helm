// What a `file` tab keeps in its data and what survives a restart: the path and line it opened
// at, plus how the person was looking at it (Preview or Source, the first visible line) and whether
// it was opened from the Files tree. Pure so the saved shape is tested without a browser.

import { driveForm, isRemoteOrDevicePath } from '../../markdown/paths.ts';

export type ViewMode = 'preview' | 'source';

export interface SavedFileTab {
  path: string;
  line?: number;
  mode?: ViewMode;
  /** The first visible line of the Source view when the tab was last shown. */
  top?: number;
  fromFiles?: boolean;
}

/** Only a complete local path survives a restore. */
export function validLocalPath(p: unknown): p is string {
  return typeof p === 'string' && p.length <= 8192 && driveForm(p) === 'absolute' && !isRemoteOrDevicePath(p);
}

const positive = (n: unknown): n is number => typeof n === 'number' && Number.isInteger(n) && n >= 1 && n <= 10_000_000;

/** The parts of a tab's data worth keeping, in the stored shape. */
export function saveFileTab(d: SavedFileTab): SavedFileTab {
  const out: SavedFileTab = { path: d.path };
  if (d.line) out.line = d.line;
  if (d.mode === 'preview' || d.mode === 'source') out.mode = d.mode;
  if (d.top) out.top = d.top;
  if (d.fromFiles) out.fromFiles = true;
  return out;
}

/** Reads a stored entry leniently: a bad field is dropped, a bad path drops the whole tab (null). */
export function restoreFileTab(data: unknown): SavedFileTab | null {
  const d = data as Partial<SavedFileTab> | null;
  if (!d || typeof d !== 'object' || !validLocalPath(d.path)) return null;
  const out: SavedFileTab = { path: d.path };
  if (positive(d.line)) out.line = d.line;
  if (d.mode === 'preview' || d.mode === 'source') out.mode = d.mode;
  if (positive(d.top)) out.top = d.top;
  if (d.fromFiles === true) out.fromFiles = true;
  return out;
}
