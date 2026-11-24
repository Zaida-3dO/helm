// The `page` panel kind: a local HTML page running in its sealed frame. The frame stays mounted
// while its tab is in the background (a page keeps its place), but is read and polled only while
// the tab is shown.

import { useSyncExternalStore } from 'react';
import { IconBrowser } from '../ui/icons.tsx';
import { registerPanelKind, panelStore } from '../panel/registry.ts';
import type { TabContext } from '../panel/registry.ts';
import type { PanelTab } from '../panel/model.ts';
import { basename } from '../panel/viewer/paths.ts';
import { PageFrame } from './PageFrame.tsx';
import { PAGE_KIND, validPagePath, validRemote } from './open.ts';
import type { PageTabData } from './open.ts';

function PageTab({ tab, ctx }: { tab: PanelTab; ctx: TabContext }) {
  const data = tab.data as PageTabData;
  // Messages from an unlinked page go to the chat in view.
  const current = useSyncExternalStore(panelStore.subscribe, () => panelStore.current);
  const api = panelStore.api;
  if (!api) return <div className="sp-empty">The side panel is not connected to the host yet.</div>;
  return <PageFrame api={api} path={data.path} remote={data.remote} chatId={current} visible={ctx.visible} onTitle={ctx.setTitle} />;
}

registerPanelKind({
  kind: PAGE_KIND,
  title: (tab) => {
    const d = tab.data as PageTabData;
    return d.remote?.title ?? basename(d.path).replace(/\.html?$/i, '');
  },
  icon: () => <IconBrowser size={14} />,
  maxTabs: 6,
  keepMounted: true,
  render: (tab, ctx) => <PageTab tab={tab as PanelTab} ctx={ctx} />,
  persist: {
    version: 1,
    save: (tab) => ({ path: (tab.data as PageTabData).path, remote: (tab.data as PageTabData).remote }),
    restore: (data) => {
      const d = data as { path?: unknown; remote?: unknown } | null;
      if (d && validRemote(d.remote)) return { path: typeof d.path === 'string' ? d.path.slice(0, 8192) : '', remote: d.remote };
      return d && validPagePath(d.path) ? { path: d.path } : null;
    },
  },
});
