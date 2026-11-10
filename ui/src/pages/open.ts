// Opening a page as a side-panel tab. The tab is app-wide (a pinned page is not one chat's), keyed
// by the file's path so a page is never open twice.

import { openInPanel } from '../panel/registry.ts';
import { displayPath, pathKey } from '../panel/viewer/paths.ts';

export const PAGE_KIND = 'page';

export interface PageTabData {
  /** The page file, absolute. */
  path: string;
}

/** Only a complete local path survives a restore. */
export function validPagePath(p: unknown): p is string {
  return typeof p === 'string' && p.length <= 8192 && /^[A-Za-z]:[\\/]/.test(p) && !/\p{Cc}/u.test(p);
}

/** Opens the page in the side panel. `false` means the panel has no room for another tab. */
export function openPageTab(path: string): boolean {
  const shown = displayPath(path);
  const data: PageTabData = { path: shown };
  return openInPanel(PAGE_KIND, pathKey(shown), data, { intent: 'user', chatId: null });
}
