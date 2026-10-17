// Placement for anchored surfaces. Pure, so it is tested.

export interface Rect {
  left: number;
  top: number;
  width: number;
  height: number;
}

export interface Placed {
  left: number;
  top: number;
  /** True when it flipped above the anchor. */
  above: boolean;
}

const MARGIN = 8;

/**
 * Below the anchor's left edge with `gap` px; flip above when it would overflow the bottom and
 * fits above; clamp 8 px from the window sides (and top/bottom as a last resort).
 */
export function placeBelow(anchor: Rect, size: { width: number; height: number }, viewport: { width: number; height: number }, gap = 6): Placed {
  let top = anchor.top + anchor.height + gap;
  let above = false;
  if (top + size.height > viewport.height - MARGIN && anchor.top - gap - size.height >= MARGIN) {
    top = anchor.top - gap - size.height;
    above = true;
  }
  top = Math.max(MARGIN, Math.min(top, viewport.height - MARGIN - size.height));
  const left = Math.max(MARGIN, Math.min(anchor.left, viewport.width - MARGIN - size.width));
  return { left, top, above };
}

/** A context menu at a point: flips left/up when it would leave the window. */
export function placeAtPoint(x: number, y: number, size: { width: number; height: number }, viewport: { width: number; height: number }): Placed {
  let left = x;
  let top = y;
  if (left + size.width > viewport.width - MARGIN) left = Math.max(MARGIN, x - size.width);
  let above = false;
  if (top + size.height > viewport.height - MARGIN) {
    top = Math.max(MARGIN, y - size.height);
    above = true;
  }
  return { left, top, above };
}

/** Next enabled index for arrow-key navigation in a list, wrapping. -1 when none is enabled. */
export function nextIndex(current: number, step: 1 | -1, disabled: boolean[]): number {
  const n = disabled.length;
  if (n === 0) return -1;
  for (let k = 1; k <= n; k++) {
    const i = (((current + step * k) % n) + n) % n;
    if (!disabled[i]) return i;
  }
  return -1;
}
