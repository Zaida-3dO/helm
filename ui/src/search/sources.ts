// What a search source is, and the rules that order the groups. Only the Chats and Files groups are live. Pull requests may come later and
// Board comes in through the board client's attach-only read, so both are listed here only so the
// group order and the controller already cope with them.

export type SourceId = 'chats' | 'files' | 'prs' | 'board';

/** The most rows a group may show. */
export const GROUP_LIMIT: Record<SourceId, number> = { chats: 90, files: 20, prs: 10, board: 10 };

export const GROUP_LABEL: Record<SourceId, string> = {
  chats: 'Chats',
  files: 'Files',
  prs: 'Pull requests',
  board: 'Board',
};

/** One result line. `open` does the work of choosing it. */
export interface Row {
  /** Unique across the dialog and free of whitespace: `<source>:<key>`. */
  id: string;
  title: string;
  subtitle?: string;
  snippet?: string;
  /** A small label, for example "title" on a chat that matched by its title only. */
  tag?: string;
  open: () => void;
}

export interface SearchSource {
  id: SourceId;
  /** Fewer typed characters than this and the source stays idle. */
  minChars: number;
  /** A source that has not answered by now is settled as failed, so it can never hang the search. */
  timeoutMs: number;
  /** Must honour `signal`; the controller also drops answers it no longer wants. */
  run: (query: string, signal: AbortSignal) => Promise<Row[]>;
}

/** `<source>:<key>` with any whitespace in the key replaced, so ids are safe as DOM ids. */
export function rowId(source: SourceId, key: string): string {
  return `${source}:${key.replace(/\s+/g, '_')}`;
}

/**
 * Whether the query reads like a file path: it holds a slash or backslash, starts with `~`, `./`
 * or a drive letter and colon, or is a single `name.ext` word. Such a query puts Files first.
 */
export function looksLikePath(query: string): boolean {
  const q = query.trim();
  if (q === '') return false;
  if (/[\\/]/.test(q)) return true;
  if (q.startsWith('~') || /^[A-Za-z]:/.test(q)) return true;
  return /^[^\s\\/]+\.[A-Za-z0-9]{1,8}$/.test(q);
}

const BASE_ORDER: SourceId[] = ['chats', 'files', 'prs', 'board'];

/** Chats, Files, Pull requests, Board; Files leads when the query looks like a path. */
export function groupOrder(query: string, present: readonly SourceId[]): SourceId[] {
  const order = looksLikePath(query) ? (['files', 'chats', 'prs', 'board'] as SourceId[]) : BASE_ORDER;
  return order.filter((id) => present.includes(id));
}

/** A path from a file source, made absolute against the chat's folder for the link gate. */
export function absolutePath(cwd: string, path: string): string {
  const p = path.replace(/\\/g, '/');
  if (/^([A-Za-z]:\/|\/|~)/.test(p)) return p;
  return `${cwd.replace(/[\\/]+$/, '').replace(/\\/g, '/')}/${p}`;
}

export function baseName(path: string): string {
  const parts = path.replace(/\\/g, '/').split('/').filter(Boolean);
  return parts.at(-1) ?? path;
}

const segments = (path: string) => path.replace(/\\/g, '/').split('/').filter(Boolean);

/**
 * A short form of a file's parent folder for the second line of a row: relative to the chat's folder
 * when it is inside it, otherwise the last three folders behind an ellipsis.
 */
export function shortFolder(dir: string, cwd: string | null): string {
  const d = segments(dir);
  const c = cwd ? segments(cwd) : [];
  if (c.length > 0 && d.length >= c.length && c.every((s, i) => s.toLowerCase() === d[i]!.toLowerCase())) return d.slice(c.length).join('/');
  return d.length > 3 ? `…/${d.slice(-3).join('/')}` : d.join('/');
}

export function dirName(path: string): string {
  const parts = path.replace(/\\/g, '/').split('/').filter(Boolean);
  return parts.slice(0, -1).join('/');
}
