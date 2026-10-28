// Files changed: the pure parts. Test-file detection, sort presets, directory groups, filters and the
// "without tests" tally. No DOM, no host calls.

import type { FileStatus, PrFile, ViewedState } from './types.ts';

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
