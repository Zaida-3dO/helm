// The viewer's richer previews: a collapsible JSON tree with search, a CSV/TSV table with row
// search, and an image view with zoom. Each shows only what the file holds; nothing is executed.

import { useMemo, useState } from 'react';
import { delimiterFor, entriesOf, filterRows, jsonMatches, parseDelimited, parseJson, stepZoom } from './preview.ts';

const MAX_CHILDREN = 200;
const MAX_ROWS = 2000;
const MAX_COLS = 200;

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
