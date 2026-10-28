// One file's unified diff from GitHub's patch: line numbers, a gutter "+" to comment, a selection by
// line number for Add to chat, and the inline threads that sit under their line. Text only: nothing
// in a diff can fetch or run.

import { useMemo, useState } from 'react';
import { parseDiff } from '../panel/changes/diffModel.ts';
import type { DiffRow } from '../panel/changes/diffModel.ts';
import type { PrEntry } from './store.ts';
import { ThreadCard } from './Threads.tsx';
import { WriteBox } from './util.tsx';
import type { PrFile, Side, Thread } from './types.ts';

export interface Selection {
  path: string;
  from: number;
  to: number;
}

const ROW_CAP = 1500;

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
}) {
  const rows = useMemo(() => (file.patch ? parseDiff(file.patch) : []), [file.patch]);
  const [all, setAll] = useState(false);
  const shown = all ? rows : rows.slice(0, ROW_CAP);
  const sel = selection && selection.path === file.path ? selection : null;
  const lo = sel ? Math.min(sel.from, sel.to) : -1;
  const hi = sel ? Math.max(sel.from, sel.to) : -1;

  if (rows.length === 0) return <div className="pr-nodiff">No changes to show.</div>;
  return (
    <div className="ch-diff pr-diff" role="table" aria-label={`Diff of ${file.path}`} data-testid="pr-diff">
      {shown.map((row, i) => {
        const placed = placeThread(row, threads);
        const target = rowTarget(row);
        const inBox = openBox && openBox.path === file.path && openBox.to === i;
        const selected = i >= lo && i <= hi;
        return (
          <div key={i}>
            <div className={`ch-line ${row.kind} pr-line${selected ? ' selected' : ''}`} role="row" data-row={i}>
              {row.kind === 'hunk' ? (
                <span className="ch-code">{row.text}</span>
              ) : (
                <>
                  <button
                    type="button"
                    className="ch-gutter pr-num"
                    aria-label={`Select line ${row.newNo ?? row.oldNo ?? ''}`}
                    disabled={!target}
                    onClick={(e) => onSelect({ path: file.path, from: i, to: i }, e.shiftKey, i)}
                  >
                    {row.oldNo ?? ''}
                  </button>
                  <button
                    type="button"
                    className="ch-gutter pr-num"
                    aria-label={`Select line ${row.newNo ?? row.oldNo ?? ''}`}
                    disabled={!target}
                    onClick={(e) => onSelect({ path: file.path, from: i, to: i }, e.shiftKey, i)}
                  >
                    {row.newNo ?? ''}
                  </button>
                  <span className="ch-sign">{SIGN[row.kind]}</span>
                  <span className="ch-code">{row.text}</span>
                  {target && (
                    <button
                      type="button"
                      className="pr-plus"
                      aria-label="Comment on this line"
                      title={commentOff ?? 'Comment on this line'}
                      disabled={commentOff !== null}
                      data-testid="pr-line-plus"
                      onClick={() => setOpenBox({ path: file.path, from: sel && sel.from !== sel.to && i >= lo && i <= hi ? lo : i, to: sel && sel.from !== sel.to && i >= lo && i <= hi ? hi : i })}
                    >
                      +
                    </button>
                  )}
                </>
              )}
            </div>
            {placed.map((t) => (
              <ThreadCard key={t.id} entry={entry} thread={t} inline />
            ))}
            {inBox && (
              <LineBox entry={entry} file={file} rows={rows} from={openBox.from} to={openBox.to} head={head} onClose={() => setOpenBox(null)} />
            )}
          </div>
        );
      })}
      {!all && rows.length > ROW_CAP && (
        <button type="button" className="pr-link-btn pr-more" onClick={() => setAll(true)}>
          Show all {rows.length.toLocaleString()} lines
        </button>
      )}
    </div>
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
