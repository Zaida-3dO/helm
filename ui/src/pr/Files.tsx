// Files changed: the file list (grouped, filtered, sorted), each file's diff in a collapsible
// section with a Viewed box, inline threads, and the selection bar (Add to chat, New chat, Comment).

import { useEffect, useMemo, useRef, useState } from 'react';
import { parseDiff } from '../panel/changes/diffModel.ts';
import { Button } from '../ui/index.ts';
import { addToChat, newChatWith, openPrFile } from './actions.ts';
import { fileBlock, linesBlock } from './addToChat.ts';
import { attempt, clickFrom } from './client.ts';
import { Diff, placeThread, rowTarget } from './Diff.tsx';
import type { Selection } from './Diff.tsx';
import { NO_FILTER, SORT_LABEL, STATUS_LABEL, filterFiles, groupByDir, sortFiles, withoutTests } from './files.ts';
import type { FileFilter, SortPreset } from './files.ts';
import type { PrEntry } from './store.ts';
import type { FileStatus, PrFile, Thread } from './types.ts';
import { ErrorBlock, useEntry } from './util.tsx';

interface FilesUi {
  filter: FileFilter;
  sort: SortPreset;
  collapsed: Set<string>;
  loadedDeleted: Set<string>;
  selection: Selection | null;
  box: { path: string; from: number; to: number } | null;
  stamp: number;
  note: string | null;
  pending: Set<string>;
}

function uiOf(entry: PrEntry): FilesUi {
  let u = entry.ui.files as FilesUi | undefined;
  if (!u) {
    u = { filter: NO_FILTER, sort: 'pr', collapsed: new Set(), loadedDeleted: new Set(), selection: null, box: null, stamp: 0, note: null, pending: new Set() };
    entry.ui.files = u;
  }
  return u;
}

const TYPES: FileStatus[] = ['added', 'modified', 'removed', 'renamed'];

export function FilesView({ entry }: { entry: PrEntry }) {
  useEntry(entry);
  const ui = uiOf(entry);
  const loaded = entry.files;
  const [, bump] = useState(0);
  const rerender = () => {
    bump((n) => n + 1);
    entry.changed();
  };

  useEffect(() => {
    if (!loaded && !entry.filesLoading && !entry.filesError) void entry.loadFiles();
  }, [entry, loaded]);

  // Viewed files start collapsed at every round.
  if (loaded && ui.stamp !== loaded.at) {
    ui.stamp = loaded.at;
    ui.collapsed = new Set([...loaded.data.viewed].filter(([, v]) => v === 'VIEWED').map(([p]) => p));
    ui.selection = null;
    ui.box = null;
  }

  const data = loaded?.data;
  const sorted = useMemo(() => (data ? sortFiles(data.files, ui.sort) : []), [data, ui.sort]);
  const visible = data ? filterFiles(sorted, ui.filter, data.viewed) : [];

  if (!loaded) {
    if (entry.filesError) return <ErrorBlock message={entry.filesError} onRetry={() => void entry.loadFiles()} />;
    return <div className="pr-loading">Loading the changed files…</div>;
  }
  const files = loaded.data.files;
  const viewedCount = files.filter((f) => loaded.data.viewed.get(f.path) === 'VIEWED').length;
  const wt = withoutTests(visible);
  const threads = entry.threads?.data ?? [];
  const placeable = (f: PrFile) => {
    if (!f.patch) return [];
    return threads.filter((t) => t.path === f.path);
  };
  // Threads that cannot sit under a line here: outdated, no line, or on a file with no patch.
  const unplaced = threads.filter((t) => {
    const f = files.find((x) => x.path === t.path);
    if (!f || !f.patch || t.outdated || t.line === null) return true;
    const rows = parseDiff(f.patch);
    return !rows.some((r) => placeThread(r, [t]).length > 0);
  });
  const commentOff = entry.newCommits ? 'Refresh to comment on the latest changes' : null;
  const head = loaded.data.headSha;

  const select = (s: Selection | null, extend: boolean) => {
    if (!s) ui.selection = null;
    else if (extend && ui.selection && ui.selection.path === s.path) ui.selection = { path: s.path, from: ui.selection.from, to: s.to };
    else ui.selection = s;
    rerender();
  };

  const toggleViewed = (e: React.ChangeEvent<HTMLInputElement> | React.MouseEvent, f: PrFile, to: boolean) => {
    const click = clickFrom('nativeEvent' in e ? (e.nativeEvent as Event) : null);
    const nodeId = entry.summary?.nodeId;
    if (!click || !nodeId || ui.pending.has(f.path)) return;
    const before = loaded.data.viewed.get(f.path) ?? 'UNVIEWED';
    ui.pending.add(f.path);
    entry.setViewed(f.path, to ? 'VIEWED' : 'UNVIEWED');
    if (to) ui.collapsed.add(f.path);
    rerender();
    void entry
      .enqueue(() => attempt(() => entry.client.markViewed(click, nodeId, f.path, to ? 'VIEWED' : 'UNVIEWED')))
      .then((r) => {
        ui.pending.delete(f.path);
        if (!r.ok) {
          entry.setViewed(f.path, before);
          ui.note = r.kind === 'uncertain' ? 'Couldn’t confirm the Viewed mark; refresh to check.' : r.message;
        }
        rerender();
      });
  };

  const scrollTo = (path: string) => {
    ui.collapsed.delete(path);
    rerender();
    requestAnimationFrame(() => document.getElementById(fileAnchor(path))?.scrollIntoView({ block: 'start' }));
  };

  const selFile = ui.selection ? files.find((f) => f.path === ui.selection!.path) : undefined;
  const selBlock = (): string | null => {
    const s = ui.selection;
    if (!s || !selFile?.patch || !entry.summary) return null;
    return linesBlock(entry.ref, entry.summary, selFile, parseDiff(selFile.patch), s.from, s.to);
  };
  const selLabel = (): string => {
    const s = ui.selection;
    if (!s || !selFile?.patch) return '';
    const rows = parseDiff(selFile.patch);
    const a = rowTarget(rows[Math.min(s.from, s.to)] ?? { kind: 'meta', text: '' });
    const b = rowTarget(rows[Math.max(s.from, s.to)] ?? { kind: 'meta', text: '' });
    const n = (x: typeof a) => (x ? x.line : '?');
    return `${s.path}:${n(a)}${a?.line !== b?.line ? `–${n(b)}` : ''} (${b?.side === 'LEFT' ? 'old' : 'new'})`;
  };

  return (
    <div className="pr-files" data-testid="pr-files">
      {entry.newCommits && (
        <div className="pr-banner" role="status" data-testid="pr-new-commits">
          New commits were pushed to this pull request.
          <Button variant="secondary" onClick={() => void entry.refresh()}>
            Refresh
          </Button>
        </div>
      )}
      <div className="pr-toolbar">
        <input
          className="pr-filter"
          type="search"
          placeholder="Filter files"
          aria-label="Filter files by path"
          value={ui.filter.text}
          onChange={(e) => {
            ui.filter = { ...ui.filter, text: e.target.value };
            rerender();
          }}
        />
        <div className="pr-types" role="group" aria-label="Change types">
          {TYPES.map((t) => (
            <label key={t} className="pr-type">
              <input
                type="checkbox"
                checked={ui.filter.types.has(t)}
                onChange={(e) => {
                  const next = new Set(ui.filter.types);
                  if (e.target.checked) next.add(t);
                  else next.delete(t);
                  ui.filter = { ...ui.filter, types: next };
                  rerender();
                }}
              />
              {STATUS_LABEL[t]}
            </label>
          ))}
          <label className="pr-type">
            <input
              type="checkbox"
              checked={ui.filter.unviewedOnly}
              onChange={(e) => {
                ui.filter = { ...ui.filter, unviewedOnly: e.target.checked };
                rerender();
              }}
            />
            Unviewed only
          </label>
        </div>
        <label className="pr-sort">
          Sort files
          <select
            value={ui.sort}
            onChange={(e) => {
              ui.sort = e.target.value as SortPreset;
              rerender();
            }}
          >
            {(Object.keys(SORT_LABEL) as SortPreset[]).map((k) => (
              <option key={k} value={k}>
                {SORT_LABEL[k]}
              </option>
            ))}
          </select>
        </label>
      </div>
      <div className="pr-stats" data-testid="pr-files-stats">
        <span>
          {viewedCount} of {files.length} viewed
        </span>
        {wt && (
          <span title={`${wt.testFiles} test file${wt.testFiles === 1 ? '' : 's'} with ${wt.testLines} changed lines are left out`}>
            Without tests <span className="pr-add">+{wt.added}</span> <span className="pr-del">−{wt.removed}</span>
          </span>
        )}
        {loaded.truncated && <span className="pr-warn">Only the first {files.length.toLocaleString()} files are shown. Viewed marks cover those.</span>}
        {unplaced.length > 0 && (
          <span>
            {unplaced.length} conversation{unplaced.length === 1 ? ' is' : 's are'} not shown on the code.{' '}
            <button
              type="button"
              className="pr-link-btn"
              onClick={() => entry.goto('conversation')}
            >
              Show in Conversation
            </button>
          </span>
        )}
        {ui.note && (
          <span className="pr-warn" role="alert">
            {ui.note}
          </span>
        )}
      </div>
      {ui.selection && selFile && (
        <div className="pr-selbar" role="toolbar" aria-label="Selected lines" data-testid="pr-selbar">
          <code>{selLabel()}</code>
          <Button
            variant="secondary"
            data-testid="pr-sel-add"
            onClick={() => {
              const b = selBlock();
              ui.note = b ? addToChat(b).text : 'Those lines are not in this diff.';
              rerender();
            }}
          >
            Add to chat
          </Button>
          <Button
            variant="secondary"
            data-testid="pr-sel-newchat"
            onClick={() => {
              const b = selBlock();
              ui.note = b ? newChatWith(b).text : 'Those lines are not in this diff.';
              rerender();
            }}
          >
            New chat
          </Button>
          <Button
            variant="secondary"
            disabled={commentOff !== null}
            title={commentOff ?? undefined}
            onClick={() => {
              const s = ui.selection!;
              ui.box = { path: s.path, from: Math.min(s.from, s.to), to: Math.max(s.from, s.to) };
              rerender();
            }}
          >
            Comment
          </Button>
          <Button variant="text" onClick={() => select(null, false)}>
            Clear
          </Button>
        </div>
      )}
      <div className="pr-files-layout">
        <nav className="pr-filelist" aria-label="Changed files">
          {groupByDir(visible, ui.sort).map((g, gi) => (
            <div key={`${g.dir}-${gi}`} className="pr-dir">
              <div className="pr-dir-name">{g.dir || '(root)'}</div>
              {g.files.map((f) => (
                <FileRow key={f.path} f={f} viewed={loaded.data.viewed.get(f.path) ?? 'UNVIEWED'} disabled={!entry.summary?.nodeId || ui.pending.has(f.path)} onViewed={(e, to) => toggleViewed(e, f, to)} onOpen={() => scrollTo(f.path)} />
              ))}
            </div>
          ))}
          {visible.length === 0 && <div className="pr-empty">No files match.</div>}
        </nav>
        <div className="pr-diffs">
          {visible.map((f) => (
            <FileSection
              key={f.path}
              entry={entry}
              f={f}
              viewed={loaded.data.viewed.get(f.path) ?? 'UNVIEWED'}
              collapsed={ui.collapsed.has(f.path)}
              showDeleted={ui.loadedDeleted.has(f.path)}
              onToggle={() => {
                if (ui.collapsed.has(f.path)) ui.collapsed.delete(f.path);
                else ui.collapsed.add(f.path);
                rerender();
              }}
              onLoadDeleted={() => {
                ui.loadedDeleted.add(f.path);
                rerender();
              }}
              onViewed={(e, to) => toggleViewed(e, f, to)}
              viewedDisabled={!entry.summary?.nodeId || ui.pending.has(f.path)}
              threads={placeable(f)}
              selection={ui.selection}
              onSelect={(s, extend) => select(s, extend)}
              commentOff={commentOff}
              head={head}
              openBox={ui.box}
              setOpenBox={(b) => {
                ui.box = b;
                rerender();
              }}
              onNote={(t) => {
                ui.note = t;
                rerender();
              }}
            />
          ))}
        </div>
      </div>
    </div>
  );
}

const fileAnchor = (path: string) => `pr-file-${path.replace(/[^A-Za-z0-9_-]/g, '_')}`;

function FileRow({
  f,
  viewed,
  disabled,
  onViewed,
  onOpen,
}: {
  f: PrFile;
  viewed: string;
  disabled: boolean;
  onViewed: (e: React.ChangeEvent<HTMLInputElement>, to: boolean) => void;
  onOpen: () => void;
}) {
  const name = f.path.slice(f.path.lastIndexOf('/') + 1);
  return (
    <div className={`pr-file-row status-${f.status}`} data-testid="pr-file-row">
      <input
        type="checkbox"
        checked={viewed === 'VIEWED'}
        disabled={disabled}
        aria-label={`Viewed: ${f.path}`}
        title={viewed === 'VIEWED' ? 'Viewed' : viewed === 'DISMISSED' ? 'Changed since viewed' : 'Not viewed'}
        onChange={(e) => onViewed(e, e.target.checked)}
      />
      <button type="button" className="pr-file-name" onClick={onOpen} title={f.previousPath ? `${f.previousPath} → ${f.path}` : f.path}>
        {name}
      </button>
      {viewed === 'DISMISSED' && <span className="pr-mark">changed</span>}
      <span className="pr-file-stat">
        <span className="pr-add">+{f.additions}</span> <span className="pr-del">−{f.deletions}</span>
      </span>
    </div>
  );
}

function FileSection({
  entry,
  f,
  viewed,
  collapsed,
  showDeleted,
  onToggle,
  onLoadDeleted,
  onViewed,
  viewedDisabled,
  threads,
  selection,
  onSelect,
  commentOff,
  head,
  openBox,
  setOpenBox,
  onNote,
}: {
  entry: PrEntry;
  f: PrFile;
  viewed: string;
  collapsed: boolean;
  showDeleted: boolean;
  onToggle: () => void;
  onLoadDeleted: () => void;
  onViewed: (e: React.ChangeEvent<HTMLInputElement>, to: boolean) => void;
  viewedDisabled: boolean;
  threads: Thread[];
  selection: Selection | null;
  onSelect: (s: Selection | null, extend: boolean) => void;
  commentOff: string | null;
  head: string;
  openBox: { path: string; from: number; to: number } | null;
  setOpenBox: (b: { path: string; from: number; to: number } | null) => void;
  onNote: (text: string) => void;
}) {
  const ref = useRef<HTMLElement>(null);
  const removed = f.status === 'removed';
  const summary = entry.summary;
  return (
    <section ref={ref} id={fileAnchor(f.path)} className="pr-section" data-testid="pr-file" data-path={f.path}>
      <header className="pr-section-head" onClick={(e) => (e.target as HTMLElement).closest('button,input,label') ? undefined : onToggle()}>
        <button type="button" className="pr-chevron" aria-expanded={!collapsed} aria-label={`Diff of ${f.path}`} title={collapsed ? 'Expand' : 'Collapse'} onClick={onToggle}>
          {collapsed ? '▸' : '▾'}
        </button>
        <button
          type="button"
          className="pr-path"
          disabled={removed}
          title={removed ? 'This file was removed' : 'Open this file'}
          onClick={() => void openPrFile(entry.ref, f.path, head).then((n) => n && onNote(n))}
        >
          {f.previousPath ? `${f.previousPath} → ${f.path}` : f.path}
        </button>
        <button
          type="button"
          className="pr-link-btn"
          onClick={() => void navigator.clipboard?.writeText(f.path).then(() => onNote('Path copied.'), () => onNote('Could not copy the path.'))}
        >
          Copy path
        </button>
        <span className="pr-file-stat">
          <span className="pr-add">+{f.additions}</span> <span className="pr-del">−{f.deletions}</span>
        </span>
        <button
          type="button"
          className="pr-link-btn"
          data-testid="pr-file-add"
          onClick={() => {
            if (!summary) return;
            onNote(addToChat(fileBlock(entry.ref, summary, f)).text);
          }}
        >
          Add file to chat
        </button>
        <label className="pr-type">
          <input type="checkbox" checked={viewed === 'VIEWED'} disabled={viewedDisabled} aria-label={`Viewed: ${f.path}`} onChange={(e) => onViewed(e, e.target.checked)} />
          Viewed
        </label>
      </header>
      {!collapsed &&
        (removed && !showDeleted ? (
          <div className="pr-nodiff">
            This file was deleted.{' '}
            <button type="button" className="pr-link-btn" onClick={onLoadDeleted}>
              Load diff
            </button>
          </div>
        ) : f.patch ? (
          <Diff entry={entry} file={f} threads={threads} selection={selection} onSelect={(s, extend) => onSelect(s, extend)} commentOff={commentOff} head={head} openBox={openBox} setOpenBox={setOpenBox} />
        ) : (
          <div className="pr-nodiff">{/\.(png|jpe?g|gif|webp|svg|mp4|mov|webm)$/i.test(f.path) ? 'Image or video file, not shown.' : 'Binary file, or too large to show here.'}</div>
        ))}
    </section>
  );
}
