// The `pr` tab kind and the link route that opens it. The body is loaded as its own chunk the first
// time a PR tab is shown; the cache entry goes away with the tab.

import { Suspense, lazy } from 'react';
import './pr.css';
import { registerWebRoute } from '../panel/links.ts';
import { registerPanelKind } from '../panel/registry.ts';
import { IconFork } from '../ui/icons.tsx';
import { parsePrUrl } from './links.ts';
import { PR_KIND, openPrTab, prViewEnabled } from './route.ts';
import { dropEntry } from './store.ts';
import type { PrTabData } from './types.ts';

const Body = lazy(() => import('./PrTab.tsx'));
const Actions = lazy(() => import('./PrTab.tsx').then((m) => ({ default: m.HeaderActions })));

registerPanelKind({
  kind: PR_KIND,
  scope: 'app',
  maxTabs: 8,
  preferredWidth: 720,
  title: (tab) => {
    const d = tab.data as PrTabData;
    return `${d.repo}#${d.number}`;
  },
  icon: () => <IconFork size={14} />,
  render: (tab, ctx) => (
    <Suspense fallback={<div className="pr-loading">Loading…</div>}>
      <Body tab={tab as never} ctx={ctx} />
    </Suspense>
  ),
  headerActions: (tab, ctx) => (
    <Suspense fallback={null}>
      <Actions tab={tab as never} ctx={ctx} />
    </Suspense>
  ),
  onClose: (tab) => dropEntry(tab.data as PrTabData),
});

/** A github.com pull request address opens in a tab while the PR view is on; anything else goes to the browser. */
registerWebRoute(parsePrUrl, async (data, ctx) => {
  if (!(await prViewEnabled())) return false;
  return openPrTab(data, ctx.chatId);
});
