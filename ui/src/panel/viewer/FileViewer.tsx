// The file viewer body, shared by `file` tabs, the Inspector's Files
// slot and the modal viewer. It reads only while it is shown (a restored tab reads nothing until it
// is selected), keeps its own back/forward history, follows the file on disk while it is on screen
// (a cheap check of the modified time, then a re-read that keeps the reading position), and never
// executes what it previews.

import { useEffect, useMemo, useRef, useState } from 'react';
import type { FileView, PanelOps } from '../../api/panel.ts';
import { Markdown } from '../../markdown/index.ts';
import { Button } from '../../ui/index.ts';
import type { LinkTarget } from '../../markdown/index.ts';
import { BackIcon, BrowseFolderIcon, ChevronIcon, CopyIcon, EyeIcon, EyeOffIcon, FileTextIcon, FolderOpenIcon, FolderTreeIcon, ForwardIcon, ReloadIcon, UpIcon, WrapIcon } from '../icons.tsx';
import { gateTarget, handleLink } from '../links.ts';
import { openInPanel } from '../registry.ts';
import { CodeView } from './CodeView.tsx';
import { dirname, displayPath, joinPath, pathKey, splitSegments } from './paths.ts';
import { changedSince, LIVE_POLL_MS, noteShown, sameContent, UPDATED_DOT_MS, updatedLabel } from './live.ts';
import { sizeLabel } from './preview.ts';
import type { ViewMode } from './tabdata.ts';
import { ArchivePreview, canPreview, FontPreview, ImagePreview, JsonPreview, MediaPreview, PdfPreview, TablePreview } from './previews.tsx';

export interface Location {
  path: string;
  line?: number;
  /** The last line of a linked range. */
  endLine?: number;
  column?: number;
}

const HISTORY_LIMIT = 100;

const formatSize = sizeLabel;

const KIND_LABEL: Record<string, string> = {
  typescript: 'TypeScript', javascript: 'JavaScript', json: 'JSON', markdown: 'Markdown', rust: 'Rust',
  python: 'Python', css: 'CSS', scss: 'SCSS', xml: 'XML', yaml: 'YAML', ini: 'Config', bash: 'Shell',
  powershell: 'PowerShell', dos: 'Batch', go: 'Go', java: 'Java', csharp: 'C#', c: 'C', cpp: 'C++',
  sql: 'SQL', diff: 'Diff',
};

/** How many entries a folder listing shows, with hidden (dot) entries counted only when shown. */
function shownEntries(entries: FileView['entries'], showHidden: boolean): number {
  return (entries ?? []).filter((e) => showHidden || !e.name.startsWith('.')).length;
}

function kindLabel(v: FileView): string {
  if (v.kind === 'dir') return 'Folder';
  if (v.kind === 'image') return 'Image';
  if (v.kind === 'html') return 'HTML';
  if (v.kind === 'binary') return 'Binary';
  if (v.kind === 'pdf') return 'PDF';
  if (v.kind === 'audio') return 'Audio';
  if (v.kind === 'video') return 'Video';
  if (v.kind === 'font') return 'Font';
  if (v.kind === 'archive') return 'Archive';
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
  /** Follow the file on disk: checked about once a second, only while true (shown, window focused). */
  live?: boolean;
  /** Preview or Source to start in, and the first visible line to come back to (a restored tab). */
  initialMode?: ViewMode | null;
  initialTop?: number;
  /** The person chose a mode / scrolled the start file's source: the tab keeps it. */
  onMode?: (mode: ViewMode) => void;
  onTop?: (line: number) => void;
  /** The tab's link to the Files tree: why Show in Files is unavailable (null: it is), whether the tab was opened from the tree, and the action. */
  filesLink?: { refusal: string | null; fromFiles: boolean; show: (path: string) => void };
}

export function FileViewer({ ops, start, chatId, visible, onLocation, root, compact, onSelection, live, initialMode, initialTop, onMode, onTop, filesLink }: FileViewerProps) {
  const [history, setHistory] = useState<{ stack: Location[]; index: number }>({ stack: [start], index: 0 });
  const [view, setView] = useState<FileView | null>(null);
  const [loadedFor, setLoadedFor] = useState<Location | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(false);
  const [reload, setReload] = useState(0);
  const [wrap, setWrap] = useState(false);
  const [mode, setMode] = useState<ViewMode | null>(initialMode ?? null);
  // When the file last changed under the person's eyes (the breadcrumb's dot), and a tick to age it.
  const [updated, setUpdated] = useState<{ path: string; at: number } | null>(null);
  const [, setAge] = useState(0);
  // A live refresh keeps what is on screen and shows no spinner.
  const quietRead = useRef(false);
  const shownView = useRef<FileView | null>(null);
  const hasError = useRef(false);
  // The modified time of what is on screen: a later one on disk means the file changed.
  const baseline = useRef<number | undefined>(undefined);
  const [copied, setCopied] = useState(false);
  const [showHidden, setShowHidden] = useState(false);

  shownView.current = view;
  hasError.current = error !== null;

  // A new start (the same file reopened at another line) resets the history.
  const lastStart = useRef(start);
  useEffect(() => {
    if (lastStart.current === start) return;
    lastStart.current = start;
    setHistory({ stack: [start], index: 0 });
    setMode(null);
  }, [start]);

  const loc = history.stack[history.index] as Location;

  useEffect(() => {
    if (!visible) return;
    if (loadedFor === loc && reload === 0) return;
    let cancelled = false;
    const quiet = quietRead.current && loadedFor === loc;
    quietRead.current = false;
    if (!quiet) setLoading(true);
    ops
      .fileRead(loc.path, chatId)
      .then((v) => {
        if (cancelled) return;
        baseline.current = v.modified;
        noteShown(loc.path, v.modified);
        // A re-read that shows the same thing redraws nothing.
        if (quiet && sameContent(shownView.current, v)) return;
        if (quiet) setUpdated({ path: loc.path, at: Date.now() });
        setView(v);
        setError(null);
        setLoadedFor(loc);
      })
      .catch((e: unknown) => {
        if (cancelled) return;
        // A file caught mid-write or just removed keeps the last good view until the next change.
        if (quiet) return;
        baseline.current = undefined;
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

  // Following the file: while it is on screen, ask the host about its modified time (never its
  // contents) and re-read quietly when it moved. A file that was missing and comes back is read too.
  useEffect(() => {
    if (!live || !visible || loadedFor !== loc) return;
    let stopped = false;
    let busy = false;
    const refresh = () => {
      quietRead.current = true;
      setReload((n) => n + 1);
    };
    const tick = () => {
      if (busy) return;
      busy = true;
      ops
        .fileStat([loc.path])
        .then(([stamp]) => {
          if (stopped || !stamp) return;
          const before = baseline.current;
          if (before === undefined) {
            baseline.current = stamp.modified;
            if (hasError.current) refresh();
          } else if (changedSince(before, stamp)) {
            baseline.current = stamp.modified;
            refresh();
          }
        })
        .catch(() => undefined)
        .finally(() => {
          busy = false;
        });
    };
    const timer = setInterval(tick, LIVE_POLL_MS);
    return () => {
      stopped = true;
      clearInterval(timer);
    };
  }, [live, visible, loc, loadedFor, ops]);

  // The dot ages in whole seconds and goes after a few.
  const updatedAt = updated && updated.path === loc.path ? updated.at : null;
  useEffect(() => {
    if (updatedAt === null) return;
    const timer = setInterval(() => {
      if (Date.now() - updatedAt >= UPDATED_DOT_MS) {
        clearInterval(timer);
        setUpdated(null);
      } else setAge((n) => n + 1);
    }, 1000);
    return () => clearInterval(timer);
  }, [updatedAt]);

  const imageUrl = useObjectUrl(view);

  const go = (next: Location) =>
    setHistory((h) => {
      const stack = [...h.stack.slice(0, h.index + 1), next].slice(-HISTORY_LIMIT);
      return { stack, index: stack.length - 1 };
    });

  const isMarkdown = view?.kind === 'text' && view.language === 'markdown';
  const richKind = view ? canPreview(loc.path, view.kind, view.language) : null;
  const renderable = isMarkdown || view?.kind === 'html' || richKind !== null;
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

  const segments = splitSegments(loc.path);
  const name = segments[segments.length - 1]?.name ?? loc.path;
  const showWrap = view?.kind === 'text' && (!renderable || effectiveMode === 'source');
  const canGoBack = history.index > 0;
  const chooseMode = (m: ViewMode) => {
    setMode(m);
    onMode?.(m);
  };
  const toolbar = (
    <div className={compact ? 'fv-toolbar compact' : 'fv-toolbar'}>
      <span className="fv-label">
        {compact && canGoBack && (
          <button type="button" className="fv-icon-btn" aria-label="Back" title="Back" onClick={() => setHistory((h) => ({ ...h, index: h.index - 1 }))}>
            <BackIcon />
          </button>
        )}
        {filesLink?.fromFiles && (
          <button
            type="button"
            className="fv-icon-btn"
            data-testid="back-to-files"
            aria-label="Back to files"
            title="Back to files"
            disabled={filesLink.refusal !== null}
            onClick={() => filesLink.show(loc.path)}
          >
            <BackIcon />
          </button>
        )}
        {view?.kind === 'dir' ? <FolderOpenIcon size={14} /> : <FileTextIcon size={14} />}
        <span className="fv-label-text">
          {compact ? name : view ? (view.kind === 'dir' ? 'Folder' : `${formatSize(view.size)} · ${kindLabel(view)}`) : loading ? 'Loading…' : ''}
        </span>
      </span>
      <span className="fv-tools">
        {renderable && (
          <span className="fv-segmented" role="group" aria-label="View mode">
            <button type="button" aria-pressed={effectiveMode === 'preview'} onClick={() => chooseMode('preview')}>
              Preview
            </button>
            <button type="button" aria-pressed={effectiveMode === 'source'} onClick={() => chooseMode('source')}>
              Source
            </button>
          </span>
        )}
        {view?.kind === 'dir' && (
          <button
            type="button"
            className="fv-icon-btn"
            aria-pressed={showHidden}
            aria-label="Show hidden files"
            title={showHidden ? 'Hide hidden files' : 'Show hidden files'}
            onClick={() => setShowHidden((v) => !v)}
          >
            {showHidden ? <EyeIcon /> : <EyeOffIcon />}
          </button>
        )}
        {showWrap && (
          <button type="button" className="fv-icon-btn" aria-pressed={wrap} aria-label="Word wrap" title="Word wrap" onClick={() => setWrap((w) => !w)}>
            <WrapIcon />
          </button>
        )}
        {filesLink && (
          <Button
            className="fv-tool"
            data-testid="show-in-files"
            disabled={filesLink.refusal !== null}
            title={filesLink.refusal ?? 'Show this file in the Files tab'}
            onClick={() => filesLink.show(loc.path)}
          >
            <FolderTreeIcon />
            <span className="fv-btn-text">Show in Files</span>
          </Button>
        )}
        {view?.text !== undefined && (
          <Button
            className="fv-tool"
            title={view.truncated ? 'Copy the excerpt shown' : 'Copy the file contents'}
            onClick={() => {
              void navigator.clipboard?.writeText(view.text ?? '').then(() => setCopied(true));
              setTimeout(() => setCopied(false), 1500);
            }}
          >
            <CopyIcon />
            <span className="fv-btn-text">{copied ? 'Copied' : view.truncated ? 'Copy preview' : 'Copy'}</span>
          </Button>
        )}
        <button type="button" className="fv-icon-btn" aria-label="Show in File Explorer" title="Show in File Explorer" onClick={() => void ops.reveal(loc.path).catch(() => undefined)}>
          <FolderOpenIcon />
        </button>
        <Button className="fv-tool" title="Copy path" onClick={() => void navigator.clipboard?.writeText(displayPath(loc.path))}>
          <CopyIcon />
          <span className="fv-btn-text">Copy path</span>
        </Button>
      </span>
    </div>
  );

  const openFolderTab = () => {
    if (!parent) return;
    if (!openInPanel('file', pathKey(parent), { path: parent }, { intent: 'user', chatId })) go({ path: parent });
  };
  const nav = compact ? null : (
    <div className="fv-nav">
      <button type="button" className="fv-nav-button" aria-label="Back" title="Back" disabled={!canGoBack} onClick={() => setHistory((h) => ({ ...h, index: h.index - 1 }))}>
        <BackIcon />
      </button>
      <button
        type="button"
        className="fv-nav-button"
        aria-label="Forward"
        title="Forward"
        disabled={history.index >= history.stack.length - 1}
        onClick={() => setHistory((h) => ({ ...h, index: h.index + 1 }))}
      >
        <ForwardIcon />
      </button>
      <button type="button" className="fv-nav-button" aria-label="Up" title="Up one folder" disabled={atRoot || !parent} onClick={() => parent && go({ path: parent })}>
        <UpIcon />
      </button>
      <nav className="fv-crumbs" aria-label="Path">
        {segments.map((seg, i) => (
          <span key={seg.path} className="fv-crumb-wrap">
            <span className="fv-crumb-sep">{i > 0 ? <ChevronIcon size={12} /> : null}</span>
            <button type="button" className="fv-crumb" aria-current={i === segments.length - 1 ? 'page' : undefined} onClick={() => go({ path: seg.path })}>
              {seg.name}
            </button>
          </span>
        ))}
      </nav>
      {updatedAt !== null && (
        <span className="fv-live" data-testid="live-dot" role="status">
          <span className="fv-live-dot" aria-hidden />
          {updatedLabel(Date.now() - updatedAt)}
        </span>
      )}
      {loc.line ? (
        <span className="fv-line">
          {loc.endLine && loc.endLine > loc.line ? `Lines ${loc.line}–${loc.endLine}` : `Line ${loc.line}`}
          {loc.column ? `, column ${loc.column}` : ''}
        </span>
      ) : null}
      {parent && (
        <button type="button" className="fv-button browse" title="Open this folder in its own tab" onClick={openFolderTab}>
          <BrowseFolderIcon />
          <span className="fv-btn-text">Browse folder</span>
        </button>
      )}
    </div>
  );

  let body;
  if (error) {
    body = (
      <div className="fv-message" role="alert" data-testid="file-error">
        {error}
        <Button className="fv-tool" data-testid="file-retry" onClick={() => setReload((n) => n + 1)}>
          <ReloadIcon />
          <span className="fv-btn-text">Try again</span>
        </Button>
      </div>
    );
  } else if (!view) {
    body = <div className="fv-message">{loading ? 'Loading…' : ''}</div>;
  } else if (view.kind === 'dir') {
    body = (
      <div className={compact ? 'fv-folder compact' : 'fv-folder'} data-testid="folder-view">
        {!compact && (
          <div className="fv-folder-head">
            <FolderOpenIcon size={32} />
            <div>
              <div className="fv-folder-name">{name}</div>
              <div className="fv-folder-count">
                {shownEntries(view.entries, showHidden)} items{view.truncated ? ' (listing limited to 1,000 entries)' : ''}
              </div>
            </div>
          </div>
        )}
        {!atRoot && parent && (
          <button type="button" className="fv-entry" onClick={() => go({ path: parent })}>
            <BackIcon size={compact ? 13 : 17} />
            <span className="fv-entry-name">..</span>
          </button>
        )}
        {(view.entries ?? []).filter((e) => showHidden || !e.name.startsWith('.')).map((e) => (
          <button
            type="button"
            key={e.name}
            className="fv-entry"
            data-testid="folder-entry"
            data-kind={e.kind}
            onClick={() => go({ path: joinPath(loc.path, e.name) })}
          >
            {e.kind === 'dir' ? <FolderOpenIcon size={compact ? 13 : 17} /> : <FileTextIcon size={compact ? 13 : 17} />}
            <span className="fv-entry-name">{e.name}</span>
            {compact ? (
              e.kind === 'dir' && <ChevronIcon className="fv-entry-chevron" />
            ) : (
              <span className="fv-entry-kind">{e.kind === 'dir' ? 'Folder' : e.kind === 'link' ? 'Link' : e.size !== undefined ? formatSize(e.size) : ''}</span>
            )}
          </button>
        ))}
        {compact && view.truncated && <div className="fv-notice">Listing limited to 1,000 entries.</div>}
      </div>
    );
  } else if (view.kind === 'image') {
    body = <ImagePreview url={imageUrl} name={name} size={view.size} />;
  } else if (view.kind === 'pdf') {
    body = <PdfPreview url={view.url} name={name} />;
  } else if (view.kind === 'audio' || view.kind === 'video') {
    body = <MediaPreview url={view.url} name={name} kind={view.kind} />;
  } else if (view.kind === 'font') {
    body = <FontPreview url={view.url} name={name} />;
  } else if (view.kind === 'archive') {
    body = <ArchivePreview ops={ops} path={loc.path} chatId={chatId} />;
  } else if (view.kind === 'html' && effectiveMode === 'preview') {
    // Fully sandboxed: no scripts, no same-origin, no forms, no navigation.
    body = <iframe className="fv-html" title="HTML preview" sandbox="" srcDoc={view.text ?? ''} referrerPolicy="no-referrer" />;
  } else if (richKind === 'json' && effectiveMode === 'preview') {
    body = <JsonPreview text={view.text ?? ''} />;
  } else if (richKind === 'table' && effectiveMode === 'preview') {
    body = <TablePreview text={view.text ?? ''} path={loc.path} />;
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
      <CodeView
        text={view.text ?? ''}
        language={view.language}
        wrap={wrap}
        line={loc.line}
        endLine={loc.endLine}
        column={loc.column}
        onSelection={onSelection}
        fileKey={loc.path}
        topLine={loc.path === start.path ? initialTop : undefined}
        onTopLine={onTop && loc.path === start.path ? onTop : undefined}
      />
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
