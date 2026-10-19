// The work-timeline rail: a 28 px column in the transcript's right gutter with one marker
// per prompt or piece of work (edits, writes, tests, commands, failures), spread evenly down a
// hairline track; neighbours cluster when the rail is short. A marker's tooltip names the event;
// clicking it jumps the transcript there. ↑/↓/Home/End move between markers. The event count sits
// at the foot, with the chat's new tokens in its tooltip. Hovering a marker (220 ms) or clicking it
// opens a preview card beside the rail (a click pins it; Esc or a press elsewhere closes it), and a
// band marks the stretch of markers whose rows are on screen.

import { useCallback, useEffect, useLayoutEffect, useMemo, useRef, useState, type ComponentType, type KeyboardEvent, type RefObject } from 'react';
import type { MessageRow } from '../api/types.ts';
import { IconAlert, IconChat, IconCheck, IconClock, IconEdit, IconFilePlus, IconTerminal, type IconProps } from '../ui/icons.tsx';
import { compactCount, newTokens } from './rowMeta.ts';
import { hasWork, railMarkers, timelineEvents, visibleSpan, type EventKind, type RailMarker } from './timeline.ts';

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
  /** The transcript's box: its rows on screen decide the visible-range band. */
  container?: RefObject<HTMLElement | null>;
}

const OPEN_MS = 220;
const CLOSE_MS = 180;

function PreviewCard({ marker, index, total, top, onStep, onShow }: { marker: RailMarker; index: number; total: number; top: number; onStep: (d: -1 | 1) => void; onShow: () => void }) {
  const e = marker.event;
  const Icon = ICON[e.kind];
  return (
    <div className={`tl-card tl-${e.kind}`} style={{ top }} role="dialog" aria-label={`${e.title} preview`} data-testid="timeline-card">
      <div className="tl-card-head">
        <Icon size={13} />
        <span className="tl-card-title">{e.title}</span>
        <span className="tl-card-pos">
          {index + 1} / {total}
        </span>
      </div>
      {e.detail && e.kind !== 'prompt' && <div className="tl-card-path">{e.detail}</div>}
      {e.kind === 'prompt' && <div className="tl-card-body">{e.body || e.detail}</div>}
      {e.diff.length > 0 && (
        <pre className="tl-card-diff">
          {e.diff.map((l, i) => (
            <span key={i} className={`tl-line tl-line-${l.sign === '+' ? 'add' : l.sign === '-' ? 'del' : 'ctx'}`}>
              {l.sign}
              {l.text}
              {'\n'}
            </span>
          ))}
        </pre>
      )}
      {e.output && (
        <details className="tl-card-output">
          <summary>Output</summary>
          <pre>{e.output}</pre>
        </details>
      )}
      {marker.size > 1 && <div className="tl-card-near">and {marker.size - 1} more nearby</div>}
      <div className="tl-card-foot">
        <button type="button" onClick={() => onStep(-1)} disabled={index === 0} aria-label="Previous event">
          Previous
        </button>
        <button type="button" onClick={() => onStep(1)} disabled={index >= total - 1} aria-label="Next event">
          Next
        </button>
        <button type="button" className="tl-card-show" onClick={onShow}>
          Show in conversation
        </button>
      </div>
    </div>
  );
}

export function TimelineRail({ rows, onJump, container }: TimelineRailProps) {
  const events = useMemo(() => timelineEvents(rows), [rows]);
  const shown = hasWork(events);
  const track = useRef<HTMLDivElement>(null);
  const [height, setHeight] = useState(0);
  const [active, setActive] = useState(0);
  const [card, setCard] = useState<{ index: number; pinned: boolean } | null>(null);
  const [rowSpan, setRowSpan] = useState<{ min: number; max: number } | null>(null);
  const [band, setBand] = useState<{ top: number; height: number } | null>(null);
  const timer = useRef<number | undefined>(undefined);
  const root = useRef<HTMLElement>(null);
  const later = (ms: number, f: () => void) => {
    window.clearTimeout(timer.current);
    timer.current = window.setTimeout(f, ms);
  };
  useEffect(() => () => window.clearTimeout(timer.current), []);
  useEffect(() => {
    const el = track.current;
    if (!el || typeof ResizeObserver === 'undefined') return;
    const ro = new ResizeObserver(() => setHeight(el.clientHeight));
    ro.observe(el);
    setHeight(el.clientHeight);
    return () => ro.disconnect();
  }, [shown]);
  const markers = useMemo(() => railMarkers(events, height), [events, height]);

  // Which rows are on screen: read from the transcript's DOM on scroll, so the list stays untouched.
  const measureRows = useCallback(() => {
    const box = container?.current;
    if (!box) return;
    const b = box.getBoundingClientRect();
    let min = Infinity;
    let max = -Infinity;
    box.querySelectorAll<HTMLElement>('[data-testid="transcript-row"][data-row-id]').forEach((el) => {
      const r = el.getBoundingClientRect();
      if (r.bottom <= b.top || r.top >= b.bottom) return;
      const id = Number(el.getAttribute('data-row-id'));
      if (id < min) min = id;
      if (id > max) max = id;
    });
    setRowSpan((old) => {
      if (min > max) return null;
      return old && old.min === min && old.max === max ? old : { min, max };
    });
  }, [container]);
  useEffect(() => {
    const box = container?.current;
    if (!box || !shown) return;
    let raf = 0;
    const tick = () => {
      cancelAnimationFrame(raf);
      raf = requestAnimationFrame(measureRows);
    };
    box.addEventListener('scroll', tick, { capture: true, passive: true });
    tick();
    return () => {
      cancelAnimationFrame(raf);
      box.removeEventListener('scroll', tick, true);
    };
  }, [container, shown, measureRows, rows]);
  const span = useMemo(() => (rowSpan ? visibleSpan(markers, rowSpan.min, rowSpan.max) : null), [markers, rowSpan]);
  useLayoutEffect(() => {
    const btns = track.current?.querySelectorAll<HTMLButtonElement>('.tl-marker');
    const a = span ? btns?.[span.first] : undefined;
    const z = span ? btns?.[span.last] : undefined;
    const next = a && z ? { top: a.offsetTop, height: z.offsetTop + z.offsetHeight - a.offsetTop } : null;
    setBand((old) => (old && next && old.top === next.top && old.height === next.height ? old : next));
  }, [span, markers, height]);

  // A pinned card closes on Esc or a press outside the rail.
  const pinned = card?.pinned === true;
  useEffect(() => {
    if (!pinned) return;
    const onDown = (ev: PointerEvent) => {
      if (!root.current?.contains(ev.target as Node)) setCard(null);
    };
    const onEsc = (ev: globalThis.KeyboardEvent) => {
      if (ev.key === 'Escape') setCard(null);
    };
    document.addEventListener('pointerdown', onDown, true);
    document.addEventListener('keydown', onEsc);
    return () => {
      document.removeEventListener('pointerdown', onDown, true);
      document.removeEventListener('keydown', onEsc);
    };
  }, [pinned]);
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
    <nav className="timeline-rail" aria-label="Work timeline" data-testid="timeline-rail" ref={root}>
      <div className="tl-track" ref={track} onKeyDown={onKey}>
        {band && <div className="tl-band" style={{ top: band.top, height: band.height }} aria-hidden="true" data-testid="timeline-band" />}
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
              onMouseEnter={() => later(OPEN_MS, () => setCard((c) => (c?.pinned ? c : { index: i, pinned: false })))}
              onMouseLeave={() => later(CLOSE_MS, () => setCard((c) => (c?.pinned ? c : null)))}
              onClick={() => {
                setActive(i);
                window.clearTimeout(timer.current);
                setCard({ index: i, pinned: true });
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
      {card && markers[card.index] && (
        <div
          onMouseEnter={() => window.clearTimeout(timer.current)}
          onMouseLeave={() => later(CLOSE_MS, () => setCard((c) => (c?.pinned ? c : null)))}
        >
          <PreviewCard
            marker={markers[card.index]!}
            index={card.index}
            total={markers.length}
            top={Math.max(0, (track.current?.querySelectorAll<HTMLElement>('.tl-marker')[card.index]?.offsetTop ?? 0) - 8)}
            onStep={(d) => {
              const n = card.index + d;
              if (markers[n]) setCard({ index: n, pinned: true });
            }}
            onShow={() => onJump(markers[card.index]!.event.rowId)}
          />
        </div>
      )}
      <span className="tl-count" title={`${tokens.toLocaleString()} new tokens in the loaded part of this chat`}>
        {compactCount(events.length)}
      </span>
    </nav>
  );
}
