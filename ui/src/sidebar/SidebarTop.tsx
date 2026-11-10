// The top of the sidebar: the 48 px title strip with the hide-sidebar button, the beacon (wheel
// mark, wordmark, chevron) that opens the Workspace status popover, and the tile row: New and
// Search, then Flagged, Tasks and Schedules in most-recently-used order, with More for the ones
// that do not fit.
//
// The hide button runs the shell's `sidebar.toggle` command (Ctrl+B) and is drawn only once that
// command is registered; the sidebar does not own its own width or visibility.

import { useEffect, useRef, useState, useSyncExternalStore } from 'react';
import type { Api } from '../api/client.ts';
import type { EventBus } from '../api/events.ts';
import type { AttentionRow, AttentionSummary } from '../api/types.ts';
import { dragHandlers } from '../topbar/caption.tsx';
import { keys, useChord, useLayer } from '../ui/index.ts';
import { IconBoard, IconChevronDown, IconClock, IconExternalLink, IconFlag, IconNewChat, IconSearch, IconSidebarClose, IconSuccess, IconX } from '../ui/icons.tsx';
import { WheelMark, Wordmark } from '../ui/status/index.ts';
import { countText, inboxCount } from '../attention/model.ts';
import { mayMove } from '../shell/motion.ts';
import { useSchedulesEntry } from '../schedules/SchedulesEntry.tsx';
import { useTasksEntry } from '../tasks/TasksEntry.tsx';
import { TileRow, type TileSpec } from './Tiles.tsx';
import { mergeOrder, orderViews, readOrder, touchView, writeOrder, DEFAULT_VIEW_ORDER, type ViewTileId } from './tiles.ts';

/** The shell command the hide button runs (registered by the shell with Ctrl+B). */
export const SIDEBAR_TOGGLE = 'sidebar.toggle';
/** Command ids whose chords the tile tooltips show. */
export const NEW_CHAT_COMMAND = 'chat.new';
export const SEARCH_COMMAND = 'search.toggle';

/** How long the brand wheel takes to turn once when it is clicked. */
export const WHEEL_SPIN_MS = 600;

export interface SidebarTopProps {
  api: Api;
  bus: EventBus;
  attention: AttentionSummary | null;
  /** The Flagged list is open: its tile stays pressed. */
  inboxOpen: boolean;
  onNewChat: () => void;
  onSearch?: () => void;
  onAttention: () => void;
  onOpenChat: (chatId: string) => void;
  openSettings?: () => void;
}

function useCommand(id: string): (() => void) | null {
  useSyncExternalStore(keys.subscribe, keys.getVersion);
  const b = keys.list().find((x) => x.id === id);
  return b?.run ?? null;
}

const tipWith = (name: string, chord: string | null | undefined) => (chord ? `${name} · ${chord}` : name);

/** The Flagged tile's count: the host's number, capped at 99+. */
export function flaggedBadge(attention: AttentionSummary | null): string {
  const n = inboxCount(attention);
  return n > 99 ? '99+' : countText(attention);
}

export function SidebarTop({ api, bus, attention, inboxOpen, onNewChat, onSearch, onAttention, onOpenChat, openSettings }: SidebarTopProps) {
  const hide = useCommand(SIDEBAR_TOGGLE);
  const hideChord = useChord(SIDEBAR_TOGGLE);
  const newChord = useChord(NEW_CHAT_COMMAND);
  const searchChord = useChord(SEARCH_COMMAND);
  const count = inboxCount(attention);
  const tasks = useTasksEntry(api, bus);
  const schedules = useSchedulesEntry(api, bus, openSettings ?? (() => undefined), onOpenChat);
  const [saved, setSaved] = useState<string[]>(() => readOrder());

  const lead: TileSpec[] = [
    { id: 'new', label: 'New', icon: <IconNewChat size={20} />, testId: 'new-chat', title: tipWith('New chat', newChord), onActivate: onNewChat },
    ...(onSearch ? [{ id: 'search', label: 'Search', icon: <IconSearch size={20} />, testId: 'nav-search', title: tipWith('Search chats', searchChord), onActivate: onSearch }] : []),
  ];
  const all: Record<ViewTileId, TileSpec | null> = {
    flagged: {
      id: 'flagged',
      label: 'Flagged',
      icon: <IconFlag size={20} />,
      testId: 'nav-attention',
      title: attention ? 'Flagged: unfinished work, independent of read state' : 'Checking for unfinished work…',
      onActivate: onAttention,
      badge: count > 0 ? { text: flaggedBadge(attention), label: `${count} need attention` } : null,
      pressed: inboxOpen,
      disabled: attention === null,
    },
    tasks: tasks.ready ? { id: 'tasks', label: 'Tasks', icon: <IconBoard size={20} />, testId: 'nav-tasks', title: 'Tasks', onActivate: tasks.open } : null,
    schedules: openSettings
      ? {
          id: 'schedules',
          label: 'Schedules',
          icon: <IconClock size={20} />,
          testId: 'nav-schedules',
          title: 'Schedules',
          onActivate: schedules.open,
          badge: schedules.waiting > 0 ? { text: String(schedules.waiting), label: `${schedules.waiting} reminders waiting`, testId: 'nav-schedules-count' } : null,
        }
      : null,
  };
  const available = DEFAULT_VIEW_ORDER.filter((id) => all[id] !== null);
  const order = orderViews(saved, available);
  const views = order.map((id) => all[id]!);
  const remember = (next: string[]) => {
    const full = mergeOrder(saved, next);
    setSaved(full);
    writeOrder(full);
  };

  return (
    <div className="sb-top">
      <div className="sb-titlestrip" data-testid="sidebar-titlestrip" {...dragHandlers()}>
        {hide && (
          <button
            type="button"
            className="sb-icon-btn"
            aria-label="Hide sidebar"
            title={hideChord ? `Hide sidebar · ${hideChord}` : 'Hide sidebar'}
            onClick={hide}
          >
            <IconSidebarClose size={16} />
          </button>
        )}
      </div>
      <Beacon attention={attention} onOpenChat={onOpenChat} />
      <TileRow lead={lead} views={views} onOrder={remember} onUsed={(id) => remember(touchView(order, id as ViewTileId))} />
    </div>
  );
}

/** The app's mark and wordmark with a chevron; opens Workspace status. The mark turns once on a click. */
function Beacon({ attention, onOpenChat }: { attention: AttentionSummary | null; onOpenChat: (chatId: string) => void }) {
  const [open, setOpen] = useState(false);
  const [spin, setSpin] = useState(0);
  const btn = useRef<HTMLButtonElement>(null);
  const pop = useRef<HTMLDivElement>(null);
  const close = () => {
    setOpen(false);
    btn.current?.focus();
  };
  useLayer('popover', open, () => {
    close();
  });
  useEffect(() => {
    if (!spin) return;
    const t = setTimeout(() => setSpin(0), WHEEL_SPIN_MS);
    return () => clearTimeout(t);
  }, [spin]);
  useEffect(() => {
    if (!open) return;
    pop.current?.focus();
    const away = (e: Event) => {
      const t = e.target as Node | null;
      if (t && (pop.current?.contains(t) || btn.current?.contains(t))) return;
      setOpen(false);
    };
    const shut = () => setOpen(false);
    window.addEventListener('pointerdown', away, true);
    window.addEventListener('focusin', away, true);
    window.addEventListener('blur', shut);
    window.addEventListener('resize', shut);
    return () => {
      window.removeEventListener('pointerdown', away, true);
      window.removeEventListener('focusin', away, true);
      window.removeEventListener('blur', shut);
      window.removeEventListener('resize', shut);
    };
  }, [open]);
  return (
    <div className="sb-beacon-wrap">
      <button
        ref={btn}
        type="button"
        className="sb-beacon"
        data-testid="sidebar-beacon"
        aria-label="Workspace status"
        aria-haspopup="dialog"
        aria-expanded={open}
        onClick={() => {
          setOpen((v) => !v);
          if (mayMove()) setSpin((n) => n + 1);
        }}
      >
        <span className="sb-beacon-wheel" data-spin={spin > 0 ? 'true' : undefined}>
          <WheelMark size={23} />
        </span>
        <Wordmark className="sb-wordmark" />
        <IconChevronDown size={12} className="sb-beacon-chevron" />
      </button>
      {open && (
        <div ref={pop} className="sb-status" role="dialog" aria-label="Workspace status" tabIndex={-1} data-testid="workspace-status">
          <WorkspaceStatus
            attention={attention}
            onClose={close}
            onOpen={(id) => {
              setOpen(false);
              onOpenChat(id);
            }}
          />
        </div>
      )}
    </div>
  );
}

const FIRST = 8;

function WorkspaceStatus({ attention, onClose, onOpen }: { attention: AttentionSummary | null; onClose: () => void; onOpen: (chatId: string) => void }) {
  const counts: [string, number | null][] = [
    ['Running', attention ? attention.busy : null],
    ['Needs input', attention ? attention.needsYou.length : null],
    ['Unread', attention ? attention.unread.length : null],
  ];
  const sections: { key: string; title: string; rows: AttentionRow[]; state: (r: AttentionRow) => string }[] = attention
    ? [
        { key: 'input', title: 'Needs your input', rows: attention.needsYou, state: () => 'Needs input' },
        { key: 'progress', title: 'In progress', rows: attention.running, state: () => 'Running' },
        { key: 'ready', title: 'Ready to open', rows: attention.unread, state: () => 'Finished' },
      ].filter((s) => s.rows.length > 0)
    : [];
  return (
    <>
      <div className="sb-status-head">
        <span>Workspace status</span>
        <button type="button" className="sb-icon-btn" aria-label="Close" onClick={onClose}>
          <IconX size={13} />
        </button>
      </div>
      <div className="sb-status-counts">
        {counts.map(([label, n]) => (
          <div key={label} className="sb-status-count">
            <span className="sb-status-num">{n === null ? '—' : n}</span>
            <span className="sb-status-label">{label}</span>
          </div>
        ))}
      </div>
      {!attention && <p className="sb-status-note">Checking tasks…</p>}
      {attention && sections.length === 0 && (
        <div className="sb-status-quiet">
          <IconSuccess size={20} />
          <strong>All quiet</strong>
          <span>New results and active tasks appear here.</span>
        </div>
      )}
      {sections.map((s) => (
        <section key={s.key} className="sb-status-section">
          <h3>{s.title}</h3>
          {s.rows.slice(0, FIRST).map((r) => (
            <button key={r.chatId} type="button" className="sb-status-row" onClick={() => onOpen(r.chatId)}>
              <span className="sb-status-text">
                <span className="sb-status-title">{r.title || 'Untitled'}</span>
                <span className="sb-status-sub">
                  {r.project} · {s.state(r)}
                </span>
              </span>
              <IconExternalLink size={13} className="sb-status-arrow" />
            </button>
          ))}
        </section>
      ))}
      <div className="sb-status-foot">
        <span>
          <span className="sb-status-dot" aria-hidden="true" /> On this machine
        </span>
        <span>Esc to close</span>
      </div>
    </>
  );
}
