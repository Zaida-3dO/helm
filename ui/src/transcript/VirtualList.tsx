// The virtualised scroller. Follow mode keeps the view pinned to the bottom while the
// reader is there; scrolling up detaches it. When not following, the first visible row is the
// anchor: after any change above it (older rows arriving, a row growing) the scroll top is moved so
// the anchor stays put. A "Jump to latest" button shows whenever the reader is away from the end.

import { useCallback, useLayoutEffect, useRef, useState, type ReactNode } from 'react';
import { anchorAt, Geometry, restoreAnchor } from './virtual.ts';

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
}

const BOTTOM_SLACK = 24;
const PAGE_TRIGGER = 300;

export function VirtualList({ keys, render, hasOlder, onLoadOlder, onJumpLatest, jumpTo, onFollowChange }: VirtualListProps) {
  const scroller = useRef<HTMLDivElement>(null);
  const geometry = useRef(new Geometry()).current;
  const [, setTick] = useState(0);
  const [view, setView] = useState({ top: 0, height: 800 });
  const following = useRef(true);
  const [showJump, setShowJump] = useState(false);
  const anchor = useRef<{ key: string; delta: number } | null>(null);
  const loading = useRef(false);
  const suppressPagingUntil = useRef(0);

  const offsets = geometry.offsets(keys);
  const total = offsets[keys.length]!;
  const { start, end } = geometry.window(offsets, view.top, view.height);

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

  // After every render: keep following, or keep the anchor still.
  useLayoutEffect(() => {
    const el = scroller.current;
    if (!el) return;
    if (following.current) {
      el.scrollTop = el.scrollHeight;
    } else if (anchor.current) {
      const top = restoreAnchor(keys, geometry.offsets(keys), anchor.current);
      if (top !== null && Math.abs(el.scrollTop - top) > 1) el.scrollTop = top;
    }
  });

  useLayoutEffect(() => {
    if (!jumpTo) return;
    const el = scroller.current;
    const i = keys.indexOf(jumpTo);
    if (!el || i < 0) return;
    setFollowing(false);
    suppressPagingUntil.current = Date.now() + 500;
    const o = geometry.offsets(keys);
    el.scrollTop = Math.max(0, o[i]! - el.clientHeight / 3);
    anchor.current = anchorAt(keys, o, el.scrollTop);
    // Only once per target.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [jumpTo]);

  const onScroll = () => {
    const el = scroller.current;
    if (!el) return;
    const atBottom = el.scrollHeight - el.scrollTop - el.clientHeight < BOTTOM_SLACK;
    setFollowing(atBottom);
    anchor.current = atBottom ? null : anchorAt(keys, geometry.offsets(keys), el.scrollTop);
    setView({ top: el.scrollTop, height: el.clientHeight });
    if (el.scrollTop < PAGE_TRIGGER && hasOlder && !loading.current && Date.now() > suppressPagingUntil.current) {
      loading.current = true;
      // Paging detaches follow before the older rows arrive.
      setFollowing(false);
      anchor.current = anchorAt(keys, geometry.offsets(keys), el.scrollTop);
      void onLoadOlder().finally(() => {
        loading.current = false;
      });
    }
  };

  useLayoutEffect(() => {
    const el = scroller.current;
    if (!el) return;
    setView({ top: el.scrollTop, height: el.clientHeight });
    const ro = new ResizeObserver(() => setView({ top: el.scrollTop, height: el.clientHeight }));
    ro.observe(el);
    return () => ro.disconnect();
  }, []);

  const measureRef = useCallback(
    (key: string) => (node: HTMLDivElement | null) => {
      if (!node) return;
      const ro = new ResizeObserver(() => {
        if (geometry.measure(key, node.offsetHeight)) setTick((t) => t + 1);
      });
      ro.observe(node);
      if (geometry.measure(key, node.offsetHeight)) setTick((t) => t + 1);
      return () => ro.disconnect();
    },
    [geometry],
  );

  const jumpLatest = () => {
    void onJumpLatest().then(() => {
      setFollowing(true);
      anchor.current = null;
      const el = scroller.current;
      if (el) el.scrollTop = el.scrollHeight;
    });
  };

  return (
    <div className="transcript-wrap">
      <div className="transcript" ref={scroller} onScroll={onScroll} data-testid="transcript">
        <div style={{ height: total, position: 'relative' }}>
          <div style={{ position: 'absolute', top: offsets[start], left: 0, right: 0 }}>
            {keys.slice(start, end).map((k, j) => (
              <div key={k} ref={measureRef(k)} data-key={k}>
                {render(k, start + j)}
              </div>
            ))}
          </div>
        </div>
      </div>
      {showJump && (
        <button type="button" className="jump-latest" onClick={jumpLatest} data-testid="jump-latest">
          Jump to latest
        </button>
      )}
    </div>
  );
}
