// The sidebar footer's Pages entry: the pinned pages for the active profile. Clicking one opens
// it as a tab in the side panel; Unpin removes it from the list (its saved state stays, so
// pinning it again finds it as it was). The mounting sidebar passes the active profile's id, or
// nothing for every page.

import { useCallback, useEffect, useMemo, useState } from 'react';
import type { Api } from '../api/client.ts';
import type { EventBus } from '../api/events.ts';
import { Button, Dialog, toast } from '../ui/index.ts';
import { IconGrid, IconUnpin } from '../ui/icons.tsx';
import { pageOps } from './api.ts';
import type { PinnedPage } from './api.ts';
import { openPageTab } from './open.ts';

export function PagesEntry({ api, bus, profileId }: { api: Api; bus: EventBus; profileId?: string | null }) {
  const ops = useMemo(() => pageOps(api), [api]);
  const [open, setOpen] = useState(false);
  const [pages, setPages] = useState<PinnedPage[] | null>(null);
  const [error, setError] = useState<string | null>(null);

  const refresh = useCallback(() => {
    ops
      .list(profileId)
      .then((r) => {
        setPages(r.pages);
        setError(null);
      })
      .catch((e: unknown) => setError(e instanceof Error ? e.message : String(e)));
  }, [ops, profileId]);

  useEffect(() => {
    refresh();
    return bus.on('pages_changed', refresh);
  }, [bus, refresh]);

  const show = (p: PinnedPage) => {
    if (!openPageTab(p.path)) {
      toast.show({ kind: 'error', text: 'The side panel has no room for another tab.' });
      return;
    }
    setOpen(false);
  };

  return (
    <>
      <button type="button" className="sb-foot" onClick={() => setOpen(true)} data-testid="nav-pages">
        <IconGrid />
        <span>Pages</span>
        {pages && pages.length > 0 && (
          <span className="sb-nav-count" aria-label={`${pages.length} pinned pages`} data-testid="nav-pages-count">
            {pages.length}
          </span>
        )}
      </button>
      {open && (
        <Dialog title="Pages" onClose={() => setOpen(false)} padded testId="pages-list">
          {error && <p className="pg-handoff-error">{error}</p>}
          {pages && pages.length === 0 && (
            <p className="pg-handoff-note" data-testid="pages-empty">
              No pages are pinned yet. Ask an agent to build you a page, or open an .html file and pin it.
            </p>
          )}
          <ul className="pg-list">
            {(pages ?? []).map((p) => (
              <li key={p.id} className="pg-list-row">
                <button type="button" className="pg-list-open" onClick={() => show(p)} data-testid="pages-row">
                  <span className="pg-list-title">{p.title}</span>
                  <span className="pg-list-path">{p.path}</span>
                </button>
                <Button
                  variant="icon"
                  label={`Unpin ${p.title}`}
                  onClick={() =>
                    void ops
                      .unpin(p.id)
                      .then(refresh)
                      .catch((e: unknown) => toast.show({ kind: 'error', text: e instanceof Error ? e.message : String(e) }))
                  }
                >
                  <IconUnpin size={14} />
                </Button>
              </li>
            ))}
          </ul>
        </Dialog>
      )}
    </>
  );
}
