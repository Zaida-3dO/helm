// A structured tool result as a key/value grid, for host and MCP tool rows.
// Pure: parses the row's output text into a small model that ToolCard renders.

export type Cell =
  | { t: 'bool'; v: boolean }
  | { t: 'color'; v: string }
  | { t: 'time'; v: string }
  | { t: 'empty'; v: string }
  | { t: 'text'; v: string }
  | { t: 'chips'; v: string[] }
  | { t: 'lines'; v: { name: string; note: string }[]; more: number }
  | { t: 'group'; v: GridRow[] };

export interface GridRow {
  key: string;
  cell: Cell;
}

const MAX_DEPTH = 2;
const MAX_LINES = 40;

/** "createdAt" and "chat_id" to "Created at" and "Chat id". */
export function humanKey(k: string): string {
  const s = k
    .replace(/([a-z0-9])([A-Z])/g, '$1 $2')
    .replace(/[_-]+/g, ' ')
    .trim()
    .toLowerCase();
  return s ? s[0]!.toUpperCase() + s.slice(1) : k;
}

const HEX = /^#(?:[0-9a-f]{3}|[0-9a-f]{6}|[0-9a-f]{8})$/i;
const ISO = /^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}/;

function scalarText(v: unknown): string {
  return typeof v === 'string' ? v : String(v);
}

function cellFor(v: unknown, depth: number): Cell {
  if (v === null || v === undefined) return { t: 'empty', v: 'None' };
  if (typeof v === 'boolean') return { t: 'bool', v };
  if (typeof v === 'string') {
    if (v === '') return { t: 'empty', v: 'Empty' };
    if (HEX.test(v)) return { t: 'color', v };
    if (ISO.test(v)) {
      const d = new Date(v);
      if (!Number.isNaN(d.getTime())) return { t: 'time', v: d.toLocaleString() };
    }
    return { t: 'text', v };
  }
  if (typeof v === 'number' || typeof v === 'bigint') return { t: 'text', v: String(v) };
  if (Array.isArray(v)) {
    if (v.length === 0) return { t: 'empty', v: 'None' };
    if (v.every((x) => x === null || typeof x !== 'object')) return { t: 'chips', v: v.map(scalarText) };
    const lines = v.slice(0, MAX_LINES).map((x) => {
      if (x && typeof x === 'object' && !Array.isArray(x)) {
        const o = x as Record<string, unknown>;
        const name = o.name ?? o.title ?? o.id ?? o.label;
        const note = o.note ?? o.description ?? o.status ?? o.summary ?? '';
        return { name: name == null ? JSON.stringify(x) : scalarText(name), note: typeof note === 'object' ? '' : scalarText(note) };
      }
      return { name: JSON.stringify(x), note: '' };
    });
    return { t: 'lines', v: lines, more: Math.max(0, v.length - MAX_LINES) };
  }
  if (typeof v === 'object') {
    const entries = Object.entries(v as Record<string, unknown>);
    if (entries.length === 0) return { t: 'empty', v: 'Empty' };
    // Past the nesting limit the object is shown as its JSON text.
    if (depth >= MAX_DEPTH) return { t: 'text', v: JSON.stringify(v) };
    return { t: 'group', v: entries.map(([k, x]) => ({ key: humanKey(k), cell: cellFor(x, depth + 1) })) };
  }
  return { t: 'text', v: String(v) };
}

/** The grid for a JSON-object result, or null when the text is not one (shown as plain output). */
export function parseResult(text: string): GridRow[] | null {
  const t = text.trim();
  if (!t.startsWith('{')) return null;
  let v: unknown;
  try {
    v = JSON.parse(t);
  } catch {
    return null;
  }
  if (!v || typeof v !== 'object' || Array.isArray(v)) return null;
  const entries = Object.entries(v as Record<string, unknown>);
  if (entries.length === 0) return null;
  return entries.map(([k, x]) => ({ key: humanKey(k), cell: cellFor(x, 0) }));
}
