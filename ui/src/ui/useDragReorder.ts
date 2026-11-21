// Drag reorder for a vertical list of whole rows (the Pages list, the Queue panel), with
// plain pointer events and no library. A press becomes a drag only once the pointer has
// moved a few pixels, so a click on a row stays a click; while dragging, the row follows
// the pointer and the hook works out where among the other rows it would land. On release
// it reports `move(from, to)` — the index the row ends at — and swallows the click the
// browser would otherwise fire. The list's up/down buttons stay the keyboard way to
// reorder; the hook is the pointer way. The two decisions are pure (placeIndex, moveItem)
// so they are testable without a browser.

import { useCallback, useRef, useState } from 'react';
import type { PointerEvent as ReactPointerEvent } from 'react';

/** A press only turns into a drag after the pointer has moved this far (px). */
export const DRAG_START_PX = 5;

/** Where a dragged row lands among the other rows, in display order: its position is
 *  measured by how many rows have their middle above the pointer. */
export function placeIndex(rows: readonly { top: number; height: number }[], pointerY: number): number {
  return rows.filter((r) => r.top + r.height / 2 < pointerY).length;
}

/** `items` with the entry at `from` taken out and put back at index `to` among the rest. */
export function moveItem<T>(items: readonly T[], from: number, to: number): T[] {
  const out = items.slice();
  const [moved] = out.splice(from, 1);
  out.splice(Math.max(0, Math.min(to, out.length)), 0, moved as T);
  return out;
}

export interface DragState {
  /** The row under the pointer. */
  from: number;
  /** The index it would land at if let go now (among the list with it taken out). */
  to: number;
  /** How far the pointer has travelled; the row's own transform follows it. */
  offset: number;
}

export interface DragRowProps {
  ref: (el: HTMLElement | null) => void;
  onPointerDown?: (e: ReactPointerEvent<HTMLElement>) => void;
}

/**
 * Pass `onMove(from, to)` — indices into the list as it stood when the drag began — and
 * spread `rowProps(index)` over each row (with `draggable` false on rows that must not
 * move, e.g. remote or hand-held ones: they still measure, they just do not start).
 * While a drag is live `drag` describes it, so the caller can lift the dragged row.
 */
export function useDragReorder(onMove: (from: number, to: number) => void) {
  const [drag, setDrag] = useState<DragState | null>(null);
  const rows = useRef(new Map<number, HTMLElement>());
  const live = useRef(false);
  const onMoveRef = useRef(onMove);
  onMoveRef.current = onMove;

  const rowProps = useCallback((index: number, draggable = true): DragRowProps => {
    const ref = (el: HTMLElement | null) => {
      if (el) rows.current.set(index, el);
      else rows.current.delete(index);
    };
    if (!draggable) return { ref };
    const onPointerDown = (e: ReactPointerEvent<HTMLElement>) => {
      if (e.button !== 0 || live.current) return;
      const from = index;
      const startY = e.clientY;
      live.current = true;
      let started = false;
      // The landing index among the other rows, read live from their places on screen.
      const land = (y: number) => {
        const others = [...rows.current.entries()]
          .filter(([i]) => i !== from)
          .map(([, el]) => el.getBoundingClientRect());
        return placeIndex(others, y);
      };
      const detach = () => {
        window.removeEventListener('pointermove', onPtrMove);
        window.removeEventListener('pointerup', onPtrUp);
        window.removeEventListener('pointercancel', onPtrCancel);
        live.current = false;
        setDrag(null);
      };
      const onPtrMove = (ev: PointerEvent) => {
        if (!started && Math.abs(ev.clientY - startY) < DRAG_START_PX) return;
        started = true;
        ev.preventDefault(); // no text selection under the drag
        setDrag({ from, to: land(ev.clientY), offset: ev.clientY - startY });
      };
      const onPtrUp = (ev: PointerEvent) => {
        detach();
        if (!started) return;
        onMoveRef.current(from, land(ev.clientY));
        // A released drag must not also click the row it ended over.
        const swallow = (ce: MouseEvent) => {
          ce.preventDefault();
          ce.stopPropagation();
          window.removeEventListener('click', swallow, true);
        };
        window.addEventListener('click', swallow, true);
      };
      const onPtrCancel = () => detach();
      window.addEventListener('pointermove', onPtrMove);
      window.addEventListener('pointerup', onPtrUp);
      window.addEventListener('pointercancel', onPtrCancel);
    };
    return { ref, onPointerDown };
  }, []);

  return { rowProps, drag };
}
