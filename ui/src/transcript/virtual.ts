// Geometry for the virtualised transcript: measured row heights are cached by
// key, unmeasured rows use an estimate, and only the rows in (or near) the viewport are mounted.
// Pure, so the window and anchor maths are tested without a DOM.

export const ESTIMATE = 80;
export const OVERSCAN_PX = 600;

export class Geometry {
  private readonly heights = new Map<string, number>();

  /** Record a measured height; returns true when it changed. */
  measure(key: string, h: number): boolean {
    if (this.heights.get(key) === h) return false;
    this.heights.set(key, h);
    return true;
  }

  height(key: string): number {
    return this.heights.get(key) ?? ESTIMATE;
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

/** The first key whose row is at least partly visible, and how far its top sits from the scroll top. */
export function anchorAt(keys: readonly string[], offsets: readonly number[], scrollTop: number): { key: string; delta: number } | null {
  for (let i = 0; i < keys.length; i++) {
    if (offsets[i + 1]! > scrollTop) return { key: keys[i]!, delta: offsets[i]! - scrollTop };
  }
  return null;
}

/** The scroll top that puts `anchor` back where it was, after rows above it changed. */
export function restoreAnchor(keys: readonly string[], offsets: readonly number[], anchor: { key: string; delta: number }): number | null {
  const i = keys.indexOf(anchor.key);
  if (i < 0) return null;
  return offsets[i]! - anchor.delta;
}
