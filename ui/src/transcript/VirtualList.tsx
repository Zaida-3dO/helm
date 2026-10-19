// The virtualised scroller. Follow mode keeps the view pinned to the bottom while the
// reader is there; scrolling up detaches it. When not following, the rows in view are the anchors:
// after any change above them (older rows arriving, a row growing, a turn folding) the scroll top
// is moved so the first anchor still on screen stays put. A "Jump to latest" button shows whenever
// the reader is away from the end.
//
// Stability:
// - Every row keeps one ref callback for as long as it is mounted, and one ResizeObserver watches
//   them all. A resize is re-laid out and re-anchored in the same frame, before it is painted.
// - Rows sit between two spacers in normal flow, so a row in view never moves when the window
//   above it changes.
// - Anchors are read from the DOM (where each row really is, margins included), not from summed
//   heights, so estimates that turn out wrong never show as a jump.
// - Heights are kept in a cache shared across chat switches, and unmeasured rows use an estimate
//   by kind and text length (virtual.ts).
// - Scrolling re-renders only when the mounted window would change.

import { useCallback, useEffect, useLayoutEffect, useRef, useState, type KeyboardEvent, type ReactNode, type WheelEvent } from 'react';
import { flushSync } from 'react-dom';
import { Glyph } from './icons.tsx';
import { ESTIMATE, Geometry, restoreAnchors, sharedHeights, type Anchor } from './virtual.ts';

export interface VirtualListProps {
  keys: string[];
  render: (key: string, index: number) => ReactNode;
  hasOlder: boolean;
  /** Load the page above. Resolves once the rows are in the store. */
  onLoadOlder: () => Promise<void>;
  /** A detached thread reloads its newest page before following again. */
  onJumpLatest: () => Promise<void>;
  /** Scroll so this key is in view, once, and suppress the paging that scroll would trigger. */
  jumpTo?: string | null;
  /** Reports whether the reader is following the end (the store trims the other end). */
  onFollowChange?: (following: boolean) => void;
  /** A height guess for a row not yet measured (virtual.ts `estimateHeight`). */
  estimate?: (key: string) => number;
}

const BOTTOM_SLACK = 24;
const PAGE_TRIGGER = 300;
const MAX_ANCHORS = 8;
const GESTURE_MS = 200;
const NAV_KEYS = new Set(['ArrowUp', 'ArrowDown', 'PageUp', 'PageDown', 'Home', 'End', ' ', 'Tab']);

/** Without an estimator: folds are one short line, everything else the flat estimate. */
const defaultEstimate = (key: string) => (key.startsWith('fold:') ? 45 : ESTIMATE);

type RowRef = (node: HTMLDivElement | null) => void;

interface Rows {
  observer: ResizeObserver | null;
  keyOf: Map<Element, string>;
  nodeOf: Map<string, HTMLDivElement>;
  refs: Map<string, RowRef>;
}

export function VirtualList({ keys, render, hasOlder, onLoadOlder, onJumpLatest, jumpTo, onFollowChange, estimate }: VirtualListProps) {
  const scroller = useRef<HTMLDivElement>(null);
  const geometry = useRef<Geometry | null>(null);
  geometry.current ??= new Geometry(sharedHeights);
  const g = geometry.current;
  g.estimate = estimate ?? defaultEstimate;
  const [, setTick] = useState(0);
  const [view, setView] = useState({ top: 0, height: 800 });
  const following = useRef(true);
  const [showJump, setShowJump] = useState(false);
  /** Rows in view, preferring those whose top edge is visible; delta is the DOM distance from the scroller's top. */
  const anchors = useRef<Anchor[]>([]);
  const loading = useRef(false);
  const suppressPagingUntil = useRef(0);
  const rows = useRef<Rows>({ observer: null, keyOf: new Map(), nodeOf: new Map(), refs: new Map() });

  const offsets = g.offsets(keys);
  const total = offsets[keys.length]!;
  // The window is chosen for where this render will leave the scroll, not where it was.
  let target = view.top;
  if (following.current) target = Math.max(0, total - view.height);
  else if (anchors.current.length) target = restoreAnchors(keys, offsets, anchors.current) ?? view.top;
  const { start, end } = g.window(offsets, target, view.height);
  const windowRef = useRef({ start, end });
  windowRef.current = { start, end };

  const setFollowing = useCallback(
    (f: boolean) => {
      if (following.current !== f) {
        following.current = f;
        onFollowChange?.(f);
      }
      setShowJump(!f);
    },
    [onFollowChange],
  );

  const observer = () => {
    const r = rows.current;
    r.observer ??= new ResizeObserver((entries) => {
      let changed = false;
      for (const e of entries) {
        const k = r.keyOf.get(e.target);
        if (k !== undefined && g.measure(k, (e.target as HTMLElement).offsetHeight)) changed = true;
      }
      if (!changed) return;
      // Only mounted rows are observed and the spacers only cover rows that are not mounted, so a
      // resize needs no new render unless the mounted window moves: just re-pin or re-anchor.
      // Otherwise re-render now, so the new layout is in this frame, before it is painted.
      if (settleRef.current()) return;
      flushSync(() => setTick((t) => t + 1));
    });
    return r.observer;
  };

  useEffect(
    () => () => {
      rows.current.observer?.disconnect();
      rows.current.observer = null;
    },
    [],
  );

  const refFor = (k: string): RowRef => {
    const r = rows.current;
    let f = r.refs.get(k);
    if (!f) {
      f = (node) => {
        if (node) {
          r.keyOf.set(node, k);
          r.nodeOf.set(k, node);
          observer().observe(node);
          // Measured during the commit, so the re-render that uses it lands before the paint.
          if (g.measure(k, node.offsetHeight)) setTick((t) => t + 1);
          return;
        }
        const old = r.nodeOf.get(k);
        if (old) {
          r.observer?.unobserve(old);
          r.keyOf.delete(old);
        }
        r.nodeOf.delete(k);
      };
      r.refs.set(k, f);
    }
    return f;
  };

  /** Where the mounted rows in view sit now, from the DOM. */
  const captureAnchors = (el: HTMLDivElement): Anchor[] => {
    const top = el.getBoundingClientRect().top;
    const bottom = top + el.clientHeight;
    const whole: Anchor[] = [];
    const partial: Anchor[] = [];
    const { start: s, end: e } = windowRef.current;
    for (let i = s; i < e && whole.length < MAX_ANCHORS; i++) {
      const k = keys[i];
      const node = k === undefined ? undefined : rows.current.nodeOf.get(k);
      if (!node) continue;
      const r = node.getBoundingClientRect();
      if (r.bottom <= top) continue;
      if (r.top >= bottom) break;
      (r.top >= top ? whole : partial).push({ key: k!, delta: r.top - top });
    }
    return [...whole, ...partial];
  };

  /**
   * Keep following, or put the first anchor still on screen back where it was, then note where the
   * rows in view are now. Returns false when the mounted window no longer fits the view (the
   * caller must re-render instead).
   */
  const settle = (fromObserver: boolean): boolean => {
    const el = scroller.current;
    if (!el) return true;
    if (fromObserver) {
      const o = g.offsets(keys);
      const t = following.current ? Math.max(0, o[keys.length]! - el.clientHeight) : el.scrollTop;
      const w = g.window(o, t, el.clientHeight);
      if (w.start !== windowRef.current.start || w.end !== windowRef.current.end) return false;
    }
    if (following.current) {
      if (el.scrollHeight - el.clientHeight - el.scrollTop > 0.5) el.scrollTop = el.scrollHeight;
      return true;
    }
    if (anchors.current.length) {
      const top = el.getBoundingClientRect().top;
      let fixed = false;
      for (const a of anchors.current) {
        const node = rows.current.nodeOf.get(a.key);
        if (!node) continue;
        const moved = node.getBoundingClientRect().top - top - a.delta;
        if (Math.abs(moved) > 0.5) el.scrollTop += moved;
        fixed = true;
        break;
      }
      if (!fixed) {
        const t = restoreAnchors(keys, g.offsets(keys), anchors.current);
        if (t !== null) el.scrollTop = t;
      }
    }
    anchors.current = captureAnchors(el);
    return true;
  };
  const settleRef = useRef<() => boolean>(() => true);
  settleRef.current = () => settle(true);

  useLayoutEffect(() => {
    settle(false);
    // Forget ref callbacks of rows that left the window.
    const r = rows.current;
    if (r.refs.size > end - start + 32) {
      const live = new Set(keys.slice(start, end));
      for (const k of r.refs.keys()) if (!live.has(k)) r.refs.delete(k);
    }
  });

  useLayoutEffect(() => {
    if (!jumpTo) return;
    const el = scroller.current;
    const i = keys.indexOf(jumpTo);
    if (!el || i < 0) return;
    setFollowing(false);
    suppressPagingUntil.current = Date.now() + 500;
    el.scrollTop = Math.max(0, g.offsets(keys)[i]! - el.clientHeight / 3);
    anchors.current = [];
    setView({ top: el.scrollTop, height: el.clientHeight });
    // Only once per target.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [jumpTo]);

  // Following stops only on the reader's own input: a wheel up, or a scroll during a
  // scrollbar drag or within GESTURE_MS of a key or touch. Layout changes never stop it.
  const gestureAt = useRef(-Infinity);
  const dragging = useRef(false);
  const gesture = () => {
    gestureAt.current = performance.now();
  };
  const onWheel = (e: WheelEvent<HTMLDivElement>) => {
    gesture();
    if (e.deltaY < 0 && (scroller.current?.scrollTop ?? 0) > 0) setFollowing(false);
  };
  const onKeyDown = (e: KeyboardEvent<HTMLDivElement>) => {
    if (NAV_KEYS.has(e.key)) gesture();
  };

  const onScroll = () => {
    const el = scroller.current;
    if (!el) return;
    const atBottom = el.scrollHeight - el.scrollTop - el.clientHeight < BOTTOM_SLACK;
    if (atBottom) setFollowing(true);
    else if (dragging.current || performance.now() - gestureAt.current < GESTURE_MS) setFollowing(false);
    else if (following.current) {
      // Content moved the end away from us: follow it.
      el.scrollTop = el.scrollHeight;
      return;
    }
    anchors.current = following.current ? [] : captureAnchors(el);
    // Re-render only when the mounted window changes.
    const w = g.window(g.offsets(keys), el.scrollTop, el.clientHeight);
    if (w.start !== windowRef.current.start || w.end !== windowRef.current.end || el.clientHeight !== view.height) {
      setView({ top: el.scrollTop, height: el.clientHeight });
    }
    if (el.scrollTop < PAGE_TRIGGER && hasOlder && !loading.current && Date.now() > suppressPagingUntil.current) {
      loading.current = true;
      // Paging detaches follow before the older rows arrive.
      setFollowing(false);
      if (!anchors.current.length) anchors.current = captureAnchors(el);
      void onLoadOlder().finally(() => {
        loading.current = false;
      });
    }
  };

  useLayoutEffect(() => {
    const el = scroller.current;
    if (!el) return;
    const sync = () => {
      // Cached heights are only reused at the width they were measured at.
      g.width = Math.round(el.clientWidth);
      setView({ top: el.scrollTop, height: el.clientHeight });
    };
    sync();
    const ro = new ResizeObserver(sync);
    ro.observe(el);
    return () => ro.disconnect();
  }, [g]);

  const jumpLatest = () => {
    void onJumpLatest().then(() => {
      setFollowing(true);
      anchors.current = [];
      const el = scroller.current;
      if (el) el.scrollTop = el.scrollHeight;
    });
  };

  return (
    <div className="transcript-wrap">
      <div
        className="transcript"
        ref={scroller}
        onScroll={onScroll}
        onWheel={onWheel}
        onKeyDown={onKeyDown}
        onTouchMove={gesture}
        onPointerDown={() => {
          gesture();
          dragging.current = true;
          // The release may land outside the scroller.
          window.addEventListener(
            'pointerup',
            () => {
              gesture();
              dragging.current = false;
            },
            { once: true, capture: true },
          );
        }}
        onPointerCancel={() => {
          dragging.current = false;
        }}
        data-testid="transcript"
        // The browser's own scroll anchoring holds the rows in view still, even mid smooth-scroll;
        // the layout effect above only corrects what it leaves.
        style={{ overflowAnchor: 'auto' }}
      >
        {/* Spacers in normal flow rather than a positioned window: the rows in view never move
            when the window above them changes. */}
        <div>
          <div style={{ height: offsets[start] }} />
          {keys.slice(start, end).map((k, j) => (
            <div key={k} ref={refFor(k)} data-key={k}>
              {render(k, start + j)}
            </div>
          ))}
          <div style={{ height: total - offsets[end]! }} />
        </div>
      </div>
      {showJump && (
        <button type="button" className="jump-latest" onClick={jumpLatest} data-testid="jump-latest">
          <Glyph name="arrow-down" size={13} />
          Latest
        </button>
      )}
    </div>
  );
}
