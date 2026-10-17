// Clickable paths in replies: which inline code
// and which words of plain text are file paths an agent wrote. Rendering only marks them; nothing
// here touches the filesystem. A click goes to the panel's link gate, which resolves the path on
// the host (home folder, the chat's project folder) and explains a missing one.
//
// A drive letter's colon is part of the path, never a line separator: only a trailing `:N`,
// `:N:M`, `:N-M`, `#LN` or `#LN-LM` is a line suffix.

const EXTENSIONS = new Set(
  (
    'ts tsx mts cts js jsx mjs cjs json jsonc md markdown mdx rs py rb go java kt cs c h cc cpp hpp cxx ' +
    'css scss sass less html htm xml svg toml yaml yml ini cfg conf env txt log csv tsv sh bash zsh ps1 ' +
    'psm1 bat cmd sql lock gradle swift lua php pl r dart vue svelte astro png jpg jpeg gif webp ico pdf ' +
    'zip tar gz dockerfile makefile gitignore editorconfig'
  ).split(' '),
);

/** Characters that never appear in a Windows path (besides the drive colon). */
const INVALID = /[<>"|?*]/;

export interface PathSuffix {
  path: string;
  line?: number;
  endLine?: number;
  column?: number;
}

/** Splits a trailing line suffix off a written path. The drive colon is never taken. */
export function splitPathSuffix(raw: string): PathSuffix {
  let m = /#L(\d+)(?:-L?(\d+))?$/.exec(raw);
  if (m) {
    const out: PathSuffix = { path: raw.slice(0, m.index), line: Number(m[1]) };
    if (m[2]) out.endLine = Number(m[2]);
    return valid(out) ? out : { path: raw };
  }
  m = /:(\d+)(?:-(\d+)|:(\d+))?$/.exec(raw);
  if (m) {
    const out: PathSuffix = { path: raw.slice(0, m.index), line: Number(m[1]) };
    if (m[2]) out.endLine = Number(m[2]);
    if (m[3]) out.column = Number(m[3]);
    // "C:" alone is a drive, not a path with a line.
    if (/^[a-z]$/i.test(out.path)) return { path: raw };
    return valid(out) ? out : { path: raw };
  }
  return { path: raw };
}

function valid(s: PathSuffix): boolean {
  if (!s.path || !(s.line! >= 1)) return false;
  if (s.endLine !== undefined && !(s.endLine >= s.line!)) return false;
  if (s.column !== undefined && !(s.column >= 1)) return false;
  return true;
}

function hasExtension(name: string): boolean {
  const lower = name.toLowerCase();
  if (EXTENSIONS.has(lower)) return true; // Dockerfile, Makefile
  const dot = lower.lastIndexOf('.');
  if (dot <= 0 && !lower.startsWith('.')) return false;
  return EXTENSIONS.has(lower.slice(dot + 1));
}

const segmentsOf = (p: string) => p.split(/[\\/]/).filter(Boolean);
const allNumeric = (segs: string[]) => segs.every((s) => /^[\d.,-]+$/.test(s));

function isUrlish(s: string): boolean {
  // Schemes, `www.`, and the cross-repository form `repo@ref:path` (left to its own feature).
  return /^[a-z][a-z0-9+.-]*:\/\//i.test(s) || /^www\./i.test(s) || /@[^/\\]*:/.test(s) || /^[a-z][a-z0-9+.-]+:(?![\\/\d])/i.test(s);
}

/** The path written in inline code, if the whole token looks like one. */
export function pathInCode(text: string): PathSuffix | null {
  const t = text.trim();
  if (!t || t.length > 1024 || /[\r\n\t]/.test(t) || isUrlish(t)) return null;
  const s = splitPathSuffix(t);
  const p = s.path;
  if (INVALID.test(p)) return null;
  const drive = /^[a-z]:[\\/]/i.test(p);
  // Spaces only in an unmistakable absolute path (`C:\Program Files\…`, `~/My Notes/…`).
  if (/\s/.test(p) && !drive && !/^~[\\/]/.test(p)) return null;
  const segs = segmentsOf(p);
  if (segs.length === 0 || allNumeric(segs)) return null;
  if (drive || /^~([\\/]|$)/.test(p) || /^\.\.?[\\/]/.test(p)) return p.length > 3 || /^~/.test(p) ? s : null;
  if (/^[\\/]/.test(p)) return segs.length >= 2 ? s : null;
  if (/[\\/]/.test(p)) return /[a-z]/i.test(p) ? s : null;
  // A bare `name.ext` with a recognised extension, but never a version or a number.
  if (hasExtension(p) && !/^[\d.]+$/.test(p) && /^[\w.@+-]+$/.test(p)) return s;
  return null;
}

/** The path a word of plain text is, under the stricter prose rules. */
export function pathInProse(word: string): PathSuffix | null {
  if (!word || word.length > 1024 || isUrlish(word)) return null;
  const s = splitPathSuffix(word);
  const p = s.path;
  if (INVALID.test(p) || /\s/.test(p)) return null;
  const segs = segmentsOf(p);
  if (segs.length === 0 || allNumeric(segs)) return null;
  if (/^~[\\/]./.test(p)) return s;
  if (/^\.\.?[\\/]./.test(p)) return s;
  // Absolute paths of two or more segments: `C:\a\b`, `/c/a/b`.
  if (/^[a-z]:[\\/]/i.test(p)) return segs.length >= 3 ? s : null;
  if (/^[\\/]/.test(p)) return segs.length >= 2 && /[a-z]/i.test(p) ? s : null;
  // `seg/…/name.ext`: at least two segments, ending in a recognised extension.
  if (segs.length >= 2 && hasExtension(segs[segs.length - 1] as string) && /^[\w@.+-]/.test(p)) return s;
  return null;
}

/** Characters a prose path may contain (the run the tokenizer reads). */
export const PROSE_PATH_RUN = /^[^\s<>"|?*()[\]{}`'!,;]+/;

/** Trailing punctuation that ends a sentence rather than the path. */
export function trimProseRun(run: string): string {
  return run.replace(/[.:]+$/, '');
}
