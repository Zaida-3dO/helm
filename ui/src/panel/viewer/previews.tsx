// The viewer's richer previews: a collapsible JSON tree with search, a CSV/TSV table with row
// search, an image view with zoom, a PDF, audio and video player, a font specimen and a ZIP
// contents listing. Each shows only what the file holds; nothing is executed. Media, PDFs and fonts
// load from the host's ranged, short-lived address (never the whole file in memory, never the
// app's own origin); a ZIP is listed from its directory and never extracted.

import { useEffect, useMemo, useState } from 'react';
import type { ArchiveView, PanelOps } from '../../api/panel.ts';
import { archiveRows, delimiterFor, entriesOf, filterRows, jsonMatches, parseDelimited, parseJson, sizeLabel, stepZoom } from './preview.ts';

const MAX_CHILDREN = 200;
const MAX_ROWS = 2000;
const MAX_COLS = 200;
const MAX_ARCHIVE_ROWS = 3000;

export function canPreview(path: string, kind: string, language?: string): 'json' | 'table' | null {
  if (kind !== 'text') return null;
  if (language === 'json' || /\.json$/i.test(path)) return 'json';
  return delimiterFor(path) ? 'table' : null;
}

function Search({ value, onChange, label }: { value: string; onChange: (v: string) => void; label: string }) {
  return (
    <input
      type="search"
      className="pv-search"
      placeholder={label}
      aria-label={label}
      value={value}
      onChange={(e) => onChange(e.target.value)}
    />
  );
}

// A branch mounts its children only while it is open, so a large collapsed tree stays cheap. A
// search forces every branch open so the matches are visible.
function JsonBranch({ name, value, depth, q }: { name: string; value: object; depth: number; q: string }) {
  const [open, setOpen] = useState(depth < 1);
  const isOpen = q ? true : open;
  const all = entriesOf(value);
  const shown = isOpen ? all.filter(([k, v]) => jsonMatches(k, v, q)) : [];
  const list = shown.slice(0, MAX_CHILDREN);
  return (
    <details className="jt-node" open={isOpen} onToggle={(e) => !q && setOpen(e.currentTarget.open)}>
      <summary>
        <span className="jt-key">{name}</span>
        <span className="jt-meta">{Array.isArray(value) ? `[${all.length}]` : `{${all.length}}`}</span>
      </summary>
      {list.map(([k, v]) => (
        <JsonNode key={k} name={k} value={v} depth={depth + 1} q={q} />
      ))}
      {shown.length > list.length && <div className="jt-more">… {shown.length - list.length} more</div>}
    </details>
  );
}

function JsonNode({ name, value, depth, q }: { name: string; value: unknown; depth: number; q: string }) {
  if (value !== null && typeof value === 'object') return <JsonBranch name={name} value={value} depth={depth} q={q} />;
  const kind = value === null ? 'null' : typeof value;
  return (
    <div className="jt-leaf">
      <span className="jt-key">{name}</span>
      <span className={`jt-value jt-${kind}`}>{JSON.stringify(value)}</span>
    </div>
  );
}

export function JsonPreview({ text }: { text: string }) {
  const [q, setQ] = useState('');
  const parsed = useMemo(() => parseJson(text), [text]);
  if (!parsed.ok) return <div className="fv-message">This file is not valid JSON, so it is shown as source only.</div>;
  const value = parsed.value;
  const needle = q.trim();
  return (
    <div className="pv-root" data-testid="json-preview">
      <div className="pv-bar">
        <Search value={q} onChange={setQ} label="Search keys and values" />
      </div>
      <div className="jt-tree">
        <JsonNode name="root" value={value} depth={0} q={needle} />
      </div>
    </div>
  );
}

export function TablePreview({ text, path }: { text: string; path: string }) {
  const [q, setQ] = useState('');
  const rows = useMemo(() => parseDelimited(text, delimiterFor(path) ?? ','), [text, path]);
  const header = rows[0] ?? [];
  const body = useMemo(() => filterRows(rows.slice(1), q), [rows, q]);
  const shown = body.slice(0, MAX_ROWS);
  let cols = header.length;
  for (const r of rows) if (r.length > cols) cols = r.length;
  cols = Math.min(cols, MAX_COLS);
  return (
    <div className="pv-root" data-testid="table-preview">
      <div className="pv-bar">
        <Search value={q} onChange={setQ} label="Search rows" />
        <span className="pv-count">
          {body.length} of {Math.max(0, rows.length - 1)} rows
        </span>
      </div>
      <div className="pv-table-wrap">
        <table className="pv-table">
          <thead>
            <tr>
              {Array.from({ length: cols }, (_, c) => (
                <th key={c}>{header[c] ?? ''}</th>
              ))}
            </tr>
          </thead>
          <tbody>
            {shown.map((r) => (
              <tr key={r.index}>
                {Array.from({ length: cols }, (_, c) => (
                  <td key={c}>{r.cells[c] ?? ''}</td>
                ))}
              </tr>
            ))}
          </tbody>
        </table>
        {body.length > shown.length && <div className="jt-more">Showing the first {shown.length} rows.</div>}
      </div>
    </div>
  );
}

export function ImagePreview({ url, name, size }: { url: string | null; name: string; size: number }) {
  const [natural, setNatural] = useState<{ w: number; h: number } | null>(null);
  const [zoom, setZoom] = useState<'fit' | number>('fit');
  const pct = zoom === 'fit' ? null : Math.round(zoom * 100);
  return (
    <div className="pv-root" data-testid="image-preview">
      <div className="pv-bar">
        <span className="pv-count">
          {natural ? `${natural.w} × ${natural.h}` : ''}
          {natural ? ' · ' : ''}
          {size < 1024 ? `${size} B` : `${(size / 1024).toFixed(1)} KiB`}
        </span>
        <span className="pv-zoom">
          <button type="button" className="fv-icon-btn" aria-label="Zoom out" title="Zoom out" onClick={() => setZoom((z) => stepZoom(z === 'fit' ? 1 : z, -1))}>
            −
          </button>
          <span className="pv-pct" aria-live="polite">
            {pct === null ? 'Fit' : `${pct}%`}
          </span>
          <button type="button" className="fv-icon-btn" aria-label="Zoom in" title="Zoom in" onClick={() => setZoom((z) => stepZoom(z === 'fit' ? 1 : z, 1))}>
            +
          </button>
          <button type="button" className="fv-button bordered" aria-pressed={zoom === 'fit'} onClick={() => setZoom('fit')}>
            Fit
          </button>
          <button type="button" className="fv-button bordered" aria-pressed={zoom === 1} onClick={() => setZoom(1)}>
            100%
          </button>
        </span>
      </div>
      <div className="pv-canvas">
        {url ? (
          <img
            src={url}
            alt={name}
            className={zoom === 'fit' ? 'is-fit' : undefined}
            style={zoom !== 'fit' && natural ? { width: natural.w * zoom } : undefined}
            onLoad={(e) => setNatural({ w: e.currentTarget.naturalWidth, h: e.currentTarget.naturalHeight })}
          />
        ) : null}
      </div>
    </div>
  );
}

function NoAddress() {
  return <div className="fv-message">A preview is not available in this build.</div>;
}

/**
 * A PDF in the webview's own viewer. The address belongs to a separate origin the host serves, so
 * the file can reach nothing of the app; the viewer itself refuses to open inside a sandboxed frame,
 * so the frame carries no `sandbox` attribute, no permissions and no referrer.
 */
export function PdfPreview({ url, name }: { url?: string; name: string }) {
  if (!url) return <NoAddress />;
  return (
    <div className="pv-root" data-testid="pdf-preview">
      <iframe className="fv-pdf" title={`PDF preview of ${name}`} src={url} allow="" referrerPolicy="no-referrer" />
    </div>
  );
}

/** Audio or video with the browser's own controls; it loads only what plays, in byte ranges. */
export function MediaPreview({ url, name, kind }: { url?: string; name: string; kind: 'audio' | 'video' }) {
  const [failed, setFailed] = useState(false);
  useEffect(() => setFailed(false), [url]);
  if (!url) return <NoAddress />;
  return (
    <div className="pv-root" data-testid={`${kind}-preview`}>
      <div className="pv-media">
        {kind === 'video' ? (
          <video className="pv-video" src={url} controls preload="metadata" aria-label={name} onError={() => setFailed(true)} />
        ) : (
          <audio className="pv-audio" src={url} controls preload="metadata" aria-label={name} onError={() => setFailed(true)} />
        )}
        {failed && (
          <div className="fv-notice" role="alert">
            This {kind} could not be played here. Its format may not be supported; open it in the default app instead.
          </div>
        )}
      </div>
    </div>
  );
}

const SPECIMEN_LINES: Array<[number, string]> = [
  [36, 'Aa Bb Cc 0123456789'],
  [24, 'ABCDEFGHIJKLMNOPQRSTUVWXYZ'],
  [24, 'abcdefghijklmnopqrstuvwxyz'],
  [18, 'The quick brown fox jumps over the lazy dog.'],
  [14, 'Sphinx of black quartz, judge my vow. 0123456789 !?&@#%()[]{}'],
];

let specimenCount = 0;

/**
 * A font shown only as sample text in that face. The font loads through the FontFace API under a
 * private family name and is removed again when the view goes away; it is never installed and
 * nothing in the file runs.
 */
export function FontPreview({ url, name }: { url?: string; name: string }) {
  const [family, setFamily] = useState<string | null>(null);
  const [failed, setFailed] = useState(false);
  useEffect(() => {
    setFamily(null);
    setFailed(false);
    if (!url || typeof FontFace === 'undefined') {
      setFailed(true);
      return;
    }
    const id = `helm-specimen-${++specimenCount}`;
    const face = new FontFace(id, `url("${url}")`);
    let live = true;
    face
      .load()
      .then(() => {
        if (!live) return;
        document.fonts.add(face);
        setFamily(id);
      })
      .catch(() => {
        if (live) setFailed(true);
      });
    return () => {
      live = false;
      document.fonts.delete(face);
    };
  }, [url]);
  if (!url) return <NoAddress />;
  return (
    <div className="pv-root" data-testid="font-preview">
      <div className="pv-font" aria-label={`Specimen of ${name}`}>
        {failed && (
          <div className="fv-notice" role="alert">
            This font could not be loaded.
          </div>
        )}
        {family &&
          SPECIMEN_LINES.map(([px, text]) => (
            <div key={px} className="pv-font-line" style={{ fontFamily: `"${family}", sans-serif`, fontSize: px }}>
              {text}
            </div>
          ))}
      </div>
    </div>
  );
}

/** A ZIP's contents: folders and files with their sizes, read from its directory only. */
export function ArchivePreview({ ops, path, chatId }: { ops: PanelOps; path: string; chatId: string | null }) {
  const [view, setView] = useState<ArchiveView | null>(null);
  const [error, setError] = useState<string | null>(null);
  useEffect(() => {
    let cancelled = false;
    setView(null);
    setError(null);
    ops
      .archiveList(path, chatId)
      .then((v) => {
        if (!cancelled) setView(v);
      })
      .catch((e: unknown) => {
        if (!cancelled) setError(e instanceof Error ? e.message : String(e));
      });
    return () => {
      cancelled = true;
    };
  }, [ops, path, chatId]);
  const rows = useMemo(() => archiveRows(view?.entries ?? []), [view]);
  if (error) {
    return (
      <div className="fv-message" role="alert" data-testid="archive-error">
        {error}
      </div>
    );
  }
  if (!view) return <div className="fv-message">Loading…</div>;
  const shown = rows.slice(0, MAX_ARCHIVE_ROWS);
  const files = view.entries.filter((e) => !e.dir).length;
  return (
    <div className="pv-root" data-testid="archive-preview">
      <div className="pv-bar">
        <span className="pv-count">
          {files} {files === 1 ? 'file' : 'files'} · {sizeLabel(view.size)} · contents are listed, not extracted
        </span>
      </div>
      <div className="pv-archive" role="tree" aria-label="Archive contents">
        {shown.map((r) => (
          <div
            key={r.key}
            className="pv-arc-row"
            role="treeitem"
            aria-level={r.depth + 1}
            data-testid="archive-row"
            data-kind={r.dir ? 'dir' : 'file'}
            style={{ paddingLeft: 12 + r.depth * 16 }}
          >
            <span className="pv-arc-name">
              {r.name}
              {r.dir ? '/' : ''}
            </span>
            {r.encrypted && <span className="pv-arc-tag">Encrypted</span>}
            <span className="pv-arc-size">{r.dir ? '' : r.size !== undefined ? sizeLabel(r.size) : ''}</span>
          </div>
        ))}
        {rows.length > shown.length && <div className="jt-more">Showing the first {shown.length} rows.</div>}
        {view.truncated && (
          <div className="jt-more">
            The archive lists {view.total.toLocaleString()} entries; only the first {view.entries.length.toLocaleString()} are shown.
          </div>
        )}
      </div>
    </div>
  );
}
