// The work-timeline rail: a 28 px column in the transcript's right gutter with one marker
// per prompt or piece of work (edits, writes, tests, commands, failures), spread evenly down a
// hairline track; neighbours cluster when the rail is short. A marker's tooltip names the event;
// clicking it jumps the transcript there. ↑/↓/Home/End move between markers. The event count sits
// at the foot, with the chat's new tokens in its tooltip.

import { useEffect, useMemo, useRef, useState, type ComponentType, type KeyboardEvent } from 'react';
import type { MessageRow } from '../api/types.ts';
import { IconAlert, IconChat, IconCheck, IconClock, IconEdit, IconFilePlus, IconTerminal, type IconProps } from '../ui/icons.tsx';
import { compactCount, newTokens } from './rowMeta.ts';
import { hasWork, railMarkers, timelineEvents, type EventKind } from './timeline.ts';

const ICON: Record<EventKind, ComponentType<IconProps>> = {
  prompt: IconChat,
  edit: IconEdit,
  write: IconFilePlus,
  test: IconCheck,
  command: IconTerminal,
  failed: IconAlert,
  running: IconClock,
};

export interface TimelineRailProps {
  rows: readonly MessageRow[];
  onJump: (rowId: number) => void;
}

export function TimelineRail({ rows, onJump }: TimelineRailProps) {
  const events = useMemo(() => timelineEvents(rows), [rows]);
  const shown = hasWork(events);
  const track = useRef<HTMLDivElement>(null);
  const [height, setHeight] = useState(0);
  const [active, setActive] = useState(0);
  useEffect(() => {
    const el = track.current;
    if (!el || typeof ResizeObserver === 'undefined') return;
    const ro = new ResizeObserver(() => setHeight(el.clientHeight));
    ro.observe(el);
    setHeight(el.clientHeight);
    return () => ro.disconnect();
  }, [shown]);
  const markers = useMemo(() => railMarkers(events, height), [events, height]);
  const tokens = useMemo(() => rows.reduce((n, r) => n + (r.details?.usage ? newTokens(r.details.usage) : 0), 0), [rows]);

  if (!shown) return null;
  const focusAt = (i: number) => {
    const n = Math.max(0, Math.min(markers.length - 1, i));
    setActive(n);
    track.current?.querySelectorAll<HTMLButtonElement>('.tl-marker')[n]?.focus();
  };
  const onKey = (e: KeyboardEvent) => {
    const keyMoves: Record<string, number> = { ArrowUp: active - 1, ArrowDown: active + 1, Home: 0, End: markers.length - 1 };
    if (!(e.key in keyMoves)) return;
    e.preventDefault();
    focusAt(keyMoves[e.key]!);
  };
  const current = Math.min(active, markers.length - 1);

  return (
    <nav className="timeline-rail" aria-label="Work timeline" data-testid="timeline-rail">
      <div className="tl-track" ref={track} onKeyDown={onKey}>
        {markers.map((m, i) => {
          const Icon = ICON[m.event.kind];
          const label = `${m.event.title}${m.event.detail ? `: ${m.event.detail}` : ''}${m.size > 1 ? ` (and ${m.size - 1} nearby)` : ''}`;
          return (
            <button
              key={`${m.event.rowId}:${i}`}
              type="button"
              className={`tl-marker tl-${m.event.kind}`}
              data-row-id={m.event.rowId}
              tabIndex={i === current ? 0 : -1}
              title={label}
              aria-label={label}
              onFocus={() => setActive(i)}
              onClick={() => {
                setActive(i);
                onJump(m.event.rowId);
              }}
            >
              <span className="tl-tile">
                <Icon size={13} />
              </span>
              {m.size > 1 && <span className="tl-cluster" aria-hidden="true" />}
            </button>
          );
        })}
      </div>
      <span className="tl-count" title={`${tokens.toLocaleString()} new tokens in the loaded part of this chat`}>
        {compactCount(events.length)}
      </span>
    </nav>
  );
}
