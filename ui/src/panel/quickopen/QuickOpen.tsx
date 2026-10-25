// Quick-open, "Go to file". Ctrl+P. A query that
// looks like a path completes folders (with `~`); any other query ranks the chat's project files.
// Open tabs and recently opened paths come first and need no disk access. Enter opens (a folder
// opens as a folder view), Tab completes the path, Escape clears the input first, then closes.
// Double-Shift is not bound (that setting is off by default).

import { useEffect, useMemo, useRef, useState } from 'react';
import type { QuickItem } from '../../api/panel.ts';
import { panelOps } from '../../api/panel.ts';
import { Dialog, useBinding, useLayer } from '../../ui/index.ts';
import { FileTextIcon, FolderOpenIcon, ReloadIcon } from '../icons.tsx';
import { openInPanel, panelStore } from '../registry.ts';
import { basename, displayPath, matchSegments, pathKey } from '../viewer/paths.ts';
import { addRecent, recentPaths } from './recent.ts';

const MAX_ROWS = 50;
const DEBOUNCE_MS = 60;

interface Row {
  group: 'Open tabs' | 'Recent files' | 'In folder' | 'Project files';
  path: string;
  label: string;
  detail: string;
  kind: 'dir' | 'file' | 'link';
}

function looksLikePath(q: string): boolean {
  return /^(~|\.|[a-z]:)/i.test(q) || /[\\/]/.test(q);
}

/** A name or path with the characters the query matched drawn as marks. */
function Marked({ text, query, as }: { text: string; query: string; as: 'letters' | 'run' }) {
  return (
    <>
      {matchSegments(text, query).map((seg, i) =>
        seg.hit ? (
          <mark key={i} className={as === 'letters' ? 'qo-hit' : 'qo-hit run'}>
            {seg.text}
          </mark>
        ) : (
          <span key={i}>{seg.text}</span>
        ),
      )}
    </>
  );
}

function openPath(path: string, chatId: string | null) {
  const p = displayPath(path);
  addRecent(p);
  if (!openInPanel('file', pathKey(p), { path: p }, { intent: 'user', chatId })) {
    panelStore.openModal(p, { chatId });
  }
}

export function QuickOpen({ chatId }: { chatId: string | null }) {
  const [open, setOpen] = useState(false);
  useBinding({ id: 'panel.quickOpen', title: 'Go to file', defaultKeys: 'Ctrl+P', run: () => setOpen((o) => !o) });
  if (!open || !panelStore.api) return null;
  return <QuickOpenDialog chatId={chatId} onClose={() => setOpen(false)} />;
}

function QuickOpenDialog({ chatId, onClose }: { chatId: string | null; onClose: () => void }) {
  const ops = useMemo(() => panelOps(panelStore.api!), []);
  const [query, setQuery] = useState('');
  const [remote, setRemote] = useState<{ q: string; items: QuickItem[]; mode: string; partial: boolean } | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [selected, setSelected] = useState(0);
  const [reload, setReload] = useState(0);
  const input = useRef<HTMLInputElement>(null);
  const seq = useRef(0);

  // Escape clears a non-empty input before it closes the dialog.
  useLayer('search', query !== '', () => setQuery(''));

  useEffect(() => {
    const q = query.trim();
    if (!q) {
      setRemote(null);
      setError(null);
      return;
    }
    const my = ++seq.current;
    const timer = setTimeout(() => {
      ops
        .quickOpen(q, chatId, reload > 0)
        .then((r) => {
          if (my !== seq.current) return; // a newer query is in flight: drop this answer
          setRemote({ q, ...r });
          setError(null);
        })
        .catch((e: unknown) => {
          if (my !== seq.current) return;
          setRemote(null);
          setError(e instanceof Error ? e.message : String(e));
        });
    }, DEBOUNCE_MS);
    return () => clearTimeout(timer);
  }, [query, chatId, ops, reload]);

  const rows = useMemo<Row[]>(() => {
    const q = query.trim().toLowerCase();
    const seen = new Set<string>();
    const out: Row[] = [];
    const push = (r: Row) => {
      const k = pathKey(r.path);
      if (seen.has(k) || out.length >= MAX_ROWS) return;
      seen.add(k);
      out.push(r);
    };
    const tabs = panelStore.model.displayed(chatId).filter((t) => t.kind === 'file');
    for (const t of tabs) {
      const p = (t.data as { current?: string; path: string }).current ?? (t.data as { path: string }).path;
      if (!q || basename(p).toLowerCase().includes(q) || p.toLowerCase().includes(q)) {
        push({ group: 'Open tabs', path: p, label: basename(p), detail: displayPath(p), kind: 'file' });
      }
    }
    for (const p of recentPaths()) {
      if (!q || p.toLowerCase().includes(q)) push({ group: 'Recent files', path: p, label: basename(p), detail: p, kind: 'file' });
    }
    if (remote && remote.q === query.trim()) {
      for (const it of remote.items) {
        push({
          group: remote.mode === 'path' ? 'In folder' : 'Project files',
          path: it.path,
          label: remote.mode === 'path' ? it.label : basename(it.label),
          detail: remote.mode === 'path' ? '' : it.label,
          kind: it.kind,
        });
      }
    }
    return out;
  }, [query, remote, chatId]);

  useEffect(() => setSelected(0), [query]);
  const current = rows[Math.min(selected, rows.length - 1)];

  const choose = (row: Row | undefined) => {
    if (!row) return;
    if (row.kind === 'dir' && looksLikePath(query)) {
      // Entering a folder keeps the dialog open on its contents.
      setQuery(`${displayPath(row.path)}\\`);
      return;
    }
    onClose();
    openPath(row.path, chatId);
  };

  const pathMode = looksLikePath(query);
  return (
    <Dialog
      title="Go to file"
      wide
      onClose={onClose}
      testId="quick-open"
      intro={
        <>
          <div className="qo-field">
            <FolderOpenIcon size={18} />
            <input
              ref={input}
              autoFocus
              data-autofocus
              className="qo-input"
              placeholder="Go to file… (~/, / or a folder path)"
              aria-label="Go to file"
              role="combobox"
              aria-expanded
              aria-controls="qo-list"
              aria-activedescendant={current ? `qo-${rows.indexOf(current)}` : undefined}
              value={query}
              onChange={(e) => setQuery(e.target.value)}
              onKeyDown={(e) => {
                if (e.key === 'ArrowDown' || e.key === 'ArrowUp') {
                  e.preventDefault();
                  const n = rows.length;
                  if (n) setSelected((i) => (e.key === 'ArrowDown' ? (i + 1) % n : (i - 1 + n) % n));
                } else if (e.key === 'PageDown' || e.key === 'PageUp') {
                  e.preventDefault();
                  setSelected((i) => Math.max(0, Math.min(rows.length - 1, i + (e.key === 'PageDown' ? 10 : -10))));
                } else if (e.key === 'Enter') {
                  e.preventDefault();
                  choose(current);
                } else if (e.key === 'Tab' && current) {
                  // Tab completes the path.
                  e.preventDefault();
                  setQuery(displayPath(current.path) + (current.kind === 'dir' ? '\\' : ''));
                }
              }}
            />
            <kbd className="qo-esc">esc</kbd>
          </div>
          <div className="qo-status">
            {error ? (
              <span className="qo-error">{error}</span>
            ) : (
              <span>
                {rows.length} results{remote?.partial ? ' · Showing part of this project' : ''}
              </span>
            )}
            {remote?.mode === 'files' && (
              <button type="button" className="fv-button bordered" onClick={() => setReload((n) => n + 1)}>
                <ReloadIcon />
                <span>Reload</span>
              </button>
            )}
          </div>
        </>
      }
    >
      <div className="qo-list" id="qo-list" role="listbox" aria-label="Files">
        {rows.length === 0 ? (
          <div className="qo-empty">
            <FolderOpenIcon size={24} />
            <div>{query ? 'Nothing here matches' : 'Go to a file or folder'}</div>
            <div className="qo-hint">Start with ~/ or C:\ to browse folders; Tab completes.</div>
          </div>
        ) : (
          rows.map((row, i) => (
            <div key={`${row.group}:${row.path}`}>
              {(i === 0 || rows[i - 1]?.group !== row.group) && <div className="qo-group">{row.group}</div>}
              <div
                id={`qo-${i}`}
                role="option"
                aria-selected={row === current}
                className="qo-row"
                data-testid="quick-open-row"
                data-kind={row.kind}
                onMouseEnter={() => setSelected(i)}
                onMouseDown={(e) => e.preventDefault()}
                onClick={() => choose(row)}
              >
                {row.kind === 'dir' ? <FolderOpenIcon size={14} /> : <FileTextIcon size={14} />}
                <span className="qo-name">{pathMode ? row.label : <Marked text={row.label} query={query} as="letters" />}</span>
                {row.detail && <span className="qo-detail">{pathMode ? row.detail : <Marked text={row.detail} query={query} as="run" />}</span>}
              </div>
            </div>
          ))
        )}
      </div>
    </Dialog>
  );
}
