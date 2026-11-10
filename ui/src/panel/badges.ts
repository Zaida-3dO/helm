// Tab badges: a small dot on a file tab that changed on disk while it was not the one being read.
// A few file tabs at a time are checked by modified time (never contents), only while the panel is
// shown and the window has focus. Selecting the tab clears its dot; its viewer reads the file fresh.

import { useEffect, useRef } from 'react';
import type { PanelOps } from '../api/panel.ts';
import type { PanelTab } from './model.ts';
import { panelStore } from './registry.ts';
import { BACKGROUND_POLL_MS, changedSince, shownModified, watchedSlice } from './viewer/live.ts';

interface FileTabData {
  path: string;
  current?: string;
  currentKind?: string;
}

/** The tabs with an unseen change. */
export const badged = new Set<string>();
/** The modified time first seen for a tab the viewer never showed (a restored one). */
const seen = new Map<string, number>();

const pathOf = (t: PanelTab) => {
  const d = t.data as FileTabData;
  return d.current ?? d.path;
};

/** The file tabs worth checking: not the one on screen, not a folder, not already badged. */
export function badgeCandidates(tabs: readonly PanelTab[], active: string | null): PanelTab[] {
  return watchedSlice(tabs.filter((t) => t.kind === 'file' && t.id !== active && !badged.has(t.id) && (t.data as FileTabData).currentKind !== 'dir'));
}

export function useTabBadges(ops: Pick<PanelOps, 'fileStat'>, tabs: readonly PanelTab[], active: string | null, on: boolean): void {
  // The timer reads the latest tabs without restarting on every render.
  const latest = useRef({ tabs, active });
  latest.current = { tabs, active };
  // The tab being read is up to date by definition.
  useEffect(() => {
    if (!active) return;
    const wasBadged = badged.delete(active);
    const wasSeen = seen.delete(active);
    if (wasBadged || wasSeen) panelStore.changed();
  }, [active]);

  useEffect(() => {
    if (!on) return;
    let busy = false;
    const timer = setInterval(() => {
      if (busy) return;
      const list = badgeCandidates(latest.current.tabs, latest.current.active);
      if (list.length === 0) return;
      busy = true;
      ops
        .fileStat(list.map(pathOf))
        .then((stamps) => {
          let flagged = false;
          stamps.forEach((stamp, i) => {
            const tab = list[i] as PanelTab;
            if (!stamp) return;
            const before = shownModified(pathOf(tab)) ?? seen.get(tab.id);
            if (before === undefined) seen.set(tab.id, stamp.modified);
            else if (changedSince(before, stamp)) {
              badged.add(tab.id);
              flagged = true;
            }
          });
          if (flagged) panelStore.changed();
        })
        .catch(() => undefined)
        .finally(() => {
          busy = false;
        });
    }, BACKGROUND_POLL_MS);
    return () => clearInterval(timer);
  }, [ops, on]);
}

/** Forgets a closed tab's dot. */
export function forgetBadge(id: string): void {
  badged.delete(id);
  seen.delete(id);
}
