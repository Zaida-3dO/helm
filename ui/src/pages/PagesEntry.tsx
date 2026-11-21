// The sidebar's Pages view, as the tile row and its More menu share it (the shape of
// `useTasksEntry`): the count of pages pinned by the active profile for the tile's badge, and the
// dialog that lists them. Clicking one opens it as a tab in the side panel; Unpin removes it from
// the list (its saved state stays, so pinning it again finds it as it was). The mounting sidebar
// passes the active profile's id, or nothing for every page.

import { useCallback, useEffect, useMemo, useState, type ReactNode } from 'react';
import type { Api } from '../api/client.ts';
import type { EventBus } from '../api/events.ts';
import { Button, Dialog, toast } from '../ui/index.ts';
import { IconUnpin } from '../ui/icons.tsx';
import { pageOps } from './api.ts';
import type { PinnedPage, RemotePages } from './api.ts';
import { openPageTab, openRemotePageTab } from './open.ts';
import { displayPath, shortPath } from '../panel/viewer/paths.ts';

export interface PagesEntry {
  /** Pages pinned by the active profile, for the tile's badge. */
  count: number;
  open: boolean;
  /** Open or close the dialog (the tile's and the More menu's action). */
  toggle: () => void;
  /** The dialog, mounted by whoever holds the entry. */
  dialog: ReactNode;
}

export function usePagesEntry(api: Api, bus: EventBus, profileId?: string | null): PagesEntry {
  const ops = useMemo(() => pageOps(api), [api]);
  const [open, setOpen] = useState(false);
  const [pages, setPages] = useState<PinnedPage[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [remote, setRemote] = useState<RemotePages[] | null>(null);

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

  // The household's other PCs are asked when the list is opened, not on every change.
  useEffect(() => {
    if (!open) return;
    let cancelled = false;
    setRemote(null);
    ops
      .devicePages()
      .then((r) => !cancelled && setRemote(r.devices))
      .catch(() => !cancelled && setRemote([]));
    return () => {
      cancelled = true;
    };
  }, [ops, open]);

  const showRemote = (device: RemotePages, p: PinnedPage) => {
    if (!openRemotePageTab(device.deviceUid, p)) {
      toast.show({ kind: 'error', text: 'The side panel has no room for another tab.' });
      return;
    }
    setOpen(false);
  };

  const show = (p: PinnedPage) => {
    if (!openPageTab(p.path)) {
      toast.show({ kind: 'error', text: 'The side panel has no room for another tab.' });
      return;
    }
    setOpen(false);
  };

  const dialog = open ? (
    <Dialog title="Pages" onClose={() => setOpen(false)} padded testId="pages-list">
      {error && <p className="pg-handoff-error">{error}</p>}
      {pages && pages.length === 0 && remote && remote.every((d) => d.pages.length === 0) && (
        <p className="pg-handoff-note" data-testid="pages-empty">
          No pages are pinned yet. Ask an agent to build you a page, or open an .html file and pin it.
        </p>
      )}
      <ul className="pg-list">
        {(pages ?? []).map((p) => (
          <li key={p.id} className="pg-list-row">
            <button type="button" className="pg-list-open" onClick={() => show(p)} title={`${p.title}\n${displayPath(p.path)}`} data-testid="pages-row">
              <span className="pg-list-title">{p.title}</span>
              <span className="pg-list-path">{shortPath(p.path)}</span>
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
      {(remote ?? [])
        .filter((d) => d.pages.length > 0 || d.problem)
        .map((d) => (
          <section key={d.deviceUid} data-testid="pages-device">
            <h3 className="pg-list-device">On {d.name}</h3>
            {d.problem && <p className="pg-handoff-note">{d.problem.message}</p>}
            <ul className="pg-list">
              {d.pages.map((p) => (
                <li key={p.id} className="pg-list-row">
                  <button type="button" className="pg-list-open" onClick={() => showRemote(d, p)} title={`${p.title}\n${displayPath(p.path)}`} data-testid="pages-remote-row">
                    <span className="pg-list-title">{p.title}</span>
                    <span className="pg-list-path">{shortPath(p.path)}</span>
                  </button>
                </li>
              ))}
            </ul>
          </section>
        ))}
    </Dialog>
  ) : null;

  return { count: pages?.length ?? 0, open, toggle: () => setOpen((v) => !v), dialog };
}
