// How a pull request address becomes a tab: the settings gate, the open itself, and the rule for
// which view an already-open tab shows.

import { openInPanel, panelStore } from '../panel/registry.ts';
import { mergeOpen, prKey } from './links.ts';
import type { PrTabData } from './types.ts';

export const PR_KIND = 'pr';

let cached: { at: number; enabled: boolean } | null = null;
const CACHE_MS = 5_000;

/** Whether the PR view switch is on. A host that cannot answer counts as off, so links open in the browser. */
export async function prViewEnabled(now = Date.now()): Promise<boolean> {
  if (cached && now - cached.at < CACHE_MS) return cached.enabled;
  const api = panelStore.api;
  if (!api) return false;
  try {
    const s = await api<{ enabled?: boolean }>('github_settings_get', {});
    cached = { at: now, enabled: s?.enabled === true };
  } catch {
    cached = { at: now, enabled: false };
  }
  return cached.enabled;
}

export function forgetPrSettings(): void {
  cached = null;
}

const same = (a: PrTabData, b: PrTabData) => a.view === b.view && a.anchor?.id === b.anchor?.id && a.anchor?.kind === b.anchor?.kind;

/** Opens (or reselects) the tab for a pull request. False when the panel has no room. */
export function openPrTab(data: PrTabData, chatId: string | null): boolean {
  const key = prKey(data);
  const existing = panelStore.model.tabs.find((t) => t.kind === PR_KIND && t.key === key);
  const prev = (existing?.data as PrTabData | undefined) ?? null;
  const next = mergeOpen(prev, data);
  // Repeating exactly the same open leaves the tab's data object untouched.
  return openInPanel(PR_KIND, key, prev && same(prev, next) ? prev : { owner: data.owner, repo: data.repo, number: data.number, view: next.view, ...(next.anchor ? { anchor: next.anchor } : {}) }, { intent: 'user', chatId });
}
