// The find-in-chat bar: Ctrl+F opens it at the transcript's top right,
// Enter / Ctrl+G step to the older match, Shift+Enter / Ctrl+Shift+G to the newer one, Escape
// closes it and gives focus back. Matches are highlighted with the CSS Custom Highlight API, so the
// rendered Markdown is never rewritten. The loaded rows are matched locally at once; the host then
// searches the chat's whole stored history, and a match in an older row loads it when stepped to.

import { useEffect, useMemo, useRef, useState, type RefObject } from 'react';
import type { MessageRow } from '../api/types.ts';
import { useBinding, useLayer } from '../ui/index.ts';
import { IconChevronDown, IconChevronUp, IconSearch, IconX } from '../ui/icons.tsx';
import { FIND_CAP, FIND_MAX_QUERY, countLabel, findMatches, mergeHostMatches, step, type FindMatch, type HostFound } from './find.ts';

const ACTIVITY_KEY = 'helm.find.includeActivity';
const ALL = 'helm-find';
const CURRENT = 'helm-find-current';
/** Text inside these is chrome, not the conversation, and is never highlighted. */
const SKIP = '.row-actions, .row-user-actions, .md-code-copy';

interface HighlightApi {
  highlights: Map<string, unknown>;
}
type HighlightCtor = new (...ranges: Range[]) => unknown;

function highlightApi(): { css: HighlightApi; Highlight: HighlightCtor } | null {
  const css = (globalThis as { CSS?: Partial<HighlightApi> }).CSS;
  const Highlight = (globalThis as { Highlight?: HighlightCtor }).Highlight;
  return css?.highlights && Highlight ? { css: css as HighlightApi, Highlight } : null;
}

function clearHighlights() {
  const api = highlightApi();
  api?.css.highlights.delete(ALL);
  api?.css.highlights.delete(CURRENT);
}

/** Highlight every occurrence in the mounted rows; return the current match's range, if mounted. */
function paint(root: HTMLElement, query: string, current: FindMatch | null): Range | null {
  const api = highlightApi();
  if (!api) return null;
  const q = query.toLowerCase();
  const all: Range[] = [];
  let currentRange: Range | null = null;
  root.querySelectorAll<HTMLElement>('[data-testid="transcript-row"]').forEach((rowEl) => {
    const rowId = Number(rowEl.dataset.rowId);
    const isCurrent = current?.rowId === rowId;
    let k = 0;
    const walker = document.createTreeWalker(rowEl, NodeFilter.SHOW_TEXT, {
      acceptNode: (n) => (n.parentElement?.closest(SKIP) ? NodeFilter.FILTER_REJECT : NodeFilter.FILTER_ACCEPT),
    });
    for (let n = walker.nextNode(); n; n = walker.nextNode()) {
      const text = (n.nodeValue ?? '').toLowerCase();
      for (let i = text.indexOf(q); i >= 0; i = text.indexOf(q, i + q.length)) {
        const r = document.createRange();
        r.setStart(n, i);
        r.setEnd(n, i + q.length);
        all.push(r);
        // The rendered text can differ from the stored text (Markdown syntax), so the occurrence
        // index is clamped to the last one found in the row.
        if (isCurrent && (k <= current.occurrence || !currentRange)) currentRange = r;
        k++;
      }
    }
  });
  api.css.highlights.set(ALL, new api.Highlight(...all));
  if (currentRange) api.css.highlights.set(CURRENT, new api.Highlight(currentRange));
  else api.css.highlights.delete(CURRENT);
  return currentRange;
}

export interface FindBarProps {
  rows: readonly MessageRow[];
  /** One step of the host's whole-history search; absent where there is no host (tests). */
  hostFind?: (query: string, includeActivity: boolean, after?: string) => Promise<{ rows: HostFound[]; nextCursor: string | null }>;
  /** The transcript box: its rows are searched for highlights and it hosts the bar. */
  container: RefObject<HTMLElement | null>;
  /** Bring a row into view (opening a folded turn if needed). */
  onJump: (rowId: number) => void;
  /** The bar closed: the chat can forget the last jump. */
  onClose?: () => void;
}

export function FindBar({ rows, hostFind, container, onJump, onClose }: FindBarProps) {
  const [open, setOpen] = useState(false);
  const [query, setQuery] = useState('');
  const [debounced, setDebounced] = useState('');
  // The current match by row and occurrence, so loading older rows does not move it.
  const [target, setTarget] = useState<FindMatch | null>(null);
  const [activity, setActivity] = useState(() => {
    try {
      return localStorage.getItem(ACTIVITY_KEY) === '1';
    } catch {
      return false;
    }
  });
  const input = useRef<HTMLInputElement>(null);
  const returnFocus = useRef<HTMLElement | null>(null);

  // Search 180 ms after the last keystroke.
  useEffect(() => {
    const t = window.setTimeout(() => setDebounced(query), 180);
    return () => window.clearTimeout(t);
  }, [query]);
  // The host's matches over the whole stored history, for this query. Null while it is working.
  const [hostRows, setHostRows] = useState<HostFound[] | null>(null);
  useEffect(() => {
    setHostRows(null);
    if (!open || !hostFind || !debounced.trim()) return;
    let stale = false;
    void (async () => {
      const found: HostFound[] = [];
      let total = 0;
      let after: string | undefined;
      try {
        for (;;) {
          const r = await hostFind(debounced, activity, after);
          if (stale) return;
          found.push(...r.rows);
          total += r.rows.reduce((n, x) => n + x.count, 0);
          if (!r.nextCursor || total >= FIND_CAP) break;
          after = r.nextCursor;
        }
        setHostRows(found);
      } catch {
        // The loaded rows still answer.
      }
    })();
    return () => {
      stale = true;
    };
  }, [open, hostFind, debounced, activity]);
  const { matches, capped } = useMemo(() => {
    if (!open) return { matches: [], capped: false };
    const local = findMatches(rows, debounced, activity);
    if (!hostRows) return local;
    return mergeHostMatches(hostRows, local.matches, new Set(rows.map((r) => r.id)));
  }, [open, rows, debounced, activity, hostRows]);
  // A new query starts at the newest match; new rows keep the reader's place when they can.
  useEffect(() => setTarget(matches.at(-1) ?? null), [debounced, activity]); // eslint-disable-line react-hooks/exhaustive-deps
  const found = target ? matches.findIndex((m) => m.rowId === target.rowId && m.occurrence === target.occurrence) : -1;
  const safeIndex = found >= 0 ? found : matches.length - 1;
  const current = safeIndex >= 0 ? matches[safeIndex]! : null;
  const currentRef = useRef(current);
  currentRef.current = current;

  const show = () => {
    if (!open) returnFocus.current = document.activeElement as HTMLElement | null;
    setOpen(true);
    window.requestAnimationFrame(() => {
      input.current?.focus();
      input.current?.select();
    });
  };
  const close = () => {
    setOpen(false);
    clearHighlights();
    onClose?.();
    returnFocus.current?.focus?.();
  };
  const go = (dir: 'older' | 'newer') => {
    if (!open) show();
    const next = step(safeIndex, matches.length, dir);
    setTarget(next >= 0 ? matches[next]! : null);
  };

  useBinding({ id: 'chat.find', title: 'Find in chat', defaultKeys: 'Ctrl+F', run: show });
  useBinding({ id: 'chat.findOlder', title: 'Find: older match', defaultKeys: 'Ctrl+G', run: () => go('older'), enabled: () => open });
  useBinding({ id: 'chat.findNewer', title: 'Find: newer match', defaultKeys: 'Ctrl+Shift+G', run: () => go('newer'), enabled: () => open });
  useLayer('search', open, close);

  // Bring the current match's row into view.
  useEffect(() => {
    if (open && current) onJump(current.rowId);
  }, [open, current?.rowId, current?.occurrence]); // eslint-disable-line react-hooks/exhaustive-deps

  // Paint now and whenever the mounted rows change (scrolling, streaming, a fold opening). The view
  // is centred on the match only when the match itself changes, never on a repaint.
  useEffect(() => {
    const root = container.current;
    if (!open || !root || !debounced.trim()) {
      clearHighlights();
      return;
    }
    let frame = 0;
    let scrolled = false;
    const run = () => {
      frame = 0;
      const r = paint(root, debounced, currentRef.current);
      if (r && !scrolled) {
        scrolled = true;
        r.startContainer.parentElement?.scrollIntoView({ block: 'center' });
      }
    };
    run();
    const mo = new MutationObserver(() => {
      if (!frame) frame = window.requestAnimationFrame(run);
    });
    mo.observe(root, { childList: true, subtree: true, characterData: true });
    return () => {
      mo.disconnect();
      if (frame) window.cancelAnimationFrame(frame);
    };
  }, [open, debounced, current?.rowId, current?.occurrence, container]); // eslint-disable-line react-hooks/exhaustive-deps

  useEffect(() => () => clearHighlights(), []);

  if (!open) return null;
  return (
    <div className="find-bar" role="search" data-testid="find-bar">
      <div className="find-row">
        <IconSearch size={13} className="find-icon" />
        <input
          ref={input}
          type="text"
          className="find-input"
          data-testid="find-input"
          placeholder="Find in chat"
          aria-label="Find in chat"
          maxLength={FIND_MAX_QUERY}
          value={query}
          onChange={(e) => setQuery(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === 'Enter') {
              e.preventDefault();
              go(e.shiftKey ? 'newer' : 'older');
            }
          }}
        />
        <span className="find-count" data-testid="find-count" aria-live="polite">
          {debounced.trim() ? countLabel(safeIndex, matches.length, capped) : ''}
        </span>
        <button type="button" className="find-btn" title="Older match · Enter" aria-label="Older match" disabled={!matches.length} onClick={() => go('older')}>
          <IconChevronUp size={14} />
        </button>
        <button type="button" className="find-btn" title="Newer match · Shift Enter" aria-label="Newer match" disabled={!matches.length} onClick={() => go('newer')}>
          <IconChevronDown size={14} />
        </button>
        <button type="button" className="find-btn" title="Close · Esc" aria-label="Close find" onClick={close}>
          <IconX size={14} />
        </button>
      </div>
      <label className="find-activity">
        <input
          type="checkbox"
          checked={activity}
          onChange={(e) => {
            setActivity(e.target.checked);
            try {
              localStorage.setItem(ACTIVITY_KEY, e.target.checked ? '1' : '0');
            } catch {
              // not remembered
            }
          }}
        />
        Include activity <span className="find-hint">Tools, thinking, context</span>
      </label>
    </div>
  );
}
