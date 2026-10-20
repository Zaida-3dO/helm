// The sidebar: one grouped tree of
// folder and board projects with their chats, virtualised, with the Projects / Activity / Archived
// views, menus, drag and drop, hover preview and undo. Its test ids stay stable: `new-chat`,
// `chat-item`.

import { useCallback, useEffect, useLayoutEffect, useMemo, useRef, useState, useSyncExternalStore, type PointerEvent as ReactPointerEvent } from 'react';
import type { Api } from '../api/client.ts';
import type { EventBus } from '../api/events.ts';
import type { ChatOps } from '../api/ops.ts';
import { projectOps, type BoardSyncState, type ProjectNode, type SidebarRow, type SidebarView } from '../api/projects.ts';
import type { RowPreview } from '../api/generated/projects/index.ts';
import type { AttentionSummary } from '../api/types.ts';
import type { ChatStore } from '../state/store.ts';
import { SETTINGS_DIALOG } from '../settings/index.ts';
import { SEARCH_DIALOG } from '../search/index.ts';
import { AttentionInbox, inboxRows, nextAttention } from '../attention/index.ts';
import { useSelection, useShell } from '../shell/context.ts';
import { Button, ContextMenu, Dialog, TextInput, useBinding, type MenuItem } from '../ui/index.ts';
import { SidebarActions, compactChat } from './actions.ts';
import { SidebarData, errorText } from './data.ts';
import { dropAction, passedThreshold, type DragSource, type DropTarget } from './drag.ts';
import { chatMenuItems, projectMenuItems } from './menus.ts';
import { buildList, chatAncestors, visibleChatOrder, type ListItem } from './model.ts';
import { setVisibleOrder } from './rowOrder.ts';
import { SidebarTop } from './SidebarTop.tsx';
import { ItemChip, ItemFolderRow, ItemPickerHost, useItemRefs } from '../itemlinks/ItemUi.tsx'; // item links
import {
  IconArchive,
  IconBoard,
  IconChevronDown,
  IconChevronRight,
  IconFlag,
  IconFolder,
  IconGrip,
  IconMore,
  IconMute,
  IconPin,
  IconPlus,
  IconQuit,
  IconRename,
  IconRestore,
  IconSettings,
  IconX,
} from '../ui/icons.tsx';
import { withChatIcons, withProjectIcons } from './menuIcons.tsx';
import { StarMark, WheelMark } from '../ui/status/index.ts';
import { openNewChat } from '../newchat/index.ts';
import { ATTENTION_LABEL, age, trailingIndicator } from './status.ts';
import { useSubagentsRunning } from '../transcript/subagents/counts.ts';
import { startSyncTriggers, syncDisplay, type SyncHost } from './sync.ts';
import { sidebarUndo } from './undo.ts';
import { layout, visibleRange } from './virtual.ts';
import './sidebar.css';

/** Whether projects with no chats are shown; remembered on this machine. */
export const SHOW_EMPTY_KEY = 'helm.sidebar.showEmptyProjects';
export const PREVIEW_DELAY_MS = 300;

export interface SidebarProps {
  api: Api;
  bus: EventBus;
  store: ChatStore;
  ops: ChatOps;
  selected: string | null;
  onSelect: (chatId: string) => void;
  openSettings?: () => void;
  openSearch?: () => void;
}

/** The frame's mount: everything comes from the shell context. */
export function Sidebar() {
  const shell = useShell();
  const { chatId } = useSelection();
  return (
    <SidebarView
      api={shell.api}
      bus={shell.bus}
      store={shell.store}
      ops={shell.ops}
      selected={chatId}
      onSelect={(id) => shell.select(id)}
      openSettings={() => shell.openDialog(SETTINGS_DIALOG)}
      openSearch={() => shell.openDialog(SEARCH_DIALOG)}
    />
  );
}

/** A project colour as CSS: a named colour reads its design token, a custom one is the stored hex. */
export function colourCss(c: string | null | undefined): string | null {
  if (!c) return null;
  return c.startsWith('#') ? c : `var(--project-${c})`;
}

type Menu = { x: number; y: number; items: MenuItem[]; label: string };
export type Prompt = { title: string; label: string; value: string; onSave: (v: string) => void };
type Preview = { row: SidebarRow; top: number; left: number };
type Drag = { src: DragSource; x0: number; y0: number; active: boolean; title: string; target: DropTarget | null; x: number; y: number };

export function SidebarView({ api, bus, store, ops, selected, onSelect, openSettings, openSearch }: SidebarProps) {
  const pops = useMemo(() => projectOps(api), [api]);
  const data = useMemo(() => new SidebarData(pops), [pops]);
  const snap = useSyncExternalStore(data.subscribe, data.getSnapshot);
  const [view, setView] = useState<SidebarView>('projects');
  const [error, setError] = useState<string | null>(null);
  const [expanded, setExpanded] = useState<Set<string>>(() => new Set());
  const [collapsedItems, setCollapsedItems] = useState<Set<string>>(() => new Set());
  const [compact, setCompact] = useState(false);
  const [showEmpty, setShowEmpty] = useState(() => localStorage.getItem(SHOW_EMPTY_KEY) === 'true');
  const toggleEmpty = () =>
    setShowEmpty((v) => {
      localStorage.setItem(SHOW_EMPTY_KEY, String(!v));
      return !v;
    });
  const [menu, setMenu] = useState<Menu | null>(null);
  const [prompt, setPrompt] = useState<Prompt | null>(null);
  const [preview, setPreview] = useState<Preview | null>(null);
  const [drag, setDrag] = useState<Drag | null>(null);
  const [inboxOpen, setInboxOpen] = useState(false);
  const undo = sidebarUndo;
  const act = useMemo(() => new SidebarActions({ api, pops, ops, data, undo, onError: setError }), [api, pops, ops, data, undo]);
  const now = useNow();

  useEffect(() => data.connect(bus), [data, bus]);
  useEffect(() => {
    void data.loadProjects();
    void data.page('projects');
    ops
      .attention()
      .then((s: AttentionSummary) => data.attention(s))
      .catch(() => undefined);
    const readCompact = () =>
      api<unknown>('preference_get', { key: 'compactSidebar' })
        .then((v) => setCompact(v === true))
        .catch(() => undefined);
    void readCompact();
    return bus.on('preferences', () => void readCompact());
  }, [data, api, ops, bus]);
  useEffect(() => {
    if (!snap.paging[view].started) void data.page(view);
  }, [view, data, snap.paging]);

  // Agent Standup sync triggers. A failure only shows in the status line.
  const runSync = useCallback(() => {
    pops
      .sync()
      .then((s) => {
        data.setSync(s);
        void data.loadProjects();
      })
      .catch(() => undefined);
  }, [pops, data]);
  useEffect(
    () =>
      startSyncTriggers({
        sync: runSync,
        state: () => data.snapshot.sync,
        host: window as unknown as SyncHost,
        visible: () => document.visibilityState === 'visible',
      }),
    [runSync, data],
  );

  // The open chat's ancestors expand so it is visible, unless the user collapsed them this session.
  const userCollapsed = useRef(new Set<string>());
  useEffect(() => {
    if (!selected) return;
    const up = chatAncestors(snap.rows, selected).filter((id) => !userCollapsed.current.has(id));
    if (up.some((id) => !expanded.has(id))) setExpanded((s) => new Set([...s, ...up]));
  }, [selected, snap.rows, expanded]);

  const itemRefs = useItemRefs(useMemo(() => snap.rows.map((r) => r.chatId), [snap.rows]));
  const items = useMemo(
    () =>
      buildList(snap.projects, snap.rows, {
        view,
        compact,
        expandedChats: expanded,
        selectedChatId: selected,
        itemLinks: view === 'activity' ? undefined : { refs: itemRefs, collapsed: collapsedItems },
        showEmpty,
      }),
    [snap.projects, snap.rows, view, compact, expanded, selected, itemRefs, collapsedItems, showEmpty],
  );
  useEffect(() => setVisibleOrder(visibleChatOrder(items)), [items]);
  const projectsById = useMemo(() => new Map(snap.projects.map((p) => [p.id, p])), [snap.projects]);
  const groupDepth = useMemo(() => {
    const m = new Map<string, number>();
    for (const it of items) if (it.kind === 'header') m.set(it.project.id, it.depth);
    return m;
  }, [items]);

  // a project's "+" and New chat open the new-chat page in that project; nothing is
  // created until the first send.
  const newChatIn = (p: ProjectNode) => openNewChat({ projectId: p.id });

  // The nav's New chat: the new-chat page, in the open chat's project when one is open.
  const navNewChat = () => {
    const projectId = selectedRow?.groupProjectId;
    openNewChat(projectId ? { projectId } : {});
  };

  const toggleChat = (id: string) =>
    setExpanded((s) => {
      const n = new Set(s);
      if (n.has(id)) {
        n.delete(id);
        userCollapsed.current.add(id);
      } else {
        n.add(id);
        userCollapsed.current.delete(id);
      }
      return n;
    });

  // ---- menus ----
  const rowMenuItems = (row: SidebarRow): MenuItem[] => {
    const siblings = snap.rows.filter((r) => r.groupProjectId === row.groupProjectId && r.archived === row.archived);
    const byId = new Map(siblings.map((r) => [r.chatId, r]));
    const depthOf = (id: string) => {
      let d = 0;
      let cur = byId.get(id)?.parentChatId;
      const seen = new Set([id]);
      while (cur && byId.has(cur) && !seen.has(cur)) {
        seen.add(cur);
        d++;
        cur = byId.get(cur)!.parentChatId;
      }
      return d;
    };
    const heightOf = (id: string): number => {
      const kids = siblings.filter((r) => r.parentChatId === id);
      return kids.length === 0 ? 0 : 1 + Math.max(...kids.map((k) => heightOf(k.chatId)));
    };
    return withChatIcons(chatMenuItems(
      { row, projects: snap.projects, siblings, heightOf, depthOf, expanded: expanded.has(row.chatId), view },
      {
        fork: () => void act.fork(row),
        rename: () => setPrompt({ title: 'Rename chat', label: 'Title', value: row.title, onSave: (t) => void act.rename(row, t) }),
        compact: () => void compactChat(ops, row.chatId, setError),
        moveTo: (id) => void act.moveTo(row, id),
        nestUnder: (id) => {
          userCollapsed.current.delete(id);
          setExpanded((s) => new Set(s).add(id));
          void act.nestUnder(row, id);
        },
        topLevel: () => void act.topLevel(row),
        toggleChildren: () => toggleChat(row.chatId),
        markUnread: () => void act.markUnread(row),
        pin: (p) => void act.pin(row, p),
        mute: (m) => void act.mute(row, m),
        archive: () => void act.archive(row),
        restore: () => void act.restore(row),
        remove: () => void act.remove(row),
      },
    ));
  };
  const projectMenu = (p: ProjectNode): MenuItem[] =>
    withProjectIcons(projectMenuItems(p, snap.projects, {
      newChat: () => newChatIn(p),
      pin: (pinned) => void act.updateProject(p, { pinned }),
      rename: () => setPrompt({ title: 'Rename project', label: 'Name', value: p.name, onSave: (name) => void act.updateProject(p, { name }) }),
      changeIcon: () =>
        setPrompt({
          title: 'Project icon',
          label: 'One emoji (empty for the default icon)',
          value: p.icon ?? '',
          onSave: (v) => void act.updateProject(p, { icon: v.trim() ? v.trim() : null }),
        }),
      nestUnder: (id) => void act.nestProject(p, id),
      topLevel: () => void act.nestProject(p, null),
      archive: () => void act.archiveProject(p),
      restore: () => void act.restoreProject(p),
      colour: (c) => void act.updateProject(p, { colour: c }),
    }), p);
  const openMenu = (x: number, y: number, items: MenuItem[], label: string) => {
    setPreview(null);
    setMenu({ x, y, items, label });
  };

  // ---- shortcuts (registered with the shell's key registry, never a keydown handler) ----
  const selectedRow = selected ? snap.rows.find((r) => r.chatId === selected) : undefined;
  useBinding({ id: 'sidebar.undo', title: 'Undo archive or move', defaultKeys: 'Mod+Z', inInputs: false, run: () => void act.undo() });
  useBinding({ id: 'sidebar.redo', title: 'Redo archive or move', defaultKeys: 'Mod+Shift+Z', inInputs: false, run: () => void act.redo() });
  useBinding({ id: 'chat.pin', title: 'Pin or unpin chat', defaultKeys: 'Mod+Alt+P', enabled: () => !!selectedRow, run: () => selectedRow && void act.pin(selectedRow, !selectedRow.pinned) });
  useBinding({ id: 'chat.markUnread', title: 'Mark chat as unread', defaultKeys: 'Mod+Shift+U', enabled: () => !!selectedRow, run: () => selectedRow && void act.markUnread(selectedRow) });
  useBinding({
    id: 'attention.next',
    title: 'Next chat needing attention',
    defaultKeys: 'Ctrl+Alt+A',
    // AltGr arrives as Ctrl+Alt on Windows: never take a typed character from a text field.
    inInputs: false,
    run: () => {
      const id = nextAttention(inboxRows(data.snapshot.attention), selected);
      if (id) onSelect(id);
    },
  });
  useBinding({ id: 'chat.archive', title: 'Archive chat', defaultKeys: 'Mod+Shift+Backspace', enabled: () => !!selectedRow && !selectedRow.archived, run: () => selectedRow && void act.archive(selectedRow) });

  // ---- hover preview (300 ms) ----
  const previewTimer = useRef<ReturnType<typeof setTimeout> | undefined>(undefined);
  const scrolling = useRef(0);
  const hoverRow = (row: SidebarRow, el: HTMLElement | null) => {
    clearTimeout(previewTimer.current);
    // No preview for the chat that is already open, or while dragging or a menu is up.
    if (!el || drag || menu || row.chatId === selected) return;
    previewTimer.current = setTimeout(() => {
      if (Date.now() - scrolling.current < 180) return;
      const r = el.getBoundingClientRect();
      setPreview({ row, top: Math.max(12, r.top - 12), left: Math.min(r.right + 14, innerWidth - 12 - Math.min(480, innerWidth - 24)) });
    }, PREVIEW_DELAY_MS);
  };
  useEffect(() => {
    const close = () => {
      clearTimeout(previewTimer.current);
      setPreview(null);
    };
    window.addEventListener('blur', close);
    return () => window.removeEventListener('blur', close);
  }, []);
  const leaveRow = () => {
    clearTimeout(previewTimer.current);
    previewTimer.current = setTimeout(() => setPreview(null), PREVIEW_DELAY_MS);
  };

  // ---- drag and drop ----
  const dragRef = useRef<Drag | null>(null);
  dragRef.current = drag;
  const suppressClick = useRef(false);
  const startDrag = (e: ReactPointerEvent, src: DragSource, title: string) => {
    if (e.button !== 0 || e.ctrlKey || e.pointerType === 'touch' || view === 'activity') return;
    setDrag({ src, title, x0: e.clientX, y0: e.clientY, x: e.clientX, y: e.clientY, active: false, target: null });
  };
  useEffect(() => {
    if (!drag) return;
    const targetAt = (x: number, y: number): DropTarget | null => {
      const el = document.elementFromPoint(x, y) as HTMLElement | null;
      const header = el?.closest<HTMLElement>('[data-testid="sidebar-group"]');
      const chat = el?.closest<HTMLElement>('.sb-chat-wrap');
      const lowerOf = (n: HTMLElement) => {
        const r = n.getBoundingClientRect();
        return y > r.top + r.height / 2;
      };
      if (header) {
        const p = projectsById.get(header.dataset.projectId ?? '');
        if (!p) return null;
        return { kind: 'header', projectId: p.id, parentId: p.parentId, pinned: p.pinned, lowerHalf: lowerOf(header) };
      }
      if (chat) {
        const row = data.row(chat.dataset.chatId ?? '');
        if (!row) return null;
        return { kind: 'chat', chatId: row.chatId, groupId: row.groupProjectId, pinned: row.pinned, lowerHalf: lowerOf(chat) };
      }
      return null;
    };
    const move = (e: PointerEvent) => {
      const d = dragRef.current;
      if (!d) return;
      const active = d.active || passedThreshold(e.clientX - d.x0, e.clientY - d.y0);
      setDrag({ ...d, active, x: e.clientX, y: e.clientY, target: active ? targetAt(e.clientX, e.clientY) : null });
      if (active) {
        setPreview(null);
        // Auto-scroll within 32 px of the list's edges, up to 12 px a frame.
        const list = listRef.current;
        if (list) {
          const r = list.getBoundingClientRect();
          if (e.clientY < r.top + 32) list.scrollTop -= Math.min(12, (r.top + 32 - e.clientY) / 3 + 1);
          else if (e.clientY > r.bottom - 32) list.scrollTop += Math.min(12, (e.clientY - r.bottom + 32) / 3 + 1);
        }
      }
    };
    const up = () => {
      const d = dragRef.current;
      setDrag(null);
      if (!d?.active) return;
      suppressClick.current = true;
      setTimeout(() => (suppressClick.current = false), 0);
      const a = d.target ? dropAction(d.src, d.target) : null;
      if (!a) return;
      if (a.kind === 'chat-to-group' || a.kind === 'chat-before' || a.kind === 'chat-nest') {
        const row = data.row(a.chatId);
        if (!row) return;
        if (a.kind === 'chat-to-group') void act.moveTo(row, a.groupId);
        else if (a.kind === 'chat-before') void act.reorderBefore(row, a.beforeChatId, a.moveGroup ? a.groupId : undefined);
        else {
          userCollapsed.current.delete(a.parentChatId);
          setExpanded((s) => new Set(s).add(a.parentChatId));
          void act.nestUnder(row, a.parentChatId, a.moveGroup ? a.groupId : undefined);
        }
      } else {
        const p = projectsById.get(a.projectId);
        if (!p) return;
        if (a.kind === 'project-nest') void act.nestProject(p, a.parentId);
        else void act.reorderProject(p, a.beforeProjectId);
      }
    };
    const cancel = (e: Event) => {
      if (e instanceof KeyboardEvent && e.key !== 'Escape') return;
      setDrag(null);
    };
    window.addEventListener('pointermove', move);
    window.addEventListener('pointerup', up);
    window.addEventListener('blur', cancel);
    window.addEventListener('keydown', cancel);
    window.addEventListener('resize', cancel);
    return () => {
      window.removeEventListener('pointermove', move);
      window.removeEventListener('pointerup', up);
      window.removeEventListener('blur', cancel);
      window.removeEventListener('keydown', cancel);
      window.removeEventListener('resize', cancel);
    };
  }, [drag !== null, act, data, projectsById]);

  const paging = snap.paging[view];
  const switchView = (v: SidebarView) => {
    setView(v);
    setMenu(null);
    setPreview(null);
    listRef.current?.scrollTo({ top: 0 });
  };

  // ---- virtual list ----
  const listRef = useRef<HTMLDivElement | null>(null);
  const [scrollTop, setScrollTop] = useState(0);
  const [viewport, setViewport] = useState(600);
  useLayoutEffect(() => {
    const el = listRef.current;
    if (!el) return;
    const ro = new ResizeObserver(() => setViewport(el.clientHeight));
    ro.observe(el);
    setViewport(el.clientHeight);
    return () => ro.disconnect();
  }, []);
  const heights = useMemo(() => items.map((i) => i.height), [items]);
  const lay = useMemo(() => layout(heights), [heights]);
  const [start, end] = visibleRange(lay, heights, scrollTop, viewport);

  // After a new chat or any selection, the open chat's row scrolls into view if it is out of view,
  // so it is mounted and findable.
  const selectedIndex = selected ? items.findIndex((i) => i.kind === 'chat' && i.row.chatId === selected) : -1;
  useEffect(() => {
    const el = listRef.current;
    if (!el || selectedIndex < 0) return;
    const top = lay.offsets[selectedIndex]!;
    const bottom = top + heights[selectedIndex]!;
    if (top < el.scrollTop) el.scrollTop = top;
    else if (bottom > el.scrollTop + el.clientHeight) el.scrollTop = bottom - el.clientHeight;
  }, [selected, selectedIndex]);

  const dropKey = drag?.active && drag.target ? (drag.target.kind === 'header' ? `h:${drag.target.projectId}` : drag.target.chatId) : null;
  const dropOk = drag?.active && drag.target ? dropAction(drag.src, drag.target) !== null : false;

  return (
    <div
      className="sb-root"
      data-view={view}
      data-compact={compact ? 'true' : 'false'}
      data-dragging={drag?.active ? 'true' : undefined}
      onContextMenu={(e) => {
        if (!(e.target as HTMLElement).closest('input, textarea')) e.preventDefault();
      }}
    >
      <SidebarTop
        attention={snap.attention}
        onNewChat={navNewChat}
        onSearch={openSearch}
        onAttention={() => setInboxOpen(true)}
        onOpenChat={onSelect}
      />
      {(error ?? snap.error) && (
        <p className="sb-error" role="alert">
          {error ?? snap.error}
          <button type="button" className="sb-error-x" aria-label="Dismiss" onClick={() => setError(null)}>
            <IconX size={12} />
          </button>
        </p>
      )}
      <div className="sb-section">
        {view === 'archived' ? (
          <span>Archived chats</span>
        ) : (
          <div className="sb-switch" role="group" aria-label="Sidebar view">
            <button type="button" aria-pressed={view === 'projects'} title="Chats grouped by project" onClick={() => switchView('projects')}>
              Projects
            </button>
            <button
              type="button"
              aria-pressed={view === 'activity'}
              title="Recent chat activity across all projects, newest first"
              onClick={() => switchView('activity')}
            >
              Activity
            </button>
          </div>
        )}
      </div>
      <BoardSyncLine state={snap.sync} now={now} onRefresh={runSync} />
      <div
        className="sb-list"
        ref={listRef}
        tabIndex={0}
        aria-label={view === 'activity' ? 'Recent activity' : 'Projects and chats'}
        onScroll={(e) => {
          scrolling.current = Date.now();
          setPreview(null);
          setScrollTop(e.currentTarget.scrollTop);
        }}
      >
        <div className="sb-list-inner" style={{ height: lay.total }}>
          {items.slice(start, end).map((it, k) => (
            <div
              key={it.key}
              className="sb-slot"
              data-drop={dropKey === it.key ? (dropOk ? (drag?.target?.lowerHalf ? 'into' : 'before') : 'no') : undefined}
              style={{ height: it.height, transform: `translateY(${lay.offsets[start + k]}px)` }}
            >
              {renderItem(it)}
            </div>
          ))}
        </div>
        {items.length === 0 && !paging.loading && (
          <div className="sb-empty">
            {view === 'activity' ? 'No recent activity' : view === 'archived' ? 'No archived chats' : 'Add your first project'}
          </div>
        )}
        {(!paging.complete || paging.error) && paging.started && (
          <button type="button" className="sb-more" disabled={paging.loading} onClick={() => void data.page(view)}>
            {paging.loading ? 'Loading chats…' : paging.error ? 'Retry' : 'Load more chats'}
          </button>
        )}
      </div>
      <div className="sb-footer">
        <button
          type="button"
          className="sb-foot"
          aria-pressed={view === 'archived'}
          onClick={() => switchView(view === 'archived' ? 'projects' : 'archived')}
        >
          <IconArchive />
          <span>{view === 'archived' ? 'Back to projects' : 'Archived chats'}</span>
        </button>
        {openSettings && (
          <button type="button" className="sb-foot" onClick={openSettings}>
            <IconSettings />
            <span>Settings</span>
          </button>
        )}
        <button type="button" className="sb-foot" onClick={() => void api('quit_request')}>
          <IconQuit />
          <span>Quit</span>
        </button>
      </div>
      {menu && <ContextMenu x={menu.x} y={menu.y} items={menu.items} label={menu.label} onClose={() => setMenu(null)} testId="sidebar-menu" />}
      {prompt && <PromptDialog prompt={prompt} onClose={() => setPrompt(null)} />}
      {inboxOpen && <AttentionInbox summary={snap.attention} ops={ops} onOpen={onSelect} onClose={() => setInboxOpen(false)} />}
      <ItemPickerHost />
      {preview && !menu && !drag && (
        <PreviewCard
          preview={preview}
          project={projectsById.get(preview.row.groupProjectId)}
          now={now}
          load={(id) => pops.rows([id]).then((rs) => rs[0]?.preview ?? null)}
          onEnter={() => clearTimeout(previewTimer.current)}
          onLeave={leaveRow}
          onOpen={() => {
            setPreview(null);
            onSelect(preview.row.chatId);
          }}
        />
      )}
      {drag?.active && (
        <div className="sb-ghost" style={{ left: drag.x + 8, top: drag.y - 2 }} aria-hidden="true">
          <IconGrip size={12} className="sb-ghost-grip" />
          {drag.title.slice(0, 180)}
        </div>
      )}
    </div>
  );

  function renderItem(it: ListItem) {
    if (it.kind === 'pinBreak') return <div className="sb-pin-break" aria-hidden="true" />;
    if (it.kind === 'emptyToggle') {
      const n = `${it.count} empty project${it.count === 1 ? '' : 's'}`;
      return (
        <button
          type="button"
          className="sb-empty-toggle"
          data-testid="sidebar-empty-toggle"
          aria-pressed={it.shown}
          title={it.shown ? 'Hide projects that have no chats' : 'Projects with no chats are hidden'}
          onClick={toggleEmpty}
        >
          {it.shown ? `Hide ${n}` : `Show ${n}`}
        </button>
      );
    }
    if (it.kind === 'itemFolder') {
      return (
        <ItemFolderRow
          itemId={it.itemId}
          title={it.title}
          members={it.members}
          collapsed={it.collapsed}
          indent={(groupDepth.get(it.key.slice(2, it.key.indexOf(':', 2))) ?? 0) * 16 + it.depth * 16}
          onToggle={() =>
            setCollapsedItems((s) => {
              const n = new Set(s);
              if (!n.delete(it.itemId)) n.add(it.itemId);
              return n;
            })
          }
        />
      );
    }
    if (it.kind === 'header') {
      const p = it.project;
      return (
        <GroupHeader
          item={it}
          dragging={drag?.active === true && drag.src.kind === 'project' && drag.src.projectId === p.id}
          current={selected ? data.row(selected)?.groupProjectId === p.id : false}
          onToggle={() => {
            if (suppressClick.current) return;
            void act.updateProject(p, { collapsed: !it.collapsed });
          }}
          onPin={() => void act.updateProject(p, { pinned: !p.pinned })}
          onNew={() => newChatIn(p)}
          onRestore={() => void act.restoreProject(p)}
          onMenu={(x, y) => openMenu(x, y, projectMenu(p), `Options for ${p.name}`)}
          onPointerDown={(e) => startDrag(e, { kind: 'project', projectId: p.id, parentId: p.parentId, pinned: p.pinned }, p.name)}
        />
      );
    }
    const row = it.row;
    return (
      <ChatRow
        item={it}
        indent={view === 'activity' ? 0 : (groupDepth.get(row.groupProjectId) ?? 0) * 16}
        project={projectsById.get(row.groupProjectId)}
        selected={row.chatId === selected}
        needsYou={snap.needsYou.has(row.chatId)}
        dragging={drag?.active === true && drag.src.kind === 'chat' && drag.src.chatId === row.chatId}
        now={now}
        onOpen={() => {
          if (suppressClick.current) return;
          clearTimeout(previewTimer.current);
          setPreview(null);
          onSelect(row.chatId);
        }}
        onToggle={() => toggleChat(row.chatId)}
        onMenu={(x, y) => openMenu(x, y, rowMenuItems(row), `Options for ${row.title}`)}
        onArchive={() => void (row.archived ? act.restore(row) : act.archive(row))}
        onHover={(el) => hoverRow(row, el)}
        onLeave={leaveRow}
        onPointerDown={(e) => {
          clearTimeout(previewTimer.current);
          setPreview(null);
          startDrag(e, { kind: 'chat', chatId: row.chatId, groupId: row.groupProjectId, pinned: row.pinned }, row.title);
        }}
      />
    );
  }
}

function BoardSyncLine({ state, now, onRefresh }: { state: BoardSyncState | null; now: number; onRefresh: () => void }) {
  const d = syncDisplay(state);
  const text =
    d === 'syncing'
      ? 'Syncing Agent Standup…'
      : d === 'error'
        ? state?.error
        : d === 'ok' && state?.lastSyncAt
          ? `Agent Standup synced ${age(state.lastSyncAt, now)}${age(state.lastSyncAt, now) === 'now' ? '' : ' ago'}`
          : null;
  return (
    <div className="sb-sync" data-testid="board-sync" data-state={d} hidden={d === 'unconfigured' || d === 'never'}>
      <span className="sb-sync-text" role="status">
        {text}
      </span>
      {d !== 'syncing' && (
        <button type="button" className="sb-sync-refresh" onClick={onRefresh}>
          Refresh
        </button>
      )}
    </div>
  );
}

function GroupHeader({
  item,
  current,
  dragging,
  onToggle,
  onPin,
  onNew,
  onRestore,
  onMenu,
  onPointerDown,
}: {
  item: Extract<ListItem, { kind: 'header' }>;
  current: boolean;
  dragging: boolean;
  onToggle: () => void;
  onPin: () => void;
  onNew: () => void;
  onRestore: () => void;
  onMenu: (x: number, y: number) => void;
  onPointerDown: (e: ReactPointerEvent) => void;
}) {
  const p = item.project;
  const colour = colourCss(p.colour);
  return (
    <div
      className="sb-header"
      data-testid="sidebar-group"
      data-project-id={p.id}
      data-kind={p.kind}
      data-depth={item.depth}
      data-current={current ? 'true' : undefined}
      data-coloured={colour ? 'true' : undefined}
      data-subtitle={item.subtitle ? 'true' : undefined}
      data-drag-source={dragging ? 'true' : undefined}
      style={{ marginLeft: item.depth * 16, ...(colour ? { ['--group-colour' as string]: colour } : {}) }}
      title={[p.path ?? p.board?.title ?? p.name, p.board?.editedLocally ? 'Edited in Helm' : null, 'Drag to reorder or nest']
        .filter(Boolean)
        .join('\n')}
      onContextMenu={(e) => {
        e.preventDefault();
        onMenu(e.clientX, e.clientY);
      }}
    >
      <div className="sb-header-line">
        <button
          type="button"
          className="sb-toggle"
          aria-label={`${item.collapsed ? 'Expand' : 'Collapse'} ${p.name}`}
          aria-expanded={!item.collapsed}
          onClick={onToggle}
        >
          {item.collapsed ? <IconChevronRight size={11} strokeWidth={2.4} /> : <IconChevronDown size={11} strokeWidth={2.4} />}
        </button>
        <button type="button" className="sb-name" onClick={onToggle} onPointerDown={onPointerDown} aria-expanded={!item.collapsed}>
          <span className="sb-icon" aria-hidden="true">
            {p.icon && !p.icon.startsWith('lib:') ? p.icon : p.kind === 'board' ? <IconBoard size={16} strokeWidth={1.8} /> : <IconFolder size={16} strokeWidth={1.8} />}
          </span>
          <span className="sb-title">{p.name}</span>
          {p.archived && (
            <span className="sb-mark" role="img" aria-label="Archived project" title="Archived project">
              <IconArchive size={12} />
            </span>
          )}
          {p.board?.editedLocally && (
            <span className="sb-edited" role="img" title="Edited in Helm" aria-label="Edited in Helm">
              <IconRename size={11} />
            </span>
          )}
          <span className="sb-count" title={`${p.chatCount} non-archived chats`}>
            {p.chatCount}
          </span>
        </button>
        <span className="sb-actions">
          {!p.archived && (
            <button
              type="button"
              className="sb-act sb-pin"
              aria-label={`${p.pinned ? 'Unpin' : 'Pin'} project ${p.name}`}
              aria-pressed={p.pinned}
              title={p.pinned ? 'Unpin project' : 'Pin project'}
              onClick={onPin}
            >
              <IconPin filled={p.pinned} />
            </button>
          )}
          <button
            type="button"
            className="sb-act"
            aria-label={`Options for ${p.name}`}
            title="Project options"
            onClick={(e) => {
              const r = e.currentTarget.getBoundingClientRect();
              onMenu(r.right, r.bottom);
            }}
          >
            <IconMore />
          </button>
          {p.archived ? (
            <button type="button" className="sb-act" aria-label={`Restore project ${p.name}`} title="Restore project and its chats" onClick={onRestore}>
              <IconRestore />
            </button>
          ) : (
            <button type="button" className="sb-act" aria-label={`New chat in ${p.name}`} title="New chat" onClick={onNew}>
              <IconPlus />
            </button>
          )}
        </span>
      </div>
      {item.subtitle && (
        <span className="sb-subtitle" title={item.subtitle}>
          {item.subtitle}
        </span>
      )}
    </div>
  );
}

function Indicator({ row }: { row: SidebarRow }) {
  const a = row.attention;
  if (!a) return <span className="sb-ind" aria-hidden="true" />;
  const label = a === 'running' && row.stalled ? 'Running, no output for a while' : ATTENTION_LABEL[a];
  return (
    <span
      className={`sb-ind sb-ind-${a}`}
      data-state={a}
      data-stalled={row.stalled ? 'true' : undefined}
      role="img"
      aria-label={label}
      title={label}
    >
      {a === 'pinned' && <IconPin size={11} />}
      {a === 'running' && <WheelMark size={20} state={row.status === 'stopping' ? 'stopping' : 'working'} className="sb-ind-wheel" />}
      {a === 'lingering' && <WheelMark size={20} state="calm" className="sb-ind-wheel" />}
      {a === 'unread' && <StarMark size={26} className="sb-ind-star" />}
    </span>
  );
}

function ChatRow({
  item,
  indent,
  project,
  selected,
  needsYou,
  dragging,
  now,
  onOpen,
  onToggle,
  onMenu,
  onArchive,
  onHover,
  onLeave,
  onPointerDown,
}: {
  item: Extract<ListItem, { kind: 'chat' }>;
  indent: number;
  project: ProjectNode | undefined;
  selected: boolean;
  needsYou: boolean;
  dragging: boolean;
  now: number;
  onOpen: () => void;
  onToggle: () => void;
  onMenu: (x: number, y: number) => void;
  onArchive: () => void;
  onHover: (el: HTMLElement | null) => void;
  onLeave: () => void;
  onPointerDown: (e: ReactPointerEvent) => void;
}) {
  // A chat whose own turn ended but whose subagents still run shows the wheel.
  const subagents = useSubagentsRunning(item.row.chatId);
  const r = subagents > 0 && item.row.attention !== 'running' ? { ...item.row, attention: 'running' as const } : item.row;
  const colour = colourCss(project?.colour);
  const trailing = trailingIndicator(r.attention);
  const busy = r.status === 'starting' || r.status === 'running' || r.status === 'stopping';
  const wrap = useRef<HTMLDivElement>(null);
  return (
    <div
      ref={wrap}
      className="sb-chat-wrap"
      data-selected={selected ? 'true' : undefined}
      data-trailing={trailing ? 'true' : undefined}
      data-coloured={colour ? 'true' : undefined}
      data-flagged={needsYou ? 'true' : undefined}
      data-drag-source={dragging ? 'true' : undefined}
      data-chat-id={r.chatId}
      style={{
        marginLeft: indent + item.depth * 16,
        ...(colour ? { ['--group-colour' as string]: colour } : {}),
      }}
      onPointerEnter={() => onHover(wrap.current)}
      onPointerLeave={onLeave}
      onContextMenu={(e) => {
        e.preventDefault();
        onMenu(e.clientX, e.clientY);
      }}
    >
      {item.children > 0 && (
        <button
          type="button"
          className="sb-children"
          data-snug={r.attention === null || trailing ? 'true' : undefined}
          aria-expanded={item.expanded}
          aria-label={`${item.expanded ? 'Collapse' : 'Expand'} ${item.children} started chats under ${r.title}`}
          onClick={onToggle}
        >
          {item.expanded ? <IconChevronDown size={12} strokeWidth={2.4} /> : <IconChevronRight size={12} strokeWidth={2.4} />}
        </button>
      )}
      <button
        type="button"
        className="sb-chat"
        data-testid="chat-item"
        data-chat-id={r.chatId}
        data-status={r.status}
        data-depth={item.depth}
        data-group={r.groupProjectId}
        data-muted={r.muted ? 'true' : undefined}
        data-stalled={r.stalled ? 'true' : undefined}
        aria-current={selected ? 'true' : undefined}
        onClick={onOpen}
        onPointerDown={onPointerDown}
      >
        <Indicator row={r} />
        <span className="sb-chat-text">
          <span className="sb-chat-title">{r.title}</span>
          {item.twoLine && <span className="sb-chat-sub">{project?.name ?? r.folderPath}</span>}
        </span>
        {needsYou && (
          <span className="sb-flag" role="img" aria-label="Needs you" title="Needs you">
            <IconFlag size={12} filled />
          </span>
        )}
        {r.muted && (
          <span className="sb-muted" role="img" aria-label="Muted: no notifications or needs-you flags" title="Muted">
            <IconMute size={11} />
          </span>
        )}
        {!item.expanded && item.children > 0 && <span className="sb-agg">{item.children}</span>}
        {item.itemChip && <ItemChip item={item.itemChip} />}
        <span className="sb-age">{age(r.updatedAt, now)}</span>
      </button>
      <span className="sb-row-actions">
        <button
          type="button"
          className="sb-act"
          aria-label={`Options for ${r.title}`}
          title="Chat options"
          onClick={(e) => {
            const b = e.currentTarget.getBoundingClientRect();
            onMenu(b.right, b.bottom);
          }}
        >
          <IconMore />
        </button>
        <button
          type="button"
          className="sb-act"
          aria-label={r.archived ? `Restore ${r.title}` : `Archive ${r.title}`}
          title={r.archived ? 'Restore chat' : busy ? 'Stop the task first, then archive it' : 'Archive chat'}
          disabled={!r.archived && busy}
          onClick={onArchive}
        >
          {r.archived ? <IconRestore /> : <IconArchive />}
        </button>
      </span>
    </div>
  );
}

export function PromptDialog({ prompt, onClose }: { prompt: Prompt; onClose: () => void }) {
  const [value, setValue] = useState(prompt.value);
  const save = () => {
    prompt.onSave(value);
    onClose();
  };
  return (
    <Dialog
      title={prompt.title}
      onClose={onClose}
      testId="sidebar-prompt"
      footer={
        <>
          <Button variant="secondary" onClick={onClose}>
            Cancel
          </Button>
          <Button variant="primary" onClick={save} disabled={value === prompt.value}>
            Save
          </Button>
        </>
      }
    >
      <label className="sb-prompt">
        <span>{prompt.label}</span>
        <TextInput
          autoFocus
          value={value}
          maxLength={120}
          onChange={(e) => setValue(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === 'Enter') save();
          }}
        />
      </label>
    </Dialog>
  );
}

function PreviewCard({
  preview,
  project,
  now,
  load,
  onEnter,
  onLeave,
  onOpen,
}: {
  preview: Preview;
  project: ProjectNode | undefined;
  now: number;
  /** The row's hover preview from the host (`sidebar_rows`): last reply and current step. */
  load: (chatId: string) => Promise<RowPreview | null>;
  onEnter: () => void;
  onLeave: () => void;
  onOpen: () => void;
}) {
  const r = preview.row;
  const live = r.status === 'starting' || r.status === 'running' || r.status === 'stopping';
  const [got, setGot] = useState<{ chatId: string; preview: RowPreview | null; failed: boolean } | null>(null);
  useEffect(() => {
    let on = true;
    load(r.chatId)
      .then((p) => on && setGot({ chatId: r.chatId, preview: p, failed: false }))
      .catch(() => on && setGot({ chatId: r.chatId, preview: null, failed: true }));
    return () => {
      on = false;
    };
    // Re-read when the row changes (a new reply or step arrives as a new row revision).
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [r.chatId, r.revision, r.status, r.updatedAt]);
  const mine = got?.chatId === r.chatId ? got : null;
  const step = live ? mine?.preview?.step : null;
  const state = r.stalled
    ? 'Running, no output for a while'
    : live
      ? (step ?? 'Running')
      : r.status === 'interrupted'
        ? 'Interrupted'
        : r.unread
          ? 'Finished, unread'
          : 'Finished';
  const body = !mine
    ? 'Loading preview…'
    : mine.failed
      ? 'Preview unavailable. Open the chat to view this task.'
      : (mine.preview?.excerpt ?? (live ? 'Waiting for the first update…' : 'No messages yet.'));
  return (
    <div
      className="sb-preview"
      data-testid="sidebar-preview"
      data-chat-id={r.chatId}
      role="dialog"
      aria-label={`Preview of ${r.title}`}
      style={{ top: preview.top, left: preview.left }}
      onPointerEnter={onEnter}
      onPointerLeave={onLeave}
    >
      <div className="sb-preview-head">
        <span className="sb-preview-project">{project?.name ?? r.folderPath}</span>
        <span className="sb-preview-title">{r.title}</span>
        {live && <span className="sb-preview-live">● Live</span>}
      </div>
      <div className="sb-preview-body">
        <p className="sb-preview-excerpt" data-empty={mine?.preview?.excerpt ? undefined : 'true'}>
          {body}
        </p>
        <span className="sb-preview-meta">
          {r.folderPath} · Last activity {age(r.lastActivityAt || r.updatedAt, now)}
        </span>
      </div>
      <div className="sb-preview-foot">
        <span>{state}</span>
        <button type="button" className="sb-preview-open" onClick={onOpen}>
          Open chat ↗
        </button>
      </div>
    </div>
  );
}

/** A clock that ticks every 30 seconds, for row ages. */
function useNow(): number {
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    const t = setInterval(() => setNow(Date.now()), 30_000);
    return () => clearInterval(t);
  }, []);
  return now;
}
