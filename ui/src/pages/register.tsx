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
import { PAGE_KIND, validPagePath } from './open.ts';
import type { PageTabData } from './open.ts';

function PageTab({ tab, ctx }: { tab: PanelTab; ctx: TabContext }) {
  const data = tab.data as PageTabData;
  // Messages from an unlinked page go to the chat in view.
  const current = useSyncExternalStore(panelStore.subscribe, () => panelStore.current);
  const api = panelStore.api;
  if (!api) return <div className="sp-empty">The side panel is not connected to the host yet.</div>;
  return <PageFrame api={api} path={data.path} chatId={current} visible={ctx.visible} onTitle={ctx.setTitle} />;
}

registerPanelKind({
  kind: PAGE_KIND,
  title: (tab) => basename((tab.data as PageTabData).path).replace(/\.html?$/i, ''),
  icon: () => <IconBrowser size={14} />,
  scope: 'app',
  maxTabs: 6,
  keepMounted: true,
  render: (tab, ctx) => <PageTab tab={tab as PanelTab} ctx={ctx} />,
  persist: {
    version: 1,
    save: (tab) => ({ path: (tab.data as PageTabData).path }),
    restore: (data) => {
      const d = data as { path?: unknown } | null;
      return d && validPagePath(d.path) ? { path: d.path } : null;
    },
  },
});
