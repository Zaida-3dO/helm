// One file's diff from GitHub's patch, unified or side by side: line numbers, a gutter "+" to comment,
// a selection by line number for Add to chat, and the inline threads that sit under their line. Long
// diffs draw only the rows near the view. Text only: nothing in a diff can fetch or run.

import { useLayoutEffect, useMemo, useRef, useState } from 'react';
import { parseDiff } from '../panel/changes/diffModel.ts';
import type { DiffRow } from '../panel/changes/diffModel.ts';
import { panelStore } from '../panel/registry.ts';
import { errorText } from './client.ts';
import { expandWhole, loadWholeFile, pairSplit, visibleRange, withSrc } from './files.ts';
import type { DiffMode, SplitItem, SrcRow } from './files.ts';
import type { PrEntry } from './store.ts';
import { ThreadCard } from './Threads.tsx';
import { WriteBox } from './util.tsx';
import type { PrFile, Side, Thread } from './types.ts';

export interface Selection {
  path: string;
  from: number;
  to: number;
}

/** Items (lines, or side-by-side pairs) beyond which only the ones near the view are drawn. */
export const WINDOW_MIN = 400;
/** Pixels drawn above and below the view. */
const OVERSCAN = 600;
const GUESS_HEIGHT = 20;

/** The thread a row carries, by the side and line number GitHub gave it. Outdated threads are never placed. */
export function placeThread(row: DiffRow, threads: readonly Thread[]): Thread[] {
  return threads.filter((t) => {
    if (t.outdated || t.line === null) return false;
    if (t.side === 'RIGHT') return row.newNo === t.line && row.kind !== 'del';
    return row.oldNo === t.line && row.kind === 'del';
  });
}

/** Which side and line number a row is commented on: deletions on the left, everything else on the right. */
export function rowTarget(row: DiffRow): { side: Side; line: number } | null {
  if (row.kind === 'del' && row.oldNo !== undefined) return { side: 'LEFT', line: row.oldNo };
  if ((row.kind === 'add' || row.kind === 'ctx') && row.newNo !== undefined) return { side: 'RIGHT', line: row.newNo };
  return null;
}

export const SIGN = { add: '+', del: '-', ctx: ' ', hunk: '', meta: '' } as const;

export function lineBoxId(path: string, side: Side, start: number | null, line: number): string {
  return `line:${path}:${side}:${start ?? ''}-${line}`;
}


interface LineCtx {
  path: string;
  /** The selected rows, as patch row numbers; -1 for none. */
  lo: number;
  hi: number;
  commentOff: string | null;
  onSelect: (s: Selection | null, extend: boolean, row: number) => void;
  setOpenBox: (b: { path: string; from: number; to: number } | null) => void;
}

/** One line of a diff: whole width in the unified view, or one half of a pair in the split view. */
function LineRow({ row, side, c }: { row: SrcRow; side: 'left' | 'right' | null; c: LineCtx }) {
  // A line filled in from the whole file is not part of the patch: it can be neither selected nor commented on.
  const target = row.src >= 0 ? rowTarget(row) : null;
  const selected = row.src >= 0 && row.src >= c.lo && row.src <= c.hi;
  const inRange = c.hi > c.lo && row.src >= c.lo && row.src <= c.hi;
  const nums = side === 'left' ? [row.oldNo] : side === 'right' ? [row.newNo] : [row.oldNo, row.newNo];
  return (
    <div className={`ch-line ${row.kind} pr-line${selected ? ' selected' : ''}`} role="row" data-row={row.src}>
      {row.kind === 'hunk' ? (
        <span className="ch-code">{row.text}</span>
      ) : (
        <>
          {nums.map((n, k) => (
            <button
              key={k}
              type="button"
              className="ch-gutter pr-num"
              aria-label={`Select line ${row.newNo ?? row.oldNo ?? ''}`}
              disabled={!target}
              onClick={(e) => c.onSelect({ path: c.path, from: row.src, to: row.src }, e.shiftKey, row.src)}
            >
              {n ?? ''}
            </button>
          ))}
          <span className="ch-sign">{SIGN[row.kind]}</span>
          <span className="ch-code">{row.text}</span>
          {target && (
            <button
              type="button"
              className="pr-plus"
              aria-label="Comment on this line"
              title={c.commentOff ?? 'Comment on this line'}
              disabled={c.commentOff !== null}
              data-testid="pr-line-plus"
              onClick={() => c.setOpenBox({ path: c.path, from: inRange ? c.lo : row.src, to: inRange ? c.hi : row.src })}
            >
              +
            </button>
          )}
        </>
      )}
    </div>
  );
}

/** The nearest ancestor that scrolls vertically, or null when the page itself does. */
function scrollParent(el: HTMLElement | null): HTMLElement | null {
  for (let p = el?.parentElement ?? null; p; p = p.parentElement) {
    const o = getComputedStyle(p).overflowY;
    if (o === 'auto' || o === 'scroll') return p;
  }
  return null;
}

export function Diff({
  entry,
  file,
  threads,
  selection,
  onSelect,
  commentOff,
  head,
  openBox,
  setOpenBox,
  mode,
}: {
  entry: PrEntry;
  file: PrFile;
  threads: readonly Thread[];
  selection: Selection | null;
  onSelect: (s: Selection | null, extend: boolean, row: number) => void;
  /** Why new comments are off (shown as the "+" tooltip), or null. */
  commentOff: string | null;
  head: string;
  openBox: { path: string; from: number; to: number } | null;
  setOpenBox: (b: { path: string; from: number; to: number } | null) => void;
  mode: DiffMode;
}) {
  const rows = useMemo(() => (file.patch ? parseDiff(file.patch) : []), [file.patch]);
  const [whole, setWhole] = useState<string[] | null>(null);
  const [load, setLoad] = useState<{ busy: boolean; error: string | null }>({ busy: false, error: null });
  const items = useMemo<SplitItem<SrcRow>[]>(() => {
    const base = whole ? expandWhole(rows, whole) : withSrc(rows);
    return mode === 'split' ? pairSplit(base) : base.map((row) => ({ kind: 'full' as const, row }));
  }, [rows, whole, mode]);

  // Windowing: heights are guessed until an item is drawn, then measured. Items that carry a thread or
  // a comment box are taller than a plain line, so each of those is measured on its own.
  const rootRef = useRef<HTMLDivElement>(null);
  const windowed = items.length > WINDOW_MIN;
  const [view, setView] = useState({ top: 0, height: 900 });
  const [, setTick] = useState(0);
  const measured = useRef<{ items: unknown; own: Map<number, number>; plain: number }>({ items, own: new Map(), plain: GUESS_HEIGHT });
  if (measured.current.items !== items) measured.current = { items, own: new Map(), plain: measured.current.plain };
  const m = measured.current;
  const range = windowed
    ? visibleRange(
        items.map((_, i) => m.own.get(i) ?? m.plain),
        view.top,
        view.height,
        OVERSCAN,
      )
    : { start: 0, end: items.length, before: 0, after: 0 };

  useLayoutEffect(() => {
    const el = rootRef.current;
    if (!windowed || !el) return;
    const sp = scrollParent(el);
    const target: EventTarget = sp ?? window;
    let raf = 0;
    const measure = () => {
      raf = 0;
      const top = (sp ? sp.getBoundingClientRect().top : 0) - el.getBoundingClientRect().top;
      const height = sp ? sp.clientHeight : window.innerHeight;
      setView((v) => (Math.abs(v.top - top) < 1 && v.height === height ? v : { top, height }));
    };
    const on = () => {
      if (!raf) raf = requestAnimationFrame(measure);
    };
    measure();
    target.addEventListener('scroll', on, { passive: true });
    window.addEventListener('resize', on);
    return () => {
      if (raf) cancelAnimationFrame(raf);
      target.removeEventListener('scroll', on);
      window.removeEventListener('resize', on);
    };
  }, [windowed]);

  useLayoutEffect(() => {
    if (!windowed) return;
    let changed = false;
    rootRef.current?.querySelectorAll<HTMLElement>('[data-item]').forEach((el) => {
      const h = el.offsetHeight;
      if (h <= 0) return;
      const i = Number(el.dataset.item);
      if (el.dataset.plain === '1') {
        if (Math.abs(h - m.plain) > 0.5) {
          m.plain = h;
          changed = true;
        }
        if (m.own.delete(i)) changed = true;
      } else if (Math.abs((m.own.get(i) ?? -1) - h) > 0.5) {
        m.own.set(i, h);
        changed = true;
      }
    });
    if (changed) setTick((t) => t + 1);
  });

  const sel = selection && selection.path === file.path ? selection : null;
  const c: LineCtx = {
    path: file.path,
    lo: sel ? Math.min(sel.from, sel.to) : -1,
    hi: sel ? Math.max(sel.from, sel.to) : -1,
    commentOff,
    onSelect,
    setOpenBox,
  };

  const loadWhole = () => {
    const api = panelStore.api;
    if (!api) {
      setLoad({ busy: false, error: 'The panel is not connected to the host yet.' });
      return;
    }
    setLoad({ busy: true, error: null });
    loadWholeFile(api, entry.ref, file.path, head).then(
      (lines) => {
        setWhole(lines);
        setLoad({ busy: false, error: null });
      },
      (e: unknown) => setLoad({ busy: false, error: errorText(e) }),
    );
  };

  if (rows.length === 0) return <div className="pr-nodiff">No changes to show.</div>;

  const renderItem = (it: SplitItem<SrcRow>, i: number) => {
    const own = it.kind === 'full' ? [it.row] : [...new Set([it.left, it.right].filter((r): r is SrcRow => r !== null))];
    const placed = own.flatMap((r) => (r.src >= 0 ? placeThread(r, threads) : []));
    const inBox = openBox !== null && openBox.path === file.path && own.some((r) => r.src === openBox.to);
    const half = (r: SrcRow | null, side: 'left' | 'right') => (r ? <LineRow row={r} side={side} c={c} /> : <div className="ch-line pr-line pr-blank" aria-hidden="true" />);
    return (
      <div key={i} data-item={i} data-plain={placed.length === 0 && !inBox ? '1' : undefined}>
        {it.kind === 'full' ? (
          <LineRow row={it.row} side={null} c={c} />
        ) : (
          <div className="pr-split">
            {half(it.left, 'left')}
            {half(it.right, 'right')}
          </div>
        )}
        {placed.map((t) => (
          <ThreadCard key={t.id} entry={entry} thread={t} inline />
        ))}
        {inBox && <LineBox entry={entry} file={file} rows={rows} from={openBox.from} to={openBox.to} head={head} onClose={() => setOpenBox(null)} />}
      </div>
    );
  };

  return (
    <>
      {file.status !== 'removed' && (
        <div className="pr-diff-tools">
          <button type="button" className="pr-link-btn" data-testid="pr-load-whole" disabled={load.busy || !head} onClick={whole ? () => setWhole(null) : loadWhole}>
            {whole ? 'Show changes only' : load.busy ? 'Loading the whole file…' : 'Load whole file'}
          </button>
          {load.error && (
            <span className="pr-warn" role="alert">
              {load.error}
            </span>
          )}
        </div>
      )}
      <div ref={rootRef} className={`ch-diff pr-diff${mode === 'split' ? ' split' : ''}`} role="table" aria-label={`Diff of ${file.path}`} data-testid="pr-diff">
        {range.before > 0 && <div style={{ height: range.before }} aria-hidden="true" />}
        {items.slice(range.start, range.end).map((it, k) => renderItem(it, range.start + k))}
        {range.after > 0 && <div style={{ height: range.after }} aria-hidden="true" />}
      </div>
    </>
  );
}

/** The comment box under a line or a range. A range counts only when both ends are on one side. */
function LineBox({ entry, file, rows, from, to, head, onClose }: { entry: PrEntry; file: PrFile; rows: DiffRow[]; from: number; to: number; head: string; onClose: () => void }) {
  const end = rowTarget(rows[to] as DiffRow);
  const start = rowTarget(rows[from] as DiffRow);
  if (!end) return null;
  const range = start && start.side === end.side && start.line !== end.line;
  const startLine = range && start ? Math.min(start.line, end.line) : null;
  const line = range && start ? Math.max(start.line, end.line) : end.line;
  const id = lineBoxId(file.path, end.side, startLine, line);
  const where = startLine ? `lines ${startLine} to ${line}` : `line ${line}`;
  return (
    <div className="pr-linebox">
      <div className="pr-fine">
        Commenting on {where}
        {end.side === 'LEFT' ? ' (the old version)' : ''}
      </div>
      <WriteBox
        entry={entry}
        id={id}
        label={`Comment on ${where}`}
        rows={3}
        autoFocus
        onCancel={onClose}
        actions={[
          {
            label: 'Add single comment',
            primary: true,
            run: (click, text) =>
              entry.client.reviewComment(click, entry.ref, { head, path: file.path, line, side: end.side, ...(startLine ? { startLine } : {}), body: text }),
            done: () => {
              onClose();
              void entry.loadThreads();
              void entry.afterPublish();
            },
            uncertain: () => {
              void entry.loadThreads();
              void entry.poll();
            },
          },
        ]}
      />
    </div>
  );
}
