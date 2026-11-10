// The Inspector's Files tab: the chat's project folder as a tree. One click selects a row (a folder
// also opens or closes), double-click or Enter opens a file in its own panel tab and leaves the tree
// as it was, and the arrow keys move, open and close. Which folders are open, the selected row and
// the listings are kept per chat, so coming back to the tab finds the tree where it was left.

import { useCallback, useEffect, useLayoutEffect, useReducer, useRef } from 'react';
import type { KeyboardEvent } from 'react';
import type { PanelOps } from '../../api/panel.ts';
import { report } from '../context/store.ts';
import { ChevronIcon, EyeIcon, EyeOffIcon, FileTextIcon, FolderOpenIcon, ReloadIcon } from '../icons.tsx';
import { openInPanel, panelStore } from '../registry.ts';
import { BACKGROUND_POLL_MS, changedSince, watchedSlice } from './live.ts';
import { ancestorsWithin, basename, displayPath, isInside, pathKey } from './paths.ts';
import { takeReveal } from './reveal.ts';
import { treeKey, treeRows } from './tree.ts';
import type { Listing, Row } from './tree.ts';

interface TreeStore {
  root: string;
  /** Open folders: lower-cased path → path as listed. */
  expanded: Map<string, string>;
  selected: string | null;
  listings: Map<string, Listing>;
  showHidden: boolean;
}

const stores = new Map<string, TreeStore>();

function storeFor(chatId: string, root: string): TreeStore {
  let st = stores.get(chatId);
  if (!st || pathKey(st.root) !== pathKey(root)) {
    st = { root, expanded: new Map(), selected: null, listings: new Map(), showHidden: false };
    stores.set(chatId, st);
  }
  return st;
}

export interface FilesTreeProps {
  ops: PanelOps;
  chatId: string;
  root: string;
  visible: boolean;
}

export function FilesTree({ ops, chatId, root, visible }: FilesTreeProps) {
  const st = storeFor(chatId, root);
  const [, redraw] = useReducer((n: number) => n + 1, 0);
  const alive = useRef(true);
  const rowRefs = useRef(new Map<string, HTMLElement>());
  // Set by a reveal or a key move; the next render scrolls to the row and focuses it.
  const toFocus = useRef<{ key: string; center: boolean } | null>(null);

  useEffect(() => {
    alive.current = true;
    return () => {
      alive.current = false;
    };
  }, []);

  /** Lists a folder. A quiet re-list keeps the old rows on screen until the new ones arrive. */
  const load = useCallback(
    (dir: string, quiet: boolean) => {
      const key = pathKey(dir);
      const prev = st.listings.get(key);
      if (!(quiet && prev?.status === 'ready')) st.listings.set(key, { status: 'loading' });
      return ops
        .fileRead(dir, chatId)
        .then((v) => {
          if (v.kind !== 'dir') throw new Error('This is not a folder.');
          st.listings.set(key, { status: 'ready', entries: v.entries ?? [], truncated: v.truncated, modified: v.modified });
        })
        .catch((e: unknown) => {
          if (quiet && prev?.status === 'ready') {
            st.listings.set(key, prev);
            return;
          }
          st.listings.set(key, { status: 'error', message: e instanceof Error ? e.message : String(e) });
        })
        .finally(() => alive.current && redraw());
    },
    [ops, chatId, st],
  );

  // On show: take a pending "Show in Files", then list the root and every open folder again.
  useEffect(() => {
    if (!visible) return;
    const r = takeReveal(chatId);
    if (r && isInside(root, r.path)) {
      for (const a of ancestorsWithin(root, r.path)) if (pathKey(a) !== pathKey(root)) st.expanded.set(pathKey(a), a);
      st.selected = displayPath(r.path);
      toFocus.current = { key: pathKey(r.path), center: true };
    }
    for (const dir of [root, ...st.expanded.values()]) void load(dir, true);
    redraw();
  }, [visible, chatId, root, st, load]);

  // While shown, a folder that changed on disk is listed again (the root and open folders, a few at a time).
  useEffect(() => {
    if (!visible) return;
    let busy = false;
    const timer = setInterval(() => {
      if (busy || (typeof document !== 'undefined' && !document.hasFocus())) return;
      const dirs = watchedSlice([root, ...st.expanded.values()]);
      busy = true;
      ops
        .fileStat(dirs)
        .then((stamps) => {
          stamps.forEach((stamp, i) => {
            const dir = dirs[i] as string;
            const listing = st.listings.get(pathKey(dir));
            if (!stamp || listing?.status !== 'ready') return;
            if (listing.modified === undefined) listing.modified = stamp.modified;
            else if (changedSince(listing.modified, stamp)) {
              listing.modified = stamp.modified;
              void load(dir, true);
            }
          });
        })
        .catch(() => undefined)
        .finally(() => {
          busy = false;
        });
    }, BACKGROUND_POLL_MS);
    return () => clearInterval(timer);
  }, [visible, ops, root, st, load]);

  const rows = treeRows(root, new Set(st.expanded.keys()), st.listings, st.showHidden);
  const selectedKey = st.selected ? pathKey(st.selected) : null;
  const entryRows = rows.filter((r): r is Extract<Row, { type: 'entry' }> => r.type === 'entry');
  // The one row in the tab order: the selected one, else the first.
  const tabbable = entryRows.find((r) => pathKey(r.path) === selectedKey) ?? entryRows[0];

  // What the composer's chip shows while this tab is up: the selected file or folder.
  const selectedRow = entryRows.find((r) => pathKey(r.path) === selectedKey);
  const selectedPath = selectedRow?.path;
  const selectedKind = selectedRow?.kind;
  useEffect(() => {
    report(`files:${chatId}`, selectedPath ? (selectedKind === 'dir' ? { kind: 'folder', path: selectedPath } : { kind: 'file', path: selectedPath }) : null);
  }, [chatId, selectedPath, selectedKind]);

  useLayoutEffect(() => {
    const want = toFocus.current;
    if (!want) return;
    const el = rowRefs.current.get(want.key);
    if (!el) return;
    toFocus.current = null;
    el.scrollIntoView?.({ block: want.center ? 'center' : 'nearest' });
    el.focus({ preventScroll: true });
  });

  const toggle = (path: string, open: boolean) => {
    const key = pathKey(path);
    if (open) {
      st.expanded.set(key, path);
      if (st.listings.get(key)?.status !== 'ready') void load(path, false);
    } else st.expanded.delete(key);
    redraw();
  };
  const openFile = (path: string) => {
    if (!openInPanel('file', pathKey(path), { path, fromFiles: true }, { intent: 'user', chatId })) panelStore.openModal(path, { chatId });
  };
  const select = (row: Extract<Row, { type: 'entry' }>) => {
    st.selected = row.path;
    toFocus.current = { key: pathKey(row.path), center: false };
    redraw();
  };

  const onKeyDown = (e: KeyboardEvent<HTMLDivElement>) => {
    const at = Number((e.target as HTMLElement).closest<HTMLElement>('[data-row]')?.dataset.row ?? NaN);
    if (Number.isNaN(at)) return;
    const action = treeKey(rows, at, e.key);
    if (!action) return;
    e.preventDefault();
    if (action.type === 'focus') select(rows[action.index] as Extract<Row, { type: 'entry' }>);
    else if (action.type === 'toggle') {
      toggle(action.path, action.open);
      st.selected = action.path;
    } else openFile(action.path);
  };

  const refreshAll = () => {
    for (const dir of [root, ...st.expanded.values()]) void load(dir, true);
  };

  return (
    <div className="ft-root" data-testid="files-tree">
      <div className="fv-toolbar compact">
        <span className="fv-label">
          <FolderOpenIcon size={14} />
          <span className="fv-label-text">{basename(root)}</span>
        </span>
        <span className="fv-tools">
          <button
            type="button"
            className="fv-icon-btn"
            aria-pressed={st.showHidden}
            aria-label="Show hidden files"
            title={st.showHidden ? 'Hide hidden files' : 'Show hidden files'}
            onClick={() => {
              st.showHidden = !st.showHidden;
              redraw();
            }}
          >
            {st.showHidden ? <EyeIcon /> : <EyeOffIcon />}
          </button>
          <button type="button" className="fv-icon-btn" aria-label="Refresh" title="List the folders again" onClick={refreshAll}>
            <ReloadIcon />
          </button>
        </span>
      </div>
      <div className="ft-tree" role="tree" aria-label="Project files" onKeyDown={onKeyDown}>
        {rows.map((row, i) => {
          if (row.type === 'note') {
            return (
              <div key={row.id} className="ft-note" style={{ paddingLeft: 12 + row.depth * 14 }} data-testid="files-note">
                {row.text}
                {row.retry && (
                  <button type="button" className="fv-button bordered" onClick={() => void load(row.retry as string, false)}>
                    Retry
                  </button>
                )}
              </div>
            );
          }
          const key = pathKey(row.path);
          const selected = key === selectedKey;
          const isDir = row.kind === 'dir';
          return (
            <div
              key={key}
              ref={(el) => {
                if (el) rowRefs.current.set(key, el);
                else rowRefs.current.delete(key);
              }}
              role="treeitem"
              className="ft-row"
              aria-level={row.depth + 1}
              aria-expanded={isDir ? row.open : undefined}
              aria-selected={selected}
              tabIndex={row === tabbable ? 0 : -1}
              style={{ paddingLeft: 8 + row.depth * 14 }}
              title={row.kind === 'link' ? 'Links are not opened' : displayPath(row.path)}
              data-row={i}
              data-testid="files-row"
              data-path={row.path}
              data-kind={row.kind}
              data-link={row.kind === 'link' ? '' : undefined}
              onClick={() => {
                select(row);
                if (isDir) toggle(row.path, !row.open);
              }}
              onDoubleClick={() => {
                if (row.kind === 'file') openFile(row.path);
              }}
            >
              {isDir ? <ChevronIcon className={row.open ? 'ft-chevron open' : 'ft-chevron'} /> : <span className="ft-chevron-gap" aria-hidden />}
              {isDir ? <FolderOpenIcon size={13} /> : <FileTextIcon size={13} />}
              <span className="ft-name">{row.name}</span>
            </div>
          );
        })}
      </div>
    </div>
  );
}
