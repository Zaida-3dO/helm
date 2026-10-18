// The Allow card's settle gate: a click decides
// only if the clicked button's slot has stayed put (within 0.5 px) for 400 ms, checked both at the
// latest animation frame and at the click itself. Anything that can move the cards starts the
// window over: mount, a change in the set of cards, a resize, the page becoming visible, a scroll
// inside the cards, or a frame where a slot moved or has no reference yet.

export const SETTLE_MS = 400;
export const MOVE_PX = 0.5;

export interface Rect {
  x: number;
  y: number;
  width: number;
  height: number;
}

export function moved(a: Rect, b: Rect): boolean {
  return (
    Math.abs(a.x - b.x) > MOVE_PX ||
    Math.abs(a.y - b.y) > MOVE_PX ||
    Math.abs(a.width - b.width) > MOVE_PX ||
    Math.abs(a.height - b.height) > MOVE_PX
  );
}

export class SettleGate {
  private since: number;
  private readonly refs = new Map<string, Rect>();

  constructor(now: number) {
    this.since = now;
  }

  /** Start the window over (mount, card set change, resize, visible, scroll). */
  reset(now: number): void {
    this.since = now;
    this.refs.clear();
  }

  /** One animation frame's slot positions, by button id. */
  frame(now: number, rects: Map<string, Rect>): void {
    for (const id of [...this.refs.keys()]) {
      if (!rects.has(id)) {
        this.refs.delete(id);
        this.since = now;
      }
    }
    for (const [id, r] of rects) {
      const ref = this.refs.get(id);
      if (!ref || moved(ref, r)) {
        this.refs.set(id, r);
        this.since = now;
      }
    }
  }

  /** Whether a click on `id`, whose slot is at `rect` now, counts. A moved slot restarts the window. */
  canDecide(id: string, now: number, rect: Rect): boolean {
    const ref = this.refs.get(id);
    if (!ref || moved(ref, rect)) {
      this.refs.set(id, rect);
      this.since = now;
      return false;
    }
    return now - this.since >= SETTLE_MS;
  }
}
