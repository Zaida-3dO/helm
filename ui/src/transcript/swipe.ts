// Swiping through a set of pictures: a pointer drag (finger, pen or mouse) or a touchpad's sideways
// two-finger scroll steps to the next or previous one. The decisions are pure and tested; useSwipe
// binds them to an element.

import { useRef, type MouseEvent, type PointerEvent, type WheelEvent } from 'react';

/** How far a drag must travel sideways before it counts as a swipe. */
export const SWIPE_MIN = 48;
/** A touchpad scroll steps once, then waits this long, so one flick is one picture. */
export const WHEEL_COOLDOWN_MS = 450;
const WHEEL_MIN = 24;

export type Step = 1 | -1;

/** A drag that ended `dx` right and `dy` down: dragging left moves to the next picture. */
export function swipeStep(dx: number, dy: number): Step | 0 {
  if (Math.abs(dx) < SWIPE_MIN || Math.abs(dx) < Math.abs(dy) * 1.5) return 0;
  return dx < 0 ? 1 : -1;
}

/** A wheel event's sideways scroll: scrolling right moves to the next picture. */
export function wheelStep(deltaX: number, deltaY: number, now: number, lastStepAt: number): Step | 0 {
  if (Math.abs(deltaX) < WHEEL_MIN || Math.abs(deltaX) < Math.abs(deltaY) * 1.5) return 0;
  if (now - lastStepAt < WHEEL_COOLDOWN_MS) return 0;
  return deltaX > 0 ? 1 : -1;
}

/** Handlers to spread on the element that swipes; a drag that stepped also swallows the click that ends it. */
export function useSwipe(onStep: (step: Step) => void, enabled = true) {
  const start = useRef<{ x: number; y: number } | null>(null);
  const swiped = useRef(false);
  const lastWheel = useRef(-Infinity);
  if (!enabled) return {};
  return {
    onPointerDown: (e: PointerEvent<HTMLElement>) => {
      if (e.pointerType === 'mouse' && e.button !== 0) return;
      start.current = { x: e.clientX, y: e.clientY };
      swiped.current = false;
    },
    onPointerUp: (e: PointerEvent<HTMLElement>) => {
      const s = start.current;
      start.current = null;
      if (!s) return;
      const step = swipeStep(e.clientX - s.x, e.clientY - s.y);
      if (step) {
        swiped.current = true;
        onStep(step);
      }
    },
    onPointerCancel: () => {
      start.current = null;
    },
    onClickCapture: (e: MouseEvent<HTMLElement>) => {
      if (!swiped.current) return;
      swiped.current = false;
      e.preventDefault();
      e.stopPropagation();
    },
    onWheel: (e: WheelEvent<HTMLElement>) => {
      const step = wheelStep(e.deltaX, e.deltaY, e.timeStamp, lastWheel.current);
      if (step) {
        lastWheel.current = e.timeStamp;
        onStep(step);
      }
    },
  };
}
