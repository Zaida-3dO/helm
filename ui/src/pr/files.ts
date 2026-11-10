// Files changed: the pure parts. Test-file detection, sort presets, directory groups, filters and the
// "without tests" tally, plus the split-view pairing, diff windowing and whole-file text helpers.
// No DOM; the only host call is `loadWholeFile`, which takes the api it reads through.

import type { Api } from '../api/client.ts';
import type { DiffRow, RowKind } from '../panel/changes/diffModel.ts';
import type { Answer } from './client.ts';
import type { FileStatus, PrFile, PrRef, ViewedState } from './types.ts';

const TEST_DIR = /(^|\/)(tests?|__tests__|mocks?|__mocks__|snapshots?|__snapshots__|specs?|e2e|fixtures?|testdata|test-data)\//i;
const TEST_NAME = /(\.(test|spec)\.[^/]+$)|(_test\.[^/]+$)|(\.snap$)|(Tests?\.[a-z]+$)|(IT\.[a-z]+$)|(src\/test\/)/;

/** Under a test-like folder, or named like a test. A rename counts by its new name. */
export function isTestFile(path: string): boolean {
  return TEST_DIR.test(path) || TEST_NAME.test(path);
}

const LOCKFILES = /(^|\/)(package-lock\.json|yarn\.lock|pnpm-lock\.yaml|Cargo\.lock|poetry\.lock|Gemfile\.lock|composer\.lock|go\.sum|bun\.lockb?)$/;

export const isLockfile = (path: string): boolean => LOCKFILES.test(path);

export type SortPreset = 'pr' | 'logic' | 'tests-first' | 'tests-last' | 'largest' | 'smallest';

export const SORT_LABEL: Record<SortPreset, string> = {
  pr: 'PR order',
  logic: 'Main logic first',
  'tests-first': 'Tests first',
  'tests-last': 'Tests last',
  largest: 'Largest first',
  smallest: 'Smallest first',
};

const lines = (f: PrFile) => f.additions + f.deletions;

/** Types, utilities, services, hooks, components, pages, then tests. A reading order by role, from the path alone. */
function role(path: string): number {
  if (isTestFile(path)) return 7;
  if (/(^|\/)types?(\/|\.)|\.d\.ts$|types\.[a-z]+$/i.test(path)) return 0;
  if (/(^|\/)(utils?|helpers?|lib)(\/|\.)/i.test(path)) return 1;
  if (/(service|api|client|store|model)s?(\/|\.)/i.test(path)) return 2;
  if (/(^|\/)hooks?\/|(^|\/)use[A-Z][^/]*\.[a-z]+$/.test(path)) return 3;
  if (/(^|\/)(pages?|views?|screens?|routes?)(\/|\.)/i.test(path)) return 5;
  if (/component|\.tsx$|\.jsx$|\.vue$/i.test(path)) return 4;
  return 2;
}

/** Every preset starts from GitHub's order. Lockfiles go last in all of them. */
export function sortFiles(files: readonly PrFile[], preset: SortPreset): PrFile[] {
  const keyed = files.map((f, i) => ({ f, i }));
  const key = (f: PrFile): number => {
    switch (preset) {
      case 'logic':
        return role(f.path);
      case 'tests-first':
        return isTestFile(f.path) ? 0 : 1;
      case 'tests-last':
        return isTestFile(f.path) ? 1 : 0;
      case 'largest':
        return -lines(f);
      case 'smallest':
        return lines(f);
      default:
        return 0;
    }
  };
  return keyed
    .sort((a, b) => Number(isLockfile(a.f.path)) - Number(isLockfile(b.f.path)) || key(a.f) - key(b.f) || a.i - b.i)
    .map((x) => x.f);
}

export const dirOf = (path: string): string => {
  const i = path.lastIndexOf('/');
  return i < 0 ? '' : path.slice(0, i);
};

/**
 * Directory groups. In PR order a directory is one group, ordered by first appearance; with a sort
 * preset consecutive runs of one directory group together, so a directory can repeat.
 */
export function groupByDir(files: readonly PrFile[], preset: SortPreset): { dir: string; files: PrFile[] }[] {
  const out: { dir: string; files: PrFile[] }[] = [];
  for (const f of files) {
    const dir = dirOf(f.path);
    const hit = preset === 'pr' ? out.find((g) => g.dir === dir) : out[out.length - 1]?.dir === dir ? out[out.length - 1] : undefined;
    if (hit) hit.files.push(f);
    else out.push({ dir, files: [f] });
  }
  return out;
}

export interface FileFilter {
  text: string;
  types: ReadonlySet<FileStatus>;
  unviewedOnly: boolean;
}

export const NO_FILTER: FileFilter = { text: '', types: new Set(), unviewedOnly: false };

/** Filters intersect. Text matches the path, case-blind, and a rename's old name. An empty type set means all. */
export function filterFiles(files: readonly PrFile[], f: FileFilter, viewed: ReadonlyMap<string, ViewedState>): PrFile[] {
  const q = f.text.trim().toLowerCase();
  return files.filter((file) => {
    if (q && !file.path.toLowerCase().includes(q) && !(file.previousPath ?? '').toLowerCase().includes(q)) return false;
    if (f.types.size > 0 && !f.types.has(file.status)) return false;
    if (f.unviewedOnly && viewed.get(file.path) === 'VIEWED') return false;
    return true;
  });
}

/** Changed lines of the files that are not tests; null when there are no test files at all. */
export function withoutTests(files: readonly PrFile[]): { added: number; removed: number; testFiles: number; testLines: number } | null {
  let added = 0;
  let removed = 0;
  let testFiles = 0;
  let testLines = 0;
  for (const f of files) {
    if (isTestFile(f.path)) {
      testFiles++;
      testLines += lines(f);
    } else {
      added += f.additions;
      removed += f.deletions;
    }
  }
  return testFiles === 0 ? null : { added, removed, testFiles, testLines };
}

export const STATUS_LABEL: Record<FileStatus, string> = { added: 'Added', modified: 'Modified', removed: 'Removed', renamed: 'Renamed' };

/** Five small blocks proportioned between added, deleted and neutral: how many of each colour. */
export function statBlocks(added: number, removed: number): ('add' | 'del' | 'neutral')[] {
  const total = added + removed;
  if (total === 0) return Array(5).fill('neutral') as 'neutral'[];
  const a = Math.round((added / total) * 5);
  const d = Math.round((removed / total) * 5);
  const out: ('add' | 'del' | 'neutral')[] = [];
  for (let i = 0; i < 5; i++) out.push(i < Math.min(a, 5) ? 'add' : i < Math.min(a + d, 5) ? 'del' : 'neutral');
  return out;
}

// ---- split view, windowing and whole-file text ----

export type DiffMode = 'unified' | 'split';

/** A diff row with its place in the patch's own rows (-1 for a line that only fills in unchanged text). */
export interface SrcRow extends DiffRow {
  src: number;
}

export const withSrc = (rows: readonly DiffRow[]): SrcRow[] => rows.map((r, i) => ({ ...r, src: i }));

export type SplitItem<T> = { kind: 'full'; row: T } | { kind: 'pair'; left: T | null; right: T | null };

/** Side by side: a run of deletions sits beside the run of additions that follows it, line for line. Unchanged lines show on both sides; hunk headers and notes span the width. */
export function pairSplit<T extends { kind: RowKind }>(rows: readonly T[]): SplitItem<T>[] {
  const out: SplitItem<T>[] = [];
  let i = 0;
  while (i < rows.length) {
    const row = rows[i] as T;
    if (row.kind === 'ctx') {
      out.push({ kind: 'pair', left: row, right: row });
      i++;
    } else if (row.kind === 'del' || row.kind === 'add') {
      const dels: T[] = [];
      const adds: T[] = [];
      while (i < rows.length && (rows[i] as T).kind === 'del') dels.push(rows[i++] as T);
      while (i < rows.length && (rows[i] as T).kind === 'add') adds.push(rows[i++] as T);
      for (let k = 0; k < Math.max(dels.length, adds.length); k++) out.push({ kind: 'pair', left: dels[k] ?? null, right: adds[k] ?? null });
    } else {
      out.push({ kind: 'full', row });
      i++;
    }
  }
  return out;
}

/**
 * Windowing over rows of known heights: the slice to draw for a view `height` tall starting `top` down
 * the list, with `overscan` extra on both sides, and the space the undrawn rows above and below take.
 */
export function visibleRange(heights: readonly number[], top: number, height: number, overscan: number): { start: number; end: number; before: number; after: number } {
  const n = heights.length;
  let y = 0;
  let start = 0;
  while (start < n && y + (heights[start] as number) <= top - overscan) y += heights[start++] as number;
  const before = y;
  let end = start;
  while (end < n && y < top + height + overscan) y += heights[end++] as number;
  let after = 0;
  for (let i = end; i < n; i++) after += heights[i] as number;
  return { start, end, before, after };
}

/**
 * The whole new file as diff rows: the patch's own rows in place, and the unchanged lines between and
 * around its hunks filled in from the file's text (old line numbers follow from the hunks). Hunk headers
 * are left out. A patch without numbered hunks is returned as it is.
 */
export function expandWhole(rows: readonly DiffRow[], lines: readonly string[]): SrcRow[] {
  const out: SrcRow[] = [];
  let nextNew = 1;
  let nextOld = 1;
  const fill = (upTo: number) => {
    for (; nextNew <= upTo && nextNew <= lines.length; nextNew++, nextOld++) {
      out.push({ kind: 'ctx', text: lines[nextNew - 1] ?? '', oldNo: nextOld, newNo: nextNew, src: -1 });
    }
  };
  for (let i = 0; i < rows.length; i++) {
    const row = rows[i] as DiffRow;
    if (row.kind === 'hunk') {
      const h = /^@@ -\d+(?:,\d+)? \+(\d+)(?:,(\d+))? @@/.exec(row.text);
      if (!h) return withSrc(rows);
      const count = h[2] === undefined ? 1 : Number(h[2]);
      fill(count === 0 ? Number(h[1]) : Number(h[1]) - 1);
      continue;
    }
    out.push({ ...row, src: i });
    if (row.newNo !== undefined) nextNew = row.newNo + 1;
    if (row.oldNo !== undefined) nextOld = row.oldNo + 1;
  }
  fill(lines.length);
  return out;
}

/** Text of a file as lines: CRLF folded, and no empty last line for a closing newline. */
export function textLines(text: string): string[] {
  const lines = text.replace(/\r\n/g, '\n').split('\n');
  if (lines[lines.length - 1] === '') lines.pop();
  return lines;
}

/**
 * One file's text at a commit, through the read-only contents route. Rejects with a plain sentence
 * when the file is too large for that route, is not a file, or the read fails.
 */
export async function loadWholeFile(api: Api, ref: PrRef, path: string, sha: string): Promise<string[]> {
  const enc = path.split('/').map(encodeURIComponent).join('/');
  const a = await api<Answer<{ content?: unknown; encoding?: unknown; type?: unknown }>>('github_rest', {
    path: `/repos/${ref.owner}/${ref.repo}/contents/${enc}`,
    query: { ref: sha },
  });
  const d = a.data;
  if (!d || Array.isArray(d) || (d.type !== undefined && d.type !== 'file')) throw new Error('That path is not a file at this commit.');
  if (d.encoding !== 'base64' || typeof d.content !== 'string') throw new Error('This file is too large to load here.');
  const bin = atob(d.content.replace(/\s/g, ''));
  const bytes = Uint8Array.from(bin, (c) => c.charCodeAt(0));
  return textLines(new TextDecoder('utf-8').decode(bytes));
}
