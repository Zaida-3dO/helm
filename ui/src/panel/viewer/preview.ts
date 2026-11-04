// Pure helpers behind the JSON, CSV/TSV, image and archive previews: parsing, searching, zoom steps
// and the archive folder tree.

import type { ArchiveEntry } from '../../api/panel.ts';

export type Parsed = { ok: true; value: unknown } | { ok: false };

export function parseJson(text: string): Parsed {
  try {
    return { ok: true, value: JSON.parse(text) as unknown };
  } catch {
    return { ok: false };
  }
}

const isBranch = (v: unknown): v is object => v !== null && typeof v === 'object';

/** Child entries of an object or array, keyed by property name or index. */
export function entriesOf(v: object): Array<[string, unknown]> {
  return Array.isArray(v) ? v.map((x, i) => [String(i), x]) : Object.entries(v);
}

/** True when `key` or any primitive value at or below `value` contains `q` (case-insensitive). */
export function jsonMatches(key: string, value: unknown, q: string): boolean {
  if (!q) return true;
  const needle = q.toLowerCase();
  if (key.toLowerCase().includes(needle)) return true;
  if (!isBranch(value)) return String(value).toLowerCase().includes(needle);
  return entriesOf(value).some(([k, v]) => jsonMatches(k, v, needle));
}

/** The delimiter a file's extension implies, or null for a file that is not delimited text. */
export function delimiterFor(path: string): ',' | '\t' | null {
  if (/\.csv$/i.test(path)) return ',';
  if (/\.tsv$/i.test(path)) return '\t';
  return null;
}

/** Delimited text as rows of cells: quoted cells may hold the delimiter, quotes ("") and newlines. */
export function parseDelimited(text: string, delimiter: string): string[][] {
  const rows: string[][] = [];
  let row: string[] = [];
  let cell = '';
  let quoted = false;
  for (let i = 0; i < text.length; i++) {
    const c = text.charAt(i);
    if (quoted) {
      if (c === '"') {
        if (text.charAt(i + 1) === '"') {
          cell += '"';
          i++;
        } else quoted = false;
      } else cell += c;
    } else if (c === '"' && cell === '') quoted = true;
    else if (c === delimiter) {
      row.push(cell);
      cell = '';
    } else if (c === '\n' || c === '\r') {
      if (c === '\r' && text.charAt(i + 1) === '\n') i++;
      row.push(cell);
      rows.push(row);
      row = [];
      cell = '';
    } else cell += c;
  }
  if (cell !== '' || row.length > 0) {
    row.push(cell);
    rows.push(row);
  }
  return rows;
}

/** The rows with a cell containing `q`, keeping each row's index in the original. */
export function filterRows(rows: string[][], q: string): Array<{ index: number; cells: string[] }> {
  const needle = q.trim().toLowerCase();
  const all = rows.map((cells, index) => ({ index, cells }));
  return needle ? all.filter((r) => r.cells.some((c) => c.toLowerCase().includes(needle))) : all;
}

export const ZOOM_MIN = 0.1;
export const ZOOM_MAX = 8;

/** The next zoom level one step in or out, kept within the allowed range. */
export function stepZoom(current: number, dir: 1 | -1): number {
  const next = dir === 1 ? current * 1.25 : current / 1.25;
  return Math.round(Math.min(ZOOM_MAX, Math.max(ZOOM_MIN, next)) * 1000) / 1000;
}

export interface ArchiveRow {
  /** The row's full path inside the archive, unique per row. */
  key: string;
  name: string;
  depth: number;
  dir: boolean;
  size?: number;
  compressedSize?: number;
  encrypted: boolean;
}

interface ArchiveNode {
  name: string;
  dir: boolean;
  size?: number;
  compressedSize?: number;
  encrypted: boolean;
  children: Map<string, ArchiveNode>;
}

/**
 * An archive's entries as indented rows, folders before files at each level and each group sorted
 * by name. Folders the archive only implies (a file in `a/b/` with no `a/` entry) get a row too.
 */
export function archiveRows(entries: ArchiveEntry[]): ArchiveRow[] {
  const root: ArchiveNode = { name: '', dir: true, encrypted: false, children: new Map() };
  for (const e of entries) {
    const parts = e.path.split('/').filter((p) => p !== '' && p !== '.');
    let node = root;
    parts.forEach((part, i) => {
      const last = i === parts.length - 1;
      let child = node.children.get(part);
      if (!child) {
        child = { name: part, dir: !last || e.dir, encrypted: false, children: new Map() };
        node.children.set(part, child);
      }
      if (last) {
        child.dir = child.dir || e.dir;
        child.size = e.size;
        child.compressedSize = e.compressedSize;
        child.encrypted = e.encrypted;
      }
      node = child;
    });
  }
  const rows: ArchiveRow[] = [];
  const walk = (node: ArchiveNode, depth: number, prefix: string) => {
    const kids = [...node.children.values()].sort((a, b) =>
      a.dir === b.dir ? a.name.toLowerCase().localeCompare(b.name.toLowerCase()) : a.dir ? -1 : 1,
    );
    for (const k of kids) {
      const key = `${prefix}${k.name}${k.dir ? '/' : ''}`;
      rows.push({ key, name: k.name, depth, dir: k.dir, size: k.size, compressedSize: k.compressedSize, encrypted: k.encrypted });
      walk(k, depth + 1, key);
    }
  };
  walk(root, 0, '');
  return rows;
}

/** A byte count in B, KiB, MiB or GiB. */
export function sizeLabel(n: number): string {
  if (n < 1024) return `${n} B`;
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KiB`;
  if (n < 1024 * 1024 * 1024) return `${(n / (1024 * 1024)).toFixed(1)} MiB`;
  return `${(n / (1024 * 1024 * 1024)).toFixed(1)} GiB`;
}
