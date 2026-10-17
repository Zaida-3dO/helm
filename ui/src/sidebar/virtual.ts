// Fixed-height virtualisation for the sidebar list: each item has a known height,
// rows are positioned absolutely, and overscan is 8 rows on each side.

export const OVERSCAN = 8;

export interface Layout {
  offsets: number[];
  total: number;
}

export function layout(heights: number[]): Layout {
  const offsets = new Array<number>(heights.length);
  let y = 0;
  for (let i = 0; i < heights.length; i++) {
    offsets[i] = y;
    y += heights[i]!;
  }
  return { offsets, total: y };
}

/** The half-open index range to mount for a viewport, overscan included. */
export function visibleRange(
  l: Layout,
  heights: number[],
  scrollTop: number,
  viewport: number,
  overscan = OVERSCAN,
): [number, number] {
  const n = heights.length;
  if (n === 0) return [0, 0];
  // First item whose bottom is below scrollTop.
  let lo = 0;
  let hi = n - 1;
  while (lo < hi) {
    const mid = (lo + hi) >> 1;
    if (l.offsets[mid]! + heights[mid]! <= scrollTop) lo = mid + 1;
    else hi = mid;
  }
  const first = lo;
  let last = first;
  const bottom = scrollTop + viewport;
  while (last < n && l.offsets[last]! < bottom) last++;
  return [Math.max(0, first - overscan), Math.min(n, last + overscan)];
}
