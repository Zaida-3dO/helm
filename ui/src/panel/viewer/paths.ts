// Windows path helpers for the viewer: pure string work, no filesystem.

/** Backslashes, an uppercase drive letter, no trailing separator except on a drive root. */
export function displayPath(p: string): string {
  let s = p.replace(/\//g, '\\');
  if (/^[a-z]:/.test(s)) s = s.charAt(0).toUpperCase() + s.slice(1);
  if (s.length > 3) s = s.replace(/\\+$/, '');
  return s;
}

/** The tab key for a path: Windows paths compare case-insensitively. */
export function pathKey(p: string): string {
  return displayPath(p).toLowerCase();
}

export function dirname(p: string): string | null {
  const s = displayPath(p);
  if (/^[A-Za-z]:\\?$/.test(s)) return null;
  const at = s.lastIndexOf('\\');
  if (at < 0) return null;
  if (at === 2 && s.charAt(1) === ':') return s.slice(0, 3);
  return s.slice(0, at);
}

export function basename(p: string): string {
  const s = displayPath(p);
  const at = s.lastIndexOf('\\');
  return at < 0 ? s : s.slice(at + 1) || s;
}

/** Joins a relative path onto a folder, resolving `.` and `..` lexically (never above the drive). */
export function joinPath(base: string, rel: string): string {
  const parts = displayPath(base).split('\\');
  const drive = parts.shift() as string;
  for (const seg of rel.replace(/\//g, '\\').split('\\')) {
    if (!seg || seg === '.') continue;
    if (seg === '..') parts.pop();
    else parts.push(seg);
  }
  const tail = parts.filter(Boolean).join('\\');
  return tail ? `${drive}\\${tail}` : `${drive}\\`;
}

/** Breadcrumb segments from the drive down, each with the path it opens. */
export function splitSegments(p: string): Array<{ name: string; path: string }> {
  const s = displayPath(p);
  const parts = s.split('\\').filter(Boolean);
  const out: Array<{ name: string; path: string }> = [];
  let acc = '';
  parts.forEach((part, i) => {
    acc = i === 0 ? `${part}\\` : acc.endsWith('\\') ? `${acc}${part}` : `${acc}\\${part}`;
    out.push({ name: part, path: acc });
  });
  return out;
}

export interface Segment {
  text: string;
  hit: boolean;
}

/**
 * Splits `text` into runs, marking the characters a query matched. The query is matched as one
 * contiguous run when it occurs that way, otherwise as an in-order subsequence of letters (the way
 * a quick-open query like "cmpsr" finds "Composer"). Case never matters. No match: one plain run.
 */
export function matchSegments(text: string, query: string): Segment[] {
  const q = query.trim().toLowerCase();
  const plain = [{ text, hit: false }];
  if (!q || !text) return plain;
  const lower = text.toLowerCase();
  const marked = new Array<boolean>(text.length).fill(false);
  const at = lower.indexOf(q);
  if (at >= 0) {
    for (let i = at; i < at + q.length; i++) marked[i] = true;
  } else {
    let from = 0;
    for (const ch of q) {
      const found = lower.indexOf(ch, from);
      if (found < 0) return plain;
      marked[found] = true;
      from = found + 1;
    }
  }
  const out: Segment[] = [];
  for (let i = 0; i < text.length; i++) {
    const last = out[out.length - 1];
    if (last && last.hit === marked[i]) last.text += text.charAt(i);
    else out.push({ text: text.charAt(i), hit: marked[i] as boolean });
  }
  return out;
}
