// `panelTabs` v1: which tabs survive a restart. The renderer trims to the host's limits
// before writing (skipping what does not fit rather than failing the whole write); the host
// re-validates and refuses an invalid value whole. Restored tabs come back as program intent.

import { APP_SET } from './model.ts';
import type { PanelTab } from './model.ts';

export const MAX_ENTRIES = 48;
export const MAX_TOTAL_BYTES = 64 * 1024;
export const MAX_KEY_BYTES = 4096;
export const MAX_DATA_BYTES = 4096;
export const MAX_KIND_CHARS = 32;

export interface StoredEntry {
  kind: string;
  key: string;
  v: number;
  data: unknown;
}

export interface StoredTabs {
  version: 1;
  sets: Record<string, StoredEntry[]>;
}

export interface Persistable {
  scope: 'chat' | 'app';
  persist?: {
    version: number;
    save: (tab: Readonly<PanelTab>) => unknown;
    restore: (data: unknown, version: number) => unknown;
  };
}

// eslint-disable-next-line no-control-regex
const CONTROL = /[\u0000-\u001f\u007f]/;
const bytes = (s: string) => new TextEncoder().encode(s).length;

export function emptyTabs(): StoredTabs {
  return { version: 1, sets: {} };
}

/** Builds the stored value from the open tabs, in strip order, within every limit. */
export function serializeTabs(tabs: readonly PanelTab[], kindOf: (kind: string) => Persistable | undefined): StoredTabs {
  const out = emptyTabs();
  let count = 0;
  // The envelope's own bytes, counted once.
  let total = bytes(JSON.stringify(out));
  for (const tab of tabs) {
    if (count >= MAX_ENTRIES) break;
    const persist = kindOf(tab.kind)?.persist;
    if (!persist) continue;
    if (tab.kind.length > MAX_KIND_CHARS || CONTROL.test(tab.kind) || CONTROL.test(tab.key)) continue;
    if (bytes(tab.key) > MAX_KEY_BYTES) continue;
    let data: unknown;
    try {
      data = persist.save(tab);
    } catch {
      continue;
    }
    if (data === null || data === undefined) continue;
    const dataJson = JSON.stringify(data);
    if (dataJson === undefined || bytes(dataJson) > MAX_DATA_BYTES) continue;
    const entry: StoredEntry = { kind: tab.kind, key: tab.key, v: persist.version, data };
    const isNewSet = !out.sets[tab.set];
    const cost = bytes(JSON.stringify(entry)) + 1 + (isNewSet ? bytes(JSON.stringify(tab.set)) + 3 : 0);
    if (total + cost > MAX_TOTAL_BYTES) continue;
    (out.sets[tab.set] ??= []).push(entry);
    total += cost;
    count++;
  }
  return out;
}

export interface RestoredTab {
  set: string;
  kind: string;
  key: string;
  data: unknown;
}

/**
 * Reads a stored value leniently. Entries whose kind is unknown, has no `persist`, mismatches its
 * set's scope, repeats one already restored, or whose `restore` returns null or throws are dropped.
 */
export function restoreTabs(
  value: unknown,
  kindOf: (kind: string) => Persistable | undefined,
  chatExists: (chatId: string) => boolean = () => true,
): RestoredTab[] {
  if (!value || typeof value !== 'object') return [];
  const v = value as { version?: unknown; sets?: unknown };
  if (v.version !== 1 || !v.sets || typeof v.sets !== 'object') return [];
  const out: RestoredTab[] = [];
  const seen = new Set<string>();
  for (const [set, entries] of Object.entries(v.sets as Record<string, unknown>)) {
    if (!Array.isArray(entries)) continue;
    if (set !== APP_SET && set !== '' && !chatExists(set)) continue;
    for (const raw of entries) {
      if (!raw || typeof raw !== 'object') continue;
      const e = raw as Partial<StoredEntry>;
      if (typeof e.kind !== 'string' || typeof e.key !== 'string' || typeof e.v !== 'number') continue;
      const kind = kindOf(e.kind);
      if (!kind?.persist) continue;
      if ((kind.scope === 'app') !== (set === APP_SET)) continue;
      const id = `${set}\u0000${e.kind}\u0000${e.key}`;
      if (seen.has(id)) continue;
      let data: unknown;
      try {
        data = kind.persist.restore(e.data, e.v);
      } catch {
        continue;
      }
      if (data === null || data === undefined) continue;
      seen.add(id);
      out.push({ set, kind: e.kind, key: e.key, data });
    }
  }
  return out;
}
