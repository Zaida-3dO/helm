// A unified diff as rows for the Changes view: two line-number gutters, a sign,
// hunk headers, and long runs of unchanged lines folded into gaps that can be expanded.

export type RowKind = 'hunk' | 'ctx' | 'add' | 'del' | 'meta';

export interface DiffRow {
  kind: RowKind;
  text: string;
  oldNo?: number;
  newNo?: number;
}

export interface Gap {
  kind: 'gap';
  id: number;
  rows: DiffRow[];
}

export type DisplayRow = DiffRow | Gap;

/** Unchanged lines kept on each side of a change. */
export const CONTEXT = 3;
export const EXPAND_STEP = 20;

/** Parses a git unified diff. File headers (`diff --git`, `---`, `+++`, `index`) are dropped. */
export function parseDiff(patch: string): DiffRow[] {
  const rows: DiffRow[] = [];
  let oldNo = 0;
  let newNo = 0;
  let inHunk = false;
  const lines = patch.replace(/\r\n/g, '\n').split('\n');
  if (lines[lines.length - 1] === '') lines.pop();
  for (const line of lines) {
    const h = /^@@ -(\d+)(?:,\d+)? \+(\d+)(?:,\d+)? @@(.*)$/.exec(line);
    if (h) {
      oldNo = Number(h[1]);
      newNo = Number(h[2]);
      inHunk = true;
      rows.push({ kind: 'hunk', text: line });
      continue;
    }
    if (!inHunk) continue;
    const c = line.charAt(0);
    if (c === '+') rows.push({ kind: 'add', text: line.slice(1), newNo: newNo++ });
    else if (c === '-') rows.push({ kind: 'del', text: line.slice(1), oldNo: oldNo++ });
    else if (c === ' ') rows.push({ kind: 'ctx', text: line.slice(1), oldNo: oldNo++, newNo: newNo++ });
    else if (c === '\\') rows.push({ kind: 'meta', text: line.slice(1).trim() });
    else if (line.startsWith('diff --git')) inHunk = false;
  }
  return rows;
}

export function counts(rows: DiffRow[]): { added: number; removed: number } {
  let added = 0;
  let removed = 0;
  for (const r of rows) {
    if (r.kind === 'add') added++;
    else if (r.kind === 'del') removed++;
  }
  return { added, removed };
}

/**
 * Folds every run of unchanged lines longer than the context into a gap, keeping `CONTEXT` lines
 * next to each change. A whole-file-context hunk header is dropped, since gaps say what is hidden.
 * `revealed` maps a gap id to how many of its lines (from the top) are shown.
 */
export function fold(rows: DiffRow[], revealed: ReadonlyMap<number, number> = new Map()): DisplayRow[] {
  const out: DisplayRow[] = [];
  let i = 0;
  let gapId = 0;
  while (i < rows.length) {
    const r = rows[i] as DiffRow;
    if (r.kind !== 'ctx') {
      out.push(r);
      i++;
      continue;
    }
    let j = i;
    while (j < rows.length && (rows[j] as DiffRow).kind === 'ctx') j++;
    const run = rows.slice(i, j);
    const atStart = i === 0 || (rows[i - 1] as DiffRow).kind === 'hunk';
    const atEnd = j === rows.length;
    const keepTop = atStart ? 0 : CONTEXT;
    const keepBottom = atEnd ? 0 : CONTEXT;
    if (run.length <= keepTop + keepBottom + 1) {
      out.push(...run);
    } else {
      out.push(...run.slice(0, keepTop));
      const hidden = run.slice(keepTop, run.length - keepBottom);
      const id = gapId++;
      const shown = Math.min(hidden.length, revealed.get(id) ?? 0);
      out.push(...hidden.slice(0, shown));
      if (shown < hidden.length) out.push({ kind: 'gap', id, rows: hidden.slice(shown) });
      out.push(...run.slice(run.length - keepBottom));
    }
    i = j;
  }
  return out;
}
