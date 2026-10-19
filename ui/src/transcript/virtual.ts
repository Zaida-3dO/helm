// Geometry for the virtualised transcript: measured row heights are cached by
// key, unmeasured rows use an estimate, and only the rows in (or near) the viewport are mounted.
// Pure, so the window and anchor maths are tested without a DOM.
//
// Heights measured in one visit are kept in a shared cache (HeightCache) so that switching back to a
// chat starts from real heights rather than estimates. Item keys are unique across chats (row ids
// are global; a fold's key carries its turn's source key), so one cache serves every chat. A
// height is only reused at the width it was measured at.

export const ESTIMATE = 80;
export const OVERSCAN_PX = 600;

/** Measured heights by key and width, least recently used first. Bounded. */
export class HeightCache {
  private readonly map = new Map<string, { h: number; w: number }>();
  private readonly limit: number;

  constructor(limit = 5000) {
    this.limit = limit;
  }

  get(key: string, width: number): number | undefined {
    const e = this.map.get(key);
    return e && e.w === width ? e.h : undefined;
  }

  set(key: string, width: number, h: number): void {
    this.map.delete(key);
    this.map.set(key, { h, w: width });
    if (this.map.size > this.limit) {
      const oldest = this.map.keys().next().value;
      if (oldest !== undefined) this.map.delete(oldest);
    }
  }

  get size(): number {
    return this.map.size;
  }
}

/** The app's one height cache, shared by every transcript. */
export const sharedHeights = new HeightCache();

export class Geometry {
  private readonly heights = new Map<string, number>();
  private readonly cache: HeightCache | null;
  /** The list's width, which a cached height must match. 0 until it is known. */
  width = 0;
  /** A guess for a row never measured at this width. */
  estimate: (key: string) => number = () => ESTIMATE;

  constructor(cache: HeightCache | null = null) {
    this.cache = cache;
  }

  /** Record a measured height; returns true when it changed. */
  measure(key: string, h: number): boolean {
    if (this.width > 0) this.cache?.set(key, this.width, h);
    if (this.heights.get(key) === h) return false;
    this.heights.set(key, h);
    return true;
  }

  height(key: string): number {
    return this.heights.get(key) ?? (this.width > 0 ? this.cache?.get(key, this.width) : undefined) ?? this.estimate(key);
  }

  /** Top offset of every key, plus the total height as the last entry. */
  offsets(keys: readonly string[]): number[] {
    const out = new Array<number>(keys.length + 1);
    let y = 0;
    for (let i = 0; i < keys.length; i++) {
      out[i] = y;
      y += this.height(keys[i]!);
    }
    out[keys.length] = y;
    return out;
  }

  /** The mounted window [start, end) for a scroll position. */
  window(offsets: readonly number[], scrollTop: number, viewport: number, overscan = OVERSCAN_PX) {
    const n = offsets.length - 1;
    const lo = scrollTop - overscan;
    const hi = scrollTop + viewport + overscan;
    let start = 0;
    while (start < n && offsets[start + 1]! <= lo) start++;
    let end = start;
    while (end < n && offsets[end]! < hi) end++;
    return { start, end };
  }
}

/** What a row is, for its height estimate. */
export interface EstimateInput {
  kind: 'user' | 'assistant' | 'toolResult' | 'context' | 'fold';
  /** Characters of visible text. */
  chars: number;
  /** Line breaks in the visible text. */
  lines: number;
}

// Calibrated against the default theme at a 820 px reading column (about 105 characters a line at
// 22 px line height); close enough that scrolling up into unmeasured rows moves the scrollbar a
// little rather than a lot.
const CHARS_PER_LINE = 105;
const LINE_PX = 22;

/** A height guess for a row that has never been measured. */
export function estimateHeight(e: EstimateInput): number {
  switch (e.kind) {
    case 'fold':
      return 45;
    case 'context':
      return 36;
    case 'toolResult':
      // Tool cards start collapsed to their header.
      return 52;
    case 'user': {
      const lines = Math.max(1, e.lines + 1, Math.ceil(e.chars / (CHARS_PER_LINE * 0.75)));
      // The bubble's padding and the gap below it.
      return 83 + Math.min(lines, 40) * LINE_PX;
    }
    case 'assistant': {
      const lines = Math.max(1, Math.ceil(e.chars / CHARS_PER_LINE) + Math.ceil(e.lines / 2));
      return 40 + lines * LINE_PX;
    }
  }
}

/** One row in view and how far its top sits from the scroll top. */
export interface Anchor {
  key: string;
  delta: number;
}

/** The first key whose row is at least partly visible, and how far its top sits from the scroll top. */
export function anchorAt(keys: readonly string[], offsets: readonly number[], scrollTop: number): Anchor | null {
  for (let i = 0; i < keys.length; i++) {
    if (offsets[i + 1]! > scrollTop) return { key: keys[i]!, delta: offsets[i]! - scrollTop };
  }
  return null;
}

/** The scroll top that puts `anchor` back where it was, after rows above it changed. */
export function restoreAnchor(keys: readonly string[], offsets: readonly number[], anchor: Anchor): number | null {
  const i = keys.indexOf(anchor.key);
  if (i < 0) return null;
  return offsets[i]! - anchor.delta;
}

/** As restoreAnchor, with the first of several anchors still in the list (a folded row drops out). */
export function restoreAnchors(keys: readonly string[], offsets: readonly number[], anchors: readonly Anchor[]): number | null {
  if (anchors.length === 0) return null;
  const index = new Map<string, number>();
  for (const a of anchors) index.set(a.key, -1);
  for (let i = 0; i < keys.length; i++) if (index.has(keys[i]!)) index.set(keys[i]!, i);
  for (const a of anchors) {
    const i = index.get(a.key)!;
    if (i >= 0) return offsets[i]! - a.delta;
  }
  return null;
}
