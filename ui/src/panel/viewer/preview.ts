// Pure helpers behind the JSON, CSV/TSV and image previews: parsing, searching and zoom steps.

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
