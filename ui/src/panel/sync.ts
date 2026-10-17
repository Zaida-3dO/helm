// Saved tabs, wired to the host: read once per launch, restore as program intent, and
// only then start writing, debounced, and only when the value changed. Nothing is written before
// the read settles, so a boot-time empty panel never erases the saved tabs.

import type { PanelOps } from '../api/panel.ts';
import { restoreTabs, serializeTabs } from './persist.ts';
import { panelStore } from './registry.ts';

export const SAVE_DEBOUNCE_MS = 1000;

export function startTabSync(ops: Pick<PanelOps, 'tabsGet' | 'tabsSave'>, debounceMs = SAVE_DEBOUNCE_MS): () => void {
  let stopped = false;
  let last: string | null = null;
  let timer: ReturnType<typeof setTimeout> | null = null;
  let unsubscribe: (() => void) | null = null;
  const kindOf = (k: string) => {
    const kind = panelStore.kinds.get(k);
    return kind ? { scope: kind.scope ?? 'chat', persist: kind.persist } : undefined;
  };

  const flush = () => {
    timer = null;
    const value = serializeTabs(panelStore.model.tabs, kindOf);
    const text = JSON.stringify(value);
    if (text === last) return;
    last = text;
    ops.tabsSave(value).catch((e: unknown) => console.warn('saving panel tabs failed', e));
  };

  const begin = (savedText: string | null) => {
    if (stopped) return;
    last = savedText;
    unsubscribe = panelStore.subscribe(() => {
      if (timer) clearTimeout(timer);
      timer = setTimeout(flush, debounceMs);
    });
  };

  ops
    .tabsGet()
    .then((value) => {
      if (stopped) return;
      const restored = restoreTabs(value, kindOf);
      panelStore.update((m) => {
        for (const r of restored) m.restore(r.set, r.kind, r.key, r.data);
      });
      begin(JSON.stringify(serializeTabs(panelStore.model.tabs, kindOf)));
    })
    .catch(() => begin(null));

  return () => {
    stopped = true;
    if (timer) clearTimeout(timer);
    unsubscribe?.();
  };
}
