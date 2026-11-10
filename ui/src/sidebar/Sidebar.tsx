// The sidebar: one grouped tree of
// board projects with their chats (a folder project only while chats are still shown under it),
// virtualised, with the Projects / Activity / Archived views, menus, drag and drop, hover preview
// and undo. Its test ids stay stable: `new-chat`, `chat-item`.

import { useCallback, useEffect, useLayoutEffect, useMemo, useRef, useState, useSyncExternalStore, type PointerEvent as ReactPointerEvent, type ReactNode } from 'react';
import type { Api } from '../api/client.ts';
import type { EventBus } from '../api/events.ts';
import type { ChatOps } from '../api/ops.ts';
import { projectOps, type ProjectNode, type SidebarRow, type SidebarView } from '../api/projects.ts';
import type { RowPreview } from '../api/generated/projects/index.ts';
import type { AttentionSummary } from '../api/types.ts';
import type { ChatStore } from '../state/store.ts';
import { SETTINGS_DIALOG } from '../settings/index.ts';
import { SEARCH_DIALOG } from '../search/index.ts';
import { AttentionInbox, inboxRows, nextAttention } from '../attention/index.ts';
import { PagesEntry } from '../pages/PagesEntry.tsx';
import { ProfileDisc } from '../profiles/Disc.tsx';
import { ProfileItem, moveChatToProfile, profileChoices, rowsForProfile, shareChatWith, shareChoices, sharedFrom, useProfiles } from '../profiles/index.ts';
import { DeviceToggle } from '../devices/DeviceToggle.tsx';
import { useDevices, useRemoteList } from '../devices/hooks.ts';
import { scopeNote } from '../devices/model.ts';
import { useSelection, useShell } from '../shell/context.ts';
import { ConfirmDialog, ContextMenu, useBinding, type ConfirmSpec, type MenuItem } from '../ui/index.ts';
import { SidebarActions, compactChat, exportChatMarkdown, rollOverChat, rollOverConfirm } from './actions.ts';
import { SidebarData, errorText } from './data.ts';
import { canSitUnder, dropAction, passedThreshold, type DragSource, type DropTarget } from './drag.ts';
import { chatMenuItems, projectMenuItems } from './menus.ts';
import { buildList, chatAncestors, projectsHolding, showsCalmWheel, visibleChatOrder, type ListItem } from './model.ts';
import { setVisibleOrder } from './rowOrder.ts';
import { SidebarTop } from './SidebarTop.tsx';
import { PromptDialog, type Prompt } from './PromptDialog.tsx';
import { ItemChip, ItemFolderRow, ItemOpenButton, ItemPickerHost, useItemRefs, useOpenItem } from '../itemlinks/ItemUi.tsx'; // item links
import {
  IconArchive,
  IconBoard,
  IconChevronDown,
  IconChevronRight,
  IconEdit,
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
import { openNewChat, openPrimaryFolder } from '../newchat/index.ts';
import { getDefaultBoard } from '../newchat/boards.ts';
import { ATTENTION_LABEL, age, needsYouLabel, rowStatusLabel, trailingIndicator } from './status.ts';
import { localWaitLabel } from '../state/localModel.ts';
import { useSubagentsRunning } from '../transcript/subagents/counts.ts';
import { startSyncTriggers, syncOnOpen, type SyncHost } from './sync.ts';
import { sidebarUndo } from './undo.ts';
import { CostDialog, type CostTarget } from '../usage/CostDialog.tsx';
import { layout, visibleRange } from './virtual.ts';
import './sidebar.css';

export { PromptDialog, type Prompt } from './PromptDialog.tsx';

export const PREVIEW_DELAY_MS = 300;
/** The longest project description, in characters. */
export const DESCRIPTION_MAX = 200;
/** The space between one project's block and the next, in pixels. */
const BLOCK_GAP = 4;

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
type Preview = { row: SidebarRow; top: number; left: number };
type Drag = { src: DragSource; x0: number; y0: number; active: boolean; title: string; target: DropTarget | null; x: number; y: number };

export function SidebarView({ api, bus, store, ops, selected, onSelect, openSettings, openSearch }: SidebarProps) {
  const pops = useMemo(() => projectOps(api), [api]);
  const data = useMemo(() => new SidebarData(pops), [pops]);
  const snap = useSyncExternalStore(data.subscribe, data.getSnapshot);
  const { store: profileStore, snap: prof } = useProfiles(api, bus);
  // The profile choice filters the chats drawn: its own and those shared with it. The sidebar's
  // data keeps every row it has loaded.
  const activeProfile = prof.view?.activeId ?? null;
  const localRows = useMemo(() => rowsForProfile(snap.rows, activeProfile, prof.ids) as SidebarRow[], [snap.rows, activeProfile, prof.ids]);
  const [view, setView] = useState<SidebarView>('projects');
  // Another device's chats: read through the mirror for the active profile, drawn by the same tree.
  const { snap: dev } = useDevices(api, bus);
  const remote = dev.remote;
  const { snap: other, loadMore: loadMoreRemote } = useRemoteList(api, bus, remote?.uid ?? null, activeProfile, view === 'archived');
  const shownRows = remote ? other.rows : localRows;
  // Another device's groups collapse on this screen only: its projects are not ours to change.
  const [remoteShut, setRemoteShut] = useState<ReadonlySet<string>>(() => new Set());
  const shownProjects = useMemo(
    () => (remote ? other.projects.map((p) => (remoteShut.has(p.id) ? { ...p, collapsed: true } : p)) : snap.projects),
    [remote, other.projects, remoteShut, snap.projects],
  );
  const [error, setError] = useState<string | null>(null);
  const [expanded, setExpanded] = useState<Set<string>>(() => new Set());
  const [collapsedItems, setCollapsedItems] = useState<Set<string>>(() => new Set());
  const [compact, setCompact] = useState(false);
  const [menu, setMenu] = useState<Menu | null>(null);
  const [prompt, setPrompt] = useState<Prompt | null>(null);
  const [confirm, setConfirm] = useState<ConfirmSpec | null>(null);
  const [cost, setCost] = useState<CostTarget | null>(null);
  const [preview, setPreview] = useState<Preview | null>(null);
  const [drag, setDrag] = useState<Drag | null>(null);
  const [inboxOpen, setInboxOpen] = useState(false);
  const undo = sidebarUndo;
  const act = useMemo(() => new SidebarActions({ api, pops, ops, data, undo, onError: setError }), [api, pops, ops, data, undo]);
  const now = useNow();

  useEffect(() => data.connect(bus), [data, bus]);

  // The boards pickers leave out (the catch-all inbox), for "Move to project".
  const [hiddenBoards, setHiddenBoards] = useState<string[]>([]);
  useEffect(() => {
    let live = true;
    const load = () => void getDefaultBoard(api).then((b) => live && setHiddenBoards(b.hiddenBoardIds));
    load();
    const offs = [bus.on('board_sync', load), bus.on('project', load)];
    return () => {
      live = false;
      offs.forEach((o) => o());
    };
  }, [api, bus]);
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

  // Agent Standup sync triggers. Quiet: nothing is drawn, and a failure shows in Settings → Projects.
  const runSync = useCallback(() => {
    pops
      .sync()
      .then((s) => {
        data.setSync(s);
        void data.loadProjects();
      })
      .catch(() => undefined);
  }, [pops, data]);
  // The chat menu lists boards to move to, so opening it syncs too (at most every 30 seconds).
  const syncBoardsOnOpen = useMemo(() => syncOnOpen(runSync), [runSync]);
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

  // The open chat's project, and the projects it sits in, draw expanded even where they were left
  // collapsed, so a chat opened from search, a link or a notification can be seen. A project the
  // user collapses stays collapsed (only that one, not the projects around it) until another chat
  // is opened.
  const [keptShut, setKeptShut] = useState<{ chat: string | null; ids: ReadonlySet<string> }>({ chat: null, ids: new Set() });
  useEffect(() => setKeptShut({ chat: null, ids: new Set() }), [selected]);

  const openItem = useOpenItem();
  const itemRefs = useItemRefs(useMemo(() => snap.rows.map((r) => r.chatId), [snap.rows]));
  const flaggedIds = useMemo(() => new Set([...snap.needsYou, ...snap.flagged.keys()]), [snap.needsYou, snap.flagged]);
  const items = useMemo(
    () =>
      buildList(shownProjects, shownRows, {
        view,
        compact,
        expandedChats: expanded,
        selectedChatId: selected,
        keptShut: keptShut.chat === selected ? keptShut.ids : undefined,
        flagged: remote ? undefined : flaggedIds,
        closed: remote ? undefined : snap.closed,
        itemLinks: view === 'activity' || remote ? undefined : { refs: itemRefs, collapsed: collapsedItems },
      }),
    [shownProjects, shownRows, view, compact, expanded, selected, keptShut, flaggedIds, snap.closed, itemRefs, collapsedItems, remote],
  );
  useEffect(() => setVisibleOrder(visibleChatOrder(items)), [items]);
  const projectsById = useMemo(() => new Map(shownProjects.map((p) => [p.id, p])), [shownProjects]);
  const slotProject = (it: ListItem): ProjectNode | undefined =>
    projectsById.get(it.kind === 'header' ? it.project.id : it.kind === 'chat' ? it.row.groupProjectId : it.kind === 'itemFolder' ? it.groupId : '');
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
    const projectId = remote ? undefined : selectedRow?.groupProjectId;
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

  // A running chat is archived through a confirmation that stops its task first.
  const archiveRow = (row: SidebarRow) => void act.archive(row);

  // ---- menus ----
  // Alt+Up and Alt+Down on a project header move it among its siblings (the same pinned group and parent).
  const moveProject = (p: ProjectNode, dir: -1 | 1) => {
    const sibs = items.flatMap((i) => (i.kind === 'header' && i.project.parentId === p.parentId && i.project.pinned === p.pinned ? [i.project] : []));
    const at = sibs.findIndex((x) => x.id === p.id);
    const to = at + dir;
    if (at < 0 || to < 0 || to >= sibs.length) return;
    void act.reorderProject(p, dir < 0 ? sibs[to]!.id : (sibs[to + 1]?.id ?? null));
  };
  const promptRename = (row: SidebarRow) => setPrompt({ title: 'Rename chat', label: 'Title', value: row.title, suggestFor: row.chatId, onSave: (t) => act.renameChat(row, t) });
  const rowMenuItems = (row: SidebarRow): MenuItem[] => {
    // A chat on another device is renamed here; everything else is done on that device.
    if (remote) return withChatIcons([{ id: 'rename', label: 'Rename chat…', onSelect: () => promptRename(row) }]);
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
    const items = withChatIcons(chatMenuItems(
      { row, projects: snap.projects, hidden: hiddenBoards, siblings, heightOf, depthOf, expanded: expanded.has(row.chatId), view, profiles: profileChoices(prof, row.chatId, snap.rows), shares: shareChoices(prof, row.chatId, snap.rows) },
      {
        fork: () => void act.fork(row),
        rename: () => promptRename(row),
        compact: () => void compactChat(ops, row.chatId, setError),
        rollOver: () => setConfirm(rollOverConfirm(row.title, () => rollOverChat(ops, row.chatId, setError))),
        cost: () => setCost({ chatId: row.chatId }),
        exportMarkdown: () => void exportChatMarkdown(api, row.chatId, setError),
        moveTo: (id) => void act.moveTo(row, id),
        moveToProfile: (id) => moveChatToProfile(profileStore, row.chatId, id, setError),
        shareWith: (id) => shareChatWith(profileStore, row.chatId, shareChoices(prof, row.chatId, snap.rows), id, setError),
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
        archive: () => archiveRow(row),
        restore: () => void act.restore(row),
        remove: () => setConfirm(deleteChatConfirm(row, () => act.remove(row))),
      },
    ));
    // On a hovered row the chip gives way to an "open" button; the menu keeps the entry for the keyboard.
    const linked = itemRefs.get(row.chatId);
    if (linked) items.splice(1, 0, { id: 'open-item', label: linked.named ? `Open item: ${linked.title}` : 'Open linked item', onSelect: () => openItem(linked.itemId) });
    return items;
  };
  const projectMenu = (p: ProjectNode): MenuItem[] =>
    withProjectIcons(projectMenuItems(p, snap.projects, {
      newChat: () => newChatIn(p),
      pin: (pinned) => void act.updateProject(p, { pinned }),
      rename: () => setPrompt({ title: 'Rename project', label: 'Name', value: p.name, onSave: (name) => act.editProject(p, { name }) }),
      changeIcon: () =>
        setPrompt({
          title: 'Project icon',
          label: 'One emoji (empty for the default icon)',
          value: p.icon ?? '',
          onSave: (v) => act.editProject(p, { icon: v.trim() ? v.trim() : null }),
        }),
      editDescription: () =>
        setPrompt({
          title: 'Project description',
          label: 'One line telling agents what this project is for',
          value: (p as ProjectNode & { description?: string | null }).description ?? '',
          maxLength: DESCRIPTION_MAX,
          onSave: (v) => act.editProject(p, { description: v.trim() }),
        }),
      cost: () => setCost({ projectId: p.id }),
      nestUnder: (id) => void act.nestProject(p, id),
      topLevel: () => void act.nestProject(p, null),
      archive: () => setConfirm(archiveProjectConfirm(p, () => act.archiveProject(p))),
      restore: () => void act.restoreProject(p),
      close: () => setConfirm(closeProjectConfirm(p, () => act.closeProject(p))),
      colour: (c) => void act.updateProject(p, { colour: c }),
      customColour: () =>
        setPrompt({
          title: 'Project colour',
          label: 'Hex colour, like #4a90d9',
          value: p.colour?.startsWith('#') ? p.colour : '#',
          onSave: (v) => {
            const hex = v.trim();
            if (/^#[0-9a-fA-F]{6}$/.test(hex)) void act.updateProject(p, { colour: hex.toLowerCase() });
            else setError('A colour is # followed by six hex digits, like #4a90d9.');
          },
        }),
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
  useBinding({ id: 'chat.archive', title: 'Archive chat', defaultKeys: 'Mod+Shift+Backspace', enabled: () => !!selectedRow && !selectedRow.archived, run: () => selectedRow && archiveRow(selectedRow) });

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
    if (e.button !== 0 || e.ctrlKey || e.pointerType === 'touch' || view === 'activity' || remote) return;
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
        return { kind: 'header', projectId: p.id, parentId: p.parentId, pinned: p.pinned, lowerHalf: lowerOf(header), board: p.kind === 'board' };
      }
      if (chat) {
        const row = data.row(chat.dataset.chatId ?? '');
        if (!row) return null;
        const moving = dragRef.current?.src;
        const rows = data.getSnapshot().rows;
        const mine = moving?.kind === 'chat' ? moving.chatId : null;
        return {
          kind: 'chat',
          chatId: row.chatId,
          groupId: row.groupProjectId,
          pinned: row.pinned,
          lowerHalf: lowerOf(chat),
          parentChatId: row.parentChatId,
          levelOk: mine ? canSitUnder(rows, mine, row.parentChatId) : true,
          nestOk: mine ? canSitUnder(rows, mine, row.chatId) : true,
          board: projectsById.get(row.groupProjectId)?.kind === 'board',
        };
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
        else if (a.kind === 'chat-before') void act.reorderBefore(row, a.beforeChatId, { parentChatId: a.parentChatId, pinned: a.pinned, ...(a.moveGroup ? { groupProjectId: a.groupId } : {}) });
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

  const paging = remote ? { loading: other.loading, complete: !other.more, error: other.error, started: true } : snap.paging[view];
  const loadMoreChats = () => (remote ? loadMoreRemote() : void data.page(view));
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
        api={api}
        bus={bus}
        attention={snap.attention}
        inboxOpen={inboxOpen}
        onNewChat={navNewChat}
        onSearch={openSearch}
        onAttention={() => setInboxOpen(true)}
        onOpenChat={onSelect}
        openSettings={openSettings}
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
            <button type="button" data-testid="sidebar-view-projects" aria-pressed={view === 'projects'} title="Chats grouped by project" onClick={() => switchView('projects')}>
              Projects
            </button>
            <button
              type="button"
              data-testid="sidebar-view-activity"
              aria-pressed={view === 'activity'}
              title="Recent chat activity across all projects, newest first"
              onClick={() => switchView('activity')}
            >
              Activity
            </button>
          </div>
        )}
        <DeviceToggle api={api} bus={bus} onError={setError} />
      </div>
      {remote && (
        <div className="sb-device-note" data-testid="device-scope-note" data-online={remote.online ? 'true' : 'false'}>
          {scopeNote(remote, now)}
        </div>
      )}
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
          {items.slice(start, end).map((it, k) => {
            // A project's header and chats draw as one tinted block: the slot carries the tint and
            // the corners (rounded on the block's first and last slot), and the last slot is
            // shortened by the gap that separates it from the next block.
            const colour = it.block ? colourCss(slotProject(it)?.colour) : null;
            const gap = it.block === 'last' || it.block === 'only' ? BLOCK_GAP : 0;
            return (
              <div
                key={it.key}
                className="sb-slot"
                data-block={it.block}
                data-coloured={colour ? 'true' : undefined}
                data-drop={dropKey === it.key ? (dropOk ? (drag?.target?.lowerHalf ? 'into' : 'before') : 'no') : undefined}
                style={{
                  height: it.height - gap,
                  transform: `translateY(${lay.offsets[start + k]}px)`,
                  ...(colour ? { ['--group-colour' as string]: colour } : {}),
                }}
              >
                {renderItem(it)}
              </div>
            );
          })}
        </div>
        {items.length === 0 && !paging.loading && (
          <div className="sb-empty">
            {remote
              ? remote.online
                ? `No chats on ${remote.name} for this profile`
                : `${remote.name} is offline and no copy of its chats is held here`
              : view === 'activity'
                ? 'No recent activity'
                : view === 'archived'
                  ? 'No archived chats'
                  : 'Add your first project'}
          </div>
        )}
        {(!paging.complete || paging.error) && paging.started && (
          <button type="button" className="sb-more" disabled={paging.loading} onClick={loadMoreChats}>
            {paging.loading ? 'Loading chats…' : paging.error ? 'Retry' : 'Load more chats'}
          </button>
        )}
      </div>
      <div className="sb-footer">
        <button
          type="button"
          className="sb-foot"
          aria-pressed={view === 'archived'}
          data-testid="nav-archived"
          onClick={() => switchView(view === 'archived' ? 'projects' : 'archived')}
        >
          <IconArchive />
          <span>{view === 'archived' ? 'Back to projects' : 'Archived chats'}</span>
        </button>
        <PagesEntry api={api} bus={bus} profileId={activeProfile} />
        <ProfileItem api={api} bus={bus} />
        {openSettings && (
          <button type="button" className="sb-foot" data-testid="nav-settings" onClick={openSettings}>
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
      {confirm && <ConfirmDialog spec={confirm} onClose={() => setConfirm(null)} />}
      {cost && <CostDialog api={api} target={cost} onClose={() => setCost(null)} />}
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

  /** The owner of a chat that is shared with the active profile (a small disc after the title). */
  function sharedByOf(row: SidebarRow): { name: string; colour: string } | null {
    const owner = sharedFrom(row, activeProfile);
    const p = owner ? prof.view?.profiles.find((x) => x.id === owner) : undefined;
    return p ? { name: p.name, colour: p.colour } : null;
  }

  function renderItem(it: ListItem) {
    if (it.kind === 'pinBreak') return <div className="sb-pin-break" aria-hidden="true" />;
    if (it.kind === 'itemFolder') {
      return (
        <ItemFolderRow
          itemId={it.itemId}
          title={it.title}
          members={it.members}
          collapsed={it.collapsed}
          indent={(groupDepth.get(it.groupId) ?? 0) * 16 + it.depth * 16 + (it.block ? 4 : 0)}
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
            const collapse = !it.collapsed;
            // Drawn open only because it holds the open chat: stop doing that, so the collapse shows.
            if (collapse && selected && projectsHolding(projectsById, selectedRow?.groupProjectId).has(p.id)) {
              setKeptShut((k) => ({ chat: selected, ids: new Set(k.chat === selected ? k.ids : []).add(p.id) }));
            }
            if (remote) {
              setRemoteShut((s) => {
                const n = new Set(s);
                if (collapse) n.add(p.id);
                else n.delete(p.id);
                return n;
              });
            } else if (p.collapsed !== collapse) void act.updateProject(p, { collapsed: collapse });
          }}
          readOnly={!!remote}
          onPin={() => void act.updateProject(p, { pinned: !p.pinned })}
          onMove={(dir) => moveProject(p, dir)}
          onNew={() => newChatIn(p)}
          onSetFolder={() => openPrimaryFolder(p.id)}
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
        indent={view === 'activity' ? 0 : (groupDepth.get(row.groupProjectId) ?? 0) * 16 + (it.block ? 4 : 0)}
        project={projectsById.get(row.groupProjectId)}
        selected={row.chatId === selected}
        needsYou={!remote && snap.needsYou.has(row.chatId)}
        needsLabel={needsYouLabel(snap.attention?.needsYou.find((r) => r.chatId === row.chatId)?.kind)}
        flagReason={remote ? undefined : snap.flagged.get(row.chatId)}
        remote={remote ? { name: remote.name, online: remote.online } : null}
        sharedBy={sharedByOf(row)}
        dragging={drag?.active === true && drag.src.kind === 'chat' && drag.src.chatId === row.chatId}
        now={now}
        onOpen={() => {
          if (suppressClick.current) return;
          clearTimeout(previewTimer.current);
          setPreview(null);
          onSelect(row.chatId);
        }}
        onToggle={() => toggleChat(row.chatId)}
        onRename={() => promptRename(row)}
        onMenu={(x, y) => {
          syncBoardsOnOpen();
          openMenu(x, y, rowMenuItems(row), `Options for ${row.title}`);
        }}
        onArchive={() => (row.archived ? void act.restore(row) : archiveRow(row))}
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

/** What a project's icon looks like in the tree: its emoji, else the board or folder glyph. One
 *  place, so the shared project icon can replace it. */
function projectIconNode(p: ProjectNode): ReactNode {
  if (p.icon && !p.icon.startsWith('lib:')) return p.icon;
  return p.kind === 'board' ? <IconBoard size={16} strokeWidth={1.8} /> : <IconFolder size={16} strokeWidth={1.8} />;
}

function GroupHeader({
  item,
  current,
  dragging,
  readOnly,
  onToggle,
  onPin,
  onMove,
  onNew,
  onSetFolder,
  onRestore,
  onMenu,
  onPointerDown,
}: {
  item: Extract<ListItem, { kind: 'header' }>;
  current: boolean;
  dragging: boolean;
  /** Another device's project: it collapses here, and has no menu or buttons. */
  readOnly: boolean;
  onToggle: () => void;
  onPin: () => void;
  onMove: (dir: -1 | 1) => void;
  onNew: () => void;
  onSetFolder: () => void;
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
      title={[p.path ?? p.board?.title ?? p.name, p.board?.editedLocally ? 'Edited in Helm' : null, 'Drag to reorder or nest (Alt+Up or Down moves it)']
        .filter(Boolean)
        .join('\n')}
      onContextMenu={(e) => {
        e.preventDefault();
        if (!readOnly) onMenu(e.clientX, e.clientY);
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
        <button
          type="button"
          className="sb-name"
          onClick={onToggle}
          onPointerDown={onPointerDown}
          onKeyDown={(e) => {
            if (!readOnly && e.altKey && (e.key === 'ArrowUp' || e.key === 'ArrowDown')) {
              e.preventDefault();
              onMove(e.key === 'ArrowUp' ? -1 : 1);
            }
          }}
          aria-expanded={!item.collapsed}
        >
          <span className="sb-icon" aria-hidden="true">
            {projectIconNode(p)}
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
          {item.marks && (
            <span className="sb-group-marks" data-testid="group-marks">
              {item.marks.running > 0 && (
                <span className="sb-ind" role="img" aria-label={`${item.marks.running} running`} title={`${item.marks.running} running`}>
                  <WheelMark size={20} state="working" className="sb-ind-wheel" />
                </span>
              )}
              {showsCalmWheel(item.marks) && (
                <span className="sb-ind" data-testid="group-background" role="img" aria-label="Background work running" title="Background work running">
                  <WheelMark size={20} state="calm" className="sb-ind-wheel" />
                </span>
              )}
              {item.marks.unread > 0 && (
                <span className="sb-ind" role="img" aria-label={`${item.marks.unread} unread`} title={`${item.marks.unread} unread`}>
                  <StarMark size={26} className="sb-ind-star ui-star-shimmer" />
                </span>
              )}
              {item.marks.flagged > 0 && (
                <span className="sb-flag" data-testid="group-flag" role="img" aria-label={`${item.marks.flagged} may need you`} title={`${item.marks.flagged} may need you`}>
                  <IconFlag size={12} filled />
                </span>
              )}
            </span>
          )}
        </button>
        {item.needsFolder && (
          <button
            type="button"
            className="sb-set-folder"
            data-testid="sidebar-set-folder"
            title={`Choose the folder new chats in ${p.name} start in`}
            onClick={onSetFolder}
          >
            Set folder
          </button>
        )}
        <span className="sb-actions" hidden={readOnly}>
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
  const label = a === 'running' ? rowStatusLabel(row) : ATTENTION_LABEL[a];
  return (
    <span
      className={`sb-ind sb-ind-${a}`}
      data-state={a}
      data-stalled={row.stalled ? 'true' : undefined}
      data-local={a === 'running' && row.local ? 'true' : undefined}
      data-ahead={a === 'running' && row.local && row.localAhead !== null ? String(row.localAhead) : undefined}
      role="img"
      aria-label={label}
      title={label}
    >
      {a === 'pinned' && <IconPin size={11} />}
      {a === 'running' && <WheelMark size={20} state={row.status === 'stopping' ? 'stopping' : 'working'} className="sb-ind-wheel" />}
      {a === 'lingering' && <WheelMark size={20} state="calm" className="sb-ind-wheel" />}
      {a === 'unread' && <StarMark size={26} className="sb-ind-star ui-star-shimmer" />}
    </span>
  );
}

function ChatRow({
  item,
  indent,
  project,
  selected,
  needsYou,
  needsLabel,
  flagReason,
  remote,
  sharedBy,
  dragging,
  now,
  onOpen,
  onToggle,
  onRename,
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
  /** The flag's wording: "Answer question" when the chat waits on an answer, else "Needs you". */
  needsLabel: string;
  /** Why the idle check thinks this quiet chat still needs you. */
  flagReason: string | undefined;
  /** The chat runs on another device: dimmed while that device is offline, and it cannot be archived here. */
  remote: { name: string; online: boolean } | null;
  /** Who shared this chat with the active profile, when it is not the active profile's own. */
  sharedBy: { name: string; colour: string } | null;
  dragging: boolean;
  now: number;
  onOpen: () => void;
  onToggle: () => void;
  /** Double-click on the row. */
  onRename: () => void;
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
  const wrap = useRef<HTMLDivElement>(null);
  return (
    <div
      ref={wrap}
      className="sb-chat-wrap"
      data-selected={selected ? 'true' : undefined}
      data-trailing={trailing ? 'true' : undefined}
      data-linked={item.itemChip ? 'true' : undefined}
      data-coloured={colour ? 'true' : undefined}
      data-flagged={needsYou || flagReason !== undefined ? 'true' : undefined}
      data-drag-source={dragging ? 'true' : undefined}
      data-offline={remote && !remote.online ? 'true' : undefined}
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
        title={remote && !remote.online ? `${remote.name} is offline` : undefined}
        data-stalled={r.stalled ? 'true' : undefined}
        data-local={r.local ? 'true' : undefined}
        data-ahead={r.local && r.localAhead !== null ? String(r.localAhead) : undefined}
        aria-current={selected ? 'true' : undefined}
        onClick={onOpen}
        onDoubleClick={onRename}
        onPointerDown={onPointerDown}
      >
        <Indicator row={r} />
        <span className="sb-chat-text">
          <span className="sb-chat-title">{r.title}</span>
          {item.twoLine && <span className="sb-chat-sub">{project?.name ?? r.folderPath}</span>}
        </span>
        {needsYou && (
          <span className="sb-flag" role="img" aria-label={needsLabel} title={needsLabel}>
            <IconFlag size={12} filled />
          </span>
        )}
        {!needsYou && flagReason !== undefined && (
          <span className="sb-flag" data-kind="quiet" data-testid="chat-flag" role="img" aria-label={`May still need you: ${flagReason}`} title={`May still need you: ${flagReason}`}>
            <IconFlag size={12} filled />
          </span>
        )}
        {r.muted && (
          <span className="sb-muted" data-testid="row-muted" role="img" aria-label="Muted: no notices or flags" title="Muted: no notices or flags">
            <IconMute size={11} />
          </span>
        )}
        {sharedBy && (
          <span className="sb-shared-from" data-testid="chat-shared-from" role="img" aria-label={`Shared by ${sharedBy.name}`} title={`Shared by ${sharedBy.name}`}>
            <ProfileDisc name={sharedBy.name} colour={sharedBy.colour} size={14} />
          </span>
        )}
        {!item.expanded && item.children > 0 && <span className="sb-agg">{item.children}</span>}
        {item.itemChip && <ItemChip item={item.itemChip} />}
        {r.hasDraft && (
          <span className="sb-draft" role="img" aria-label="Unsent draft" title="Unsent draft">
            <IconEdit size={10} />
          </span>
        )}
        <span className="sb-age">{age(r.updatedAt, now)}</span>
      </button>
      <span className="sb-row-actions">
        {item.itemChip && <ItemOpenButton item={item.itemChip} />}
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
        {!remote && (
          <button
            type="button"
            className="sb-act"
            aria-label={r.archived ? `Restore ${r.title}` : `Archive ${r.title}`}
            title={r.archived ? 'Restore chat' : 'Archive chat'}
            onClick={onArchive}
          >
            {r.archived ? <IconRestore /> : <IconArchive />}
          </button>
        )}
      </span>
    </div>
  );
}

/** The questions asked before the two actions the sidebar cannot undo or that move a whole project. */
export function deleteChatConfirm(row: SidebarRow, onConfirm: () => void | Promise<unknown>): ConfirmSpec {
  return {
    title: 'Delete chat permanently?',
    body: `“${row.title || 'Untitled chat'}” and its history are removed from this PC. This cannot be undone.`,
    confirm: 'Delete permanently',
    danger: true,
    onConfirm,
  };
}

export function closeProjectConfirm(p: ProjectNode, onConfirm: () => void | Promise<unknown>): ConfirmSpec {
  return {
    title: 'Close project?',
    body: `“${p.name}” is hidden from the sidebar and every picker, and nothing new can be added to it. Chats already in it keep working. Reopen it from Settings → Projects.`,
    confirm: 'Close project',
    onConfirm,
  };
}

export function archiveProjectConfirm(p: ProjectNode, onConfirm: () => void | Promise<unknown>): ConfirmSpec {
  return {
    title: 'Archive project?',
    body: `“${p.name}” leaves the sidebar and its chats go with it. Restore it from Archived chats, or press Ctrl+Z right after.`,
    confirm: 'Archive project',
    onConfirm,
  };
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
  const state = r.local && r.localAhead !== null
    ? localWaitLabel(r.localAhead)
    : r.stalled
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
