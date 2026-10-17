// The file viewer body, shared by `file` tabs, the Inspector's Files
// slot and the modal viewer. It reads only while it is shown (a restored tab reads nothing until it
// is selected), keeps its own back/forward history, and never executes what it previews.

import { useEffect, useMemo, useState } from 'react';
import type { FileView, PanelOps } from '../../api/panel.ts';
import { Markdown } from '../../markdown/index.ts';
import type { LinkTarget } from '../../markdown/index.ts';
import { FileTextIcon, FolderOpenIcon } from '../icons.tsx';
import { gateTarget, handleLink } from '../links.ts';
import { CodeView } from './CodeView.tsx';
import { dirname, displayPath, joinPath, splitSegments } from './paths.ts';

export interface Location {
  path: string;
  line?: number;
  /** The last line of a linked range. */
  endLine?: number;
  column?: number;
}

const HISTORY_LIMIT = 100;

function formatSize(n: number): string {
  if (n < 1024) return `${n} B`;
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KiB`;
  return `${(n / (1024 * 1024)).toFixed(1)} MiB`;
}

const KIND_LABEL: Record<string, string> = {
  typescript: 'TypeScript', javascript: 'JavaScript', json: 'JSON', markdown: 'Markdown', rust: 'Rust',
  python: 'Python', css: 'CSS', scss: 'SCSS', xml: 'XML', yaml: 'YAML', ini: 'Config', bash: 'Shell',
  powershell: 'PowerShell', dos: 'Batch', go: 'Go', java: 'Java', csharp: 'C#', c: 'C', cpp: 'C++',
  sql: 'SQL', diff: 'Diff',
};

function kindLabel(v: FileView): string {
  if (v.kind === 'dir') return 'Folder';
  if (v.kind === 'image') return 'Image';
  if (v.kind === 'html') return 'HTML';
  if (v.kind === 'binary') return 'Binary';
  return (v.language && KIND_LABEL[v.language]) || 'Text';
}

/** Turns base64 image bytes into an object URL for as long as the view is shown. */
function useObjectUrl(view: FileView | null): string | null {
  const [url, setUrl] = useState<string | null>(null);
  useEffect(() => {
    if (!view || view.kind !== 'image' || !view.bytes) {
      setUrl(null);
      return;
    }
    const bin = atob(view.bytes);
    const bytes = new Uint8Array(bin.length);
    for (let i = 0; i < bin.length; i++) bytes[i] = bin.charCodeAt(i);
    const u = URL.createObjectURL(new Blob([bytes], { type: view.mime ?? 'application/octet-stream' }));
    setUrl(u);
    return () => URL.revokeObjectURL(u);
  }, [view]);
  return url;
}

export interface FileViewerProps {
  ops: PanelOps;
  start: Location;
  chatId: string | null;
  /** Read only while shown. */
  visible: boolean;
  /** The location the body shows now (the header's Open in <App> follows it). */
  onLocation?: (loc: Location, view: FileView | null) => void;
  /** Where the folder view's ".." stops (the Files slot's project folder). */
  root?: string;
  /** Hide the navigation row (the Files slot draws its own). */
  compact?: boolean;
  /** The source view's highlighted lines, for the side panel context. */
  onSelection?: (sel: { startLine: number; endLine: number; text: string } | null) => void;
}

export function FileViewer({ ops, start, chatId, visible, onLocation, root, compact, onSelection }: FileViewerProps) {
  const [history, setHistory] = useState<{ stack: Location[]; index: number }>({ stack: [start], index: 0 });
  const [view, setView] = useState<FileView | null>(null);
  const [loadedFor, setLoadedFor] = useState<Location | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(false);
  const [reload, setReload] = useState(0);
  const [wrap, setWrap] = useState(false);
  const [mode, setMode] = useState<'preview' | 'source' | null>(null);
  const [copied, setCopied] = useState(false);

  // A new start (the same file reopened at another line) resets the history.
  useEffect(() => {
    setHistory({ stack: [start], index: 0 });
    setMode(null);
  }, [start]);

  const loc = history.stack[history.index] as Location;

  useEffect(() => {
    if (!visible) return;
    if (loadedFor === loc && reload === 0) return;
    let cancelled = false;
    setLoading(true);
    ops
      .fileRead(loc.path, chatId)
      .then((v) => {
        if (cancelled) return;
        setView(v);
        setError(null);
        setLoadedFor(loc);
      })
      .catch((e: unknown) => {
        if (cancelled) return;
        setView(null);
        setError(e instanceof Error ? e.message : String(e));
        setLoadedFor(loc);
      })
      .finally(() => {
        if (!cancelled) {
          setLoading(false);
          setReload(0);
        }
      });
    return () => {
      cancelled = true;
    };
    // `loadedFor` is deliberately left out: it only records what the last read was for.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [visible, loc, reload, ops, chatId]);

  useEffect(() => {
    onLocation?.(loc, loadedFor === loc ? view : null);
  }, [loc, view, loadedFor, onLocation]);

  const imageUrl = useObjectUrl(view);

  const go = (next: Location) =>
    setHistory((h) => {
      const stack = [...h.stack.slice(0, h.index + 1), next].slice(-HISTORY_LIMIT);
      return { stack, index: stack.length - 1 };
    });

  const isMarkdown = view?.kind === 'text' && view.language === 'markdown';
  const renderable = isMarkdown || view?.kind === 'html';
  // Source is the default when the link carries a line number.
  const effectiveMode = mode ?? (loc.line ? 'source' : 'preview');
  const parent = dirname(loc.path);
  const atRoot = root ? displayPath(loc.path).toLowerCase() === displayPath(root).toLowerCase() : parent === null;

  const onDocLink = useMemo(
    () => (t: LinkTarget) => {
      // Relative links inside a rendered file resolve against its folder and navigate inside the
      // viewer, still through the gate. A relative `.md` link is the note beside this
      // file, not a vault lookup.
      const base = dirname(loc.path);
      const relative = (s: string) => !/^[a-z][a-z0-9+.-]*:/i.test(s) && !/^[\\/~]/.test(s);
      let rel: string | null = null;
      if (t.kind === 'url' && t.href && relative(t.href)) rel = t.href;
      else if (t.kind === 'vault' && !t.vault && t.note && relative(t.note)) rel = `${t.note}.md`;
      else if (t.kind === 'path' && t.href && relative(t.href)) rel = t.href;
      if (base && rel) {
        const gate = gateTarget({ kind: 'path', href: joinPath(base, rel) });
        if (gate.action === 'file') {
          go({ path: gate.path, line: gate.line, endLine: gate.endLine, column: gate.column });
          return;
        }
      }
      void handleLink(t, { chatId });
    },
    [loc.path, chatId],
  );

  const toolbar = (
    <div className="fv-toolbar">
      <span className="fv-label">
        {view?.kind === 'dir' ? <FolderOpenIcon size={14} /> : <FileTextIcon size={14} />}
        <span>{view ? (view.kind === 'dir' ? 'Folder' : `${formatSize(view.size)} · ${kindLabel(view)}`) : loading ? 'Loading…' : ''}</span>
      </span>
      <span className="fv-tools">
        {renderable && (
          <span className="fv-segmented" role="group" aria-label="View mode">
            <button type="button" aria-pressed={effectiveMode === 'preview'} onClick={() => setMode('preview')}>
              Preview
            </button>
            <button type="button" aria-pressed={effectiveMode === 'source'} onClick={() => setMode('source')}>
              Source
            </button>
          </span>
        )}
        {view?.kind === 'text' && (!renderable || effectiveMode === 'source') && (
          <button type="button" className="fv-button" aria-pressed={wrap} onClick={() => setWrap((w) => !w)} title="Word wrap">
            Wrap
          </button>
        )}
        <button type="button" className="fv-button" onClick={() => setReload((n) => n + 1)} title="Reload">
          Reload
        </button>
        {view?.text !== undefined && (
          <button
            type="button"
            className="fv-button"
            onClick={() => {
              void navigator.clipboard?.writeText(view.text ?? '').then(() => setCopied(true));
              setTimeout(() => setCopied(false), 1500);
            }}
          >
            {copied ? 'Copied' : view.truncated ? 'Copy preview' : 'Copy'}
          </button>
        )}
        <button type="button" className="fv-button" onClick={() => void ops.reveal(loc.path).catch(() => undefined)} title="Show in File Explorer">
          Show in Explorer
        </button>
        <button type="button" className="fv-button" onClick={() => void navigator.clipboard?.writeText(displayPath(loc.path))}>
          Copy path
        </button>
      </span>
    </div>
  );

  const segments = splitSegments(loc.path);
  const nav = compact ? null : (
    <div className="fv-nav">
      <button type="button" className="fv-nav-button" aria-label="Back" disabled={history.index === 0} onClick={() => setHistory((h) => ({ ...h, index: h.index - 1 }))}>
        ‹
      </button>
      <button
        type="button"
        className="fv-nav-button"
        aria-label="Forward"
        disabled={history.index >= history.stack.length - 1}
        onClick={() => setHistory((h) => ({ ...h, index: h.index + 1 }))}
      >
        ›
      </button>
      <button type="button" className="fv-nav-button" aria-label="Up" disabled={atRoot || !parent} onClick={() => parent && go({ path: parent })}>
        ˄
      </button>
      <nav className="fv-crumbs" aria-label="Path">
        {segments.map((seg, i) => (
          <span key={seg.path} className="fv-crumb-wrap">
            {i > 0 && <span className="fv-crumb-sep">›</span>}
            <button type="button" className="fv-crumb" aria-current={i === segments.length - 1 ? 'page' : undefined} onClick={() => go({ path: seg.path })}>
              {seg.name}
            </button>
          </span>
        ))}
      </nav>
      {loc.line ? (
        <span className="fv-line">
          {loc.endLine && loc.endLine > loc.line ? `Lines ${loc.line}–${loc.endLine}` : `Line ${loc.line}`}
          {loc.column ? `, column ${loc.column}` : ''}
        </span>
      ) : null}
    </div>
  );

  let body;
  if (error) {
    body = (
      <div className="fv-message" role="alert" data-testid="file-error">
        {error}
      </div>
    );
  } else if (!view) {
    body = <div className="fv-message">{loading ? 'Loading…' : ''}</div>;
  } else if (view.kind === 'dir') {
    body = (
      <div className="fv-folder" data-testid="folder-view">
        <div className="fv-folder-head">
          <FolderOpenIcon size={32} />
          <div>
            <div className="fv-folder-name">{segments[segments.length - 1]?.name ?? loc.path}</div>
            <div className="fv-folder-count">
              {view.entries?.length ?? 0} items{view.truncated ? ' (listing limited to 1,000 entries)' : ''}
            </div>
          </div>
        </div>
        {!atRoot && parent && (
          <button type="button" className="fv-entry" onClick={() => go({ path: parent })}>
            <FolderOpenIcon size={17} />
            <span className="fv-entry-name">..</span>
          </button>
        )}
        {(view.entries ?? []).map((e) => (
          <button
            type="button"
            key={e.name}
            className="fv-entry"
            data-testid="folder-entry"
            data-kind={e.kind}
            onClick={() => go({ path: joinPath(loc.path, e.name) })}
          >
            {e.kind === 'dir' ? <FolderOpenIcon size={17} /> : <FileTextIcon size={17} />}
            <span className="fv-entry-name">{e.name}</span>
            <span className="fv-entry-kind">{e.kind === 'dir' ? 'Folder' : e.kind === 'link' ? 'Link' : e.size !== undefined ? formatSize(e.size) : ''}</span>
          </button>
        ))}
      </div>
    );
  } else if (view.kind === 'image') {
    body = (
      <div className="fv-image">
        {imageUrl ? <img src={imageUrl} alt={segments[segments.length - 1]?.name ?? ''} /> : null}
      </div>
    );
  } else if (view.kind === 'html' && effectiveMode === 'preview') {
    // Fully sandboxed: no scripts, no same-origin, no forms, no navigation.
    body = <iframe className="fv-html" title="HTML preview" sandbox="" srcDoc={view.text ?? ''} referrerPolicy="no-referrer" />;
  } else if (isMarkdown && effectiveMode === 'preview') {
    body = (
      <div className="fv-markdown">
        <Markdown text={view.text ?? ''} onLink={onDocLink} />
      </div>
    );
  } else if (view.kind === 'binary') {
    body = (
      <div className="fv-message">
        An embedded preview isn&rsquo;t available for this file.
        <button type="button" className="fv-button" onClick={() => void ops.openDefault(loc.path).catch(() => undefined)}>
          Open in default app
        </button>
      </div>
    );
  } else {
    body = (
      <CodeView text={view.text ?? ''} language={view.language} wrap={wrap} line={loc.line} endLine={loc.endLine} column={loc.column} onSelection={onSelection} />
    );
  }

  return (
    <div className="fv-root" data-testid="file-viewer" data-path={loc.path}>
      {toolbar}
      {nav}
      {view?.truncated && view.kind !== 'dir' ? (
        <div className="fv-notice">Showing the first 512 KiB. Copy includes only this excerpt.</div>
      ) : null}
      <div className="fv-body">{body}</div>
    </div>
  );
}
