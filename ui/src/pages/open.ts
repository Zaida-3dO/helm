// Opening a page as a side-panel tab. The tab is app-wide (a pinned page is not one chat's), keyed
// by the file's path so a page is never open twice.

import { openInPanel } from '../panel/registry.ts';
import { displayPath, pathKey } from '../panel/viewer/paths.ts';

export const PAGE_KIND = 'page';

/** A page pinned on another PC: which PC, and which pin there. */
export interface PageRemote {
  deviceUid: string;
  pageId: string;
  title: string;
}

export interface PageTabData {
  /** The page file, absolute (on the other PC for a remote page, shown only). */
  path: string;
  remote?: PageRemote;
}

/** Only a complete remote reference survives a restore. */
export function validRemote(r: unknown): r is PageRemote {
  if (!r || typeof r !== 'object') return false;
  const o = r as Record<string, unknown>;
  const ok = (v: unknown, max: number) => typeof v === 'string' && v.length > 0 && v.length <= max && !/\p{Cc}/u.test(v);
  return ok(o.deviceUid, 200) && ok(o.pageId, 200) && ok(o.title, 300);
}

/** The tab's key for a page on another PC: never the same as a local page's path. */
export function remoteTabKey(deviceUid: string, pageId: string): string {
  return `remote:${deviceUid}:${pageId}`;
}

/** Only a complete local path survives a restore. */
export function validPagePath(p: unknown): p is string {
  return typeof p === 'string' && p.length <= 8192 && /^[A-Za-z]:[\\/]/.test(p) && !/\p{Cc}/u.test(p);
}

/** Opens a page pinned on another PC in the side panel. `false` means the panel has no room. */
export function openRemotePageTab(deviceUid: string, page: { id: string; path: string; title: string }): boolean {
  const data: PageTabData = { path: page.path, remote: { deviceUid, pageId: page.id, title: page.title } };
  return openInPanel(PAGE_KIND, remoteTabKey(deviceUid, page.id), data, { intent: 'user', chatId: null });
}

/** Opens the page in the side panel. `false` means the panel has no room for another tab. */
export function openPageTab(path: string): boolean {
  const shown = displayPath(path);
  const data: PageTabData = { path: shown };
  return openInPanel(PAGE_KIND, pathKey(shown), data, { intent: 'user', chatId: null });
}
