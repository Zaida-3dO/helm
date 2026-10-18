// The settings registry. Each area adds one `registerSettingsPane` line for its
// own pane; the settings frame (SettingsDialog.tsx) lists them by `order`, then title.

import type { ReactNode } from 'react';

export interface SettingsPane {
  /** Stable id; also the `data-pane` test hook. */
  id: string;
  title: string;
  order: number;
  render: () => ReactNode;
}

const panes = new Map<string, SettingsPane>();
const listeners = new Set<() => void>();
let snapshot: SettingsPane[] = [];

function changed(): void {
  snapshot = [...panes.values()].sort((a, b) => a.order - b.order || a.title.localeCompare(b.title));
  for (const l of [...listeners]) l();
}

/** Register (or replace) a pane. Returns the unregister function. */
export function registerSettingsPane(pane: SettingsPane): () => void {
  panes.set(pane.id, pane);
  changed();
  return () => {
    if (panes.get(pane.id) === pane) {
      panes.delete(pane.id);
      changed();
    }
  };
}

/** The panes in display order (a stable array between changes). */
export function settingsPanes(): SettingsPane[] {
  return snapshot;
}

export function subscribeSettingsPanes(l: () => void): () => void {
  listeners.add(l);
  return () => {
    listeners.delete(l);
  };
}

/** Panes whose title matches the search text (case-insensitive); all of them for empty text. */
export function filterPanes(list: SettingsPane[], query: string): SettingsPane[] {
  const q = query.trim().toLowerCase();
  if (!q) return list;
  return list.filter((p) => p.title.toLowerCase().includes(q));
}

// A pane to show when the dialog next opens ("Open Settings → Usage" from the top-bar meter).
let requested: string | null = null;

/** Ask for a pane to be selected the next time the settings dialog opens. */
export function requestPane(id: string): void {
  requested = id;
}

/** The requested pane id, if any (read twice by StrictMode's double render, so it does not clear). */
export function peekRequestedPane(): string | null {
  return requested;
}

export function clearRequestedPane(): void {
  requested = null;
}
