// The sidebar's Pages view, as the tile row and its More menu share it (the shape of
// `useTasksEntry`): the count of pages pinned by the active profile for the tile's badge, and the
// dialog that lists them. The list is one: this PC's pins, then the paired PCs', de-duplicated,
// with the PC as a small detail on the row and never a group of its own, and a PC that could not
// be read left quiet. Each row leads with the bundle's own favicon (asked once per path) or a
// letter tile. Clicking one opens it as
// a tab in the side panel; Unpin removes it from the list (its saved state stays, so pinning it
// again finds it as it was). A row for this PC can be dragged to reorder the pins; once
// reordered the list keeps that order. The mounting sidebar passes the active profile's id, or
// nothing for every page.

import { useCallback, useEffect, useMemo, useState, type ReactNode } from 'react';
import type { Api } from '../api/client.ts';
import type { EventBus } from '../api/events.ts';
import { Button, Dialog, moveItem, toast, useDragReorder } from '../ui/index.ts';
import { IconUnpin } from '../ui/icons.tsx';
import { pageOps } from './api.ts';
import type { PinnedPage, RemotePages } from './api.ts';
import { mergePages, tileLetter } from './list.ts';
import { openPageTab, openRemotePageTab } from './open.ts';
import { displayPath, shortPath } from '../panel/viewer/paths.ts';
import { devicesStoreFor } from '../devices/store.ts';
import { profilesStoreFor } from '../profiles/store.ts';

export interface PagesEntry {
  /** Pages pinned by the active profile, for the tile's badge. */
  count: number;
  open: boolean;
  /** Open or close the list (the tile's and the More menu's action). */
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
  // Each page's bundle favicon, asked once per path while the list is open. Remote pages keep
  // their icon on their own PC, so they show the letter tile.
  const [icons, setIcons] = useState<Record<string, string | null>>({});

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

  // One list: this PC's pins, then the other PCs', deduped and filtered to the acting person.
  const merged = useMemo(() => {
    const personId = profilesStoreFor(api, bus).snapshot.view?.profiles.find((p) => p.id === profileId)?.hubPersonId ?? null;
    return mergePages(pages ?? [], remote ?? [], devicesStoreFor(api, bus).snapshot.list, personId);
  }, [api, bus, pages, remote, profileId]);

  // Drag to reorder this PC's pins. Only the local rows are given to the hook, so a drag
  // can only land between local rows: the order of another PC's pins lives on that PC.
  const localIndexOf = useMemo(() => {
    const m = new Map<string, number>();
    for (const r of merged.rows) if (!r.device) m.set(r.page.id, m.size);
    return m;
  }, [merged]);
  const localIds = useMemo(() => [...localIndexOf.keys()], [localIndexOf]);
  const reorder = useDragReorder((from, to) => {
    const moved = localIds[from];
    if (moved === undefined || to === from || to === from + 1) return;
    const next = moveItem(localIds, from, to);
    void ops
      .pinMove(moved, next[to + 1] ?? null)
      .catch((e: unknown) => toast.show({ kind: 'error', text: e instanceof Error ? e.message : String(e) }));
  });

  // Fetch a favicon for each local page not asked for yet; a page without one gets null and the
  // letter tile, and a failed ask is remembered so it is not retried on every render.
  useEffect(() => {
    if (!open) return;
    const missing = [...new Set(merged.rows.filter((r) => !r.device).map((r) => r.page.path))].filter((p) => !(p in icons));
    if (missing.length === 0) return;
    let cancelled = false;
    void Promise.all(
      missing.map((p) => ops.icon(p).then((r) => [p, r.icon] as const).catch(() => [p, null] as const)),
    ).then((got) => {
      if (cancelled) return;
      setIcons((cur) => {
        const next = { ...cur };
        for (const [p, icon] of got) next[p] = icon;
        return next;
      });
    });
    return () => {
      cancelled = true;
    };
  }, [ops, open, merged, icons]);

  const show = (p: PinnedPage) => {
    if (!openPageTab(p.path)) {
      toast.show({ kind: 'error', text: 'The side panel has no room for another tab.' });
      return;
    }
    setOpen(false);
  };

  const showRemote = (deviceUid: string, p: PinnedPage) => {
    if (!openRemotePageTab(deviceUid, p)) {
      toast.show({ kind: 'error', text: 'The side panel has no room for another tab.' });
      return;
    }
    setOpen(false);
  };

  const dialog = open ? (
    <Dialog title="Pages" onClose={() => setOpen(false)} padded testId="pages-list">
      {error && <p className="pg-handoff-error">{error}</p>}
      {pages !== null && remote !== null && merged.rows.length === 0 && merged.problems.length === 0 && (
        <p className="pg-handoff-note" data-testid="pages-empty">
          No pages are pinned yet. Ask an agent to build you a page, or open an .html file and pin it.
        </p>
      )}
      <ul className="pg-list">
        {merged.rows.map(({ page: p, device }) => {
          const icon = device ? null : icons[p.path];
          const localIndex = device ? -1 : localIndexOf.get(p.id) ?? -1;
          const live = localIndex >= 0 && reorder.drag?.from === localIndex;
          return (
          <li
            key={device ? `${device.uid}:${p.id}` : p.id}
            className={`pg-list-row${live ? ' drag-live' : ''}`}
            style={live ? { transform: `translateY(${reorder.drag!.offset}px)` } : undefined}
            data-dragging={live ? 'true' : undefined}
            {...(localIndex >= 0 ? reorder.rowProps(localIndex) : {})}
          >
            <button
              type="button"
              className="pg-list-open"
              onClick={() => (device ? showRemote(device.uid, p) : show(p))}
              title={device ? `${p.title}\nOn ${device.name}\n${displayPath(p.path)}` : `${p.title}\n${displayPath(p.path)}`}
              data-testid={device ? 'pages-remote-row' : 'pages-row'}
            >
              <span className="pg-list-icon" data-testid="pages-row-icon" aria-hidden="true">
                {icon ? <img src={icon} alt="" /> : <span className="pg-list-letter">{tileLetter(p.title)}</span>}
              </span>
              <span className="pg-list-text">
                <span className="pg-list-title">{p.title}</span>
                <span className="pg-list-path">
                  {shortPath(p.path)}
                  {device && <span className="pg-list-on" data-testid="pages-row-on"> · on {device.name}</span>}
                </span>
              </span>
            </button>
            {!device && (
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
            )}
          </li>
          );
        })}
      </ul>
    </Dialog>
  ) : null;

  return { count: pages?.length ?? 0, open, toggle: () => setOpen((v) => !v), dialog };
}
