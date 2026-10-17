// The sidebar: one grouped tree of
// folder and board projects with their chats, virtualised, with the Projects / Activity / Archived
// views. The first chat list's test ids stay: `folder-input`, `new-chat`, `chat-item`.

import { useCallback, useEffect, useLayoutEffect, useMemo, useRef, useState, useSyncExternalStore } from 'react';
import type { Api } from '../api/client.ts';
import type { EventBus } from '../api/events.ts';
import type { ChatOps } from '../api/ops.ts';
import { projectOps, type ProjectNode, type SidebarRow, type SidebarView } from '../api/projects.ts';
import type { ChatStore } from '../state/store.ts';
import { SETTINGS_DIALOG } from '../settings/index.ts';
import { useSelection, useShell } from '../shell/context.ts';
import { SidebarData, errorText } from './data.ts';
import { buildList, chatAncestors, type ListItem } from './model.ts';
import { ATTENTION_LABEL, age, trailingIndicator } from './status.ts';
import { layout, visibleRange } from './virtual.ts';
import './sidebar.css';

export const LAST_FOLDER_KEY = 'helm.lastFolder';

export interface SidebarProps {
  api: Api;
  bus: EventBus;
  store: ChatStore;
  ops: ChatOps;
  selected: string | null;
  onSelect: (chatId: string) => void;
  openSettings?: () => void;
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
    />
  );
}

/** A project colour as CSS: a named colour reads its design token, a custom one is the stored hex. */
export function colourCss(c: string | null | undefined): string | null {
  if (!c) return null;
  return c.startsWith('#') ? c : `var(--project-${c})`;
}

export function SidebarView({ api, bus, store, ops, selected, onSelect, openSettings }: SidebarProps) {
  const pops = useMemo(() => projectOps(api), [api]);
  const data = useMemo(() => new SidebarData(pops), [pops]);
  const snap = useSyncExternalStore(data.subscribe, data.getSnapshot);
  const [view, setView] = useState<SidebarView>('projects');
  const [folder, setFolder] = useState(() => localStorage.getItem(LAST_FOLDER_KEY) ?? '');
  const [error, setError] = useState<string | null>(null);
  const [expanded, setExpanded] = useState<Set<string>>(() => new Set());
  const [compact, setCompact] = useState(false);
  const now = useNow();

  useEffect(() => data.connect(bus), [data, bus]);
  useEffect(() => {
    void data.loadProjects();
    void data.page('projects');
    api<unknown>('preference_get', { key: 'compactSidebar' })
      .then((v) => setCompact(v === true))
      .catch(() => undefined);
  }, [data, api]);
  useEffect(() => {
    if (!snap.paging[view].started) void data.page(view);
  }, [view, data, snap.paging]);

  // The open chat's ancestors expand so it is visible.
  useEffect(() => {
    if (!selected) return;
    const up = chatAncestors(snap.rows, selected);
    if (up.some((id) => !expanded.has(id))) setExpanded((s) => new Set([...s, ...up]));
  }, [selected, snap.rows, expanded]);

  const items = useMemo(
    () => buildList(snap.projects, snap.rows, { view, compact, expandedChats: expanded, selectedChatId: selected }),
    [snap.projects, snap.rows, view, compact, expanded, selected],
  );
  const projectsById = useMemo(() => new Map(snap.projects.map((p) => [p.id, p])), [snap.projects]);
  const groupDepth = useMemo(() => {
    const m = new Map<string, number>();
    for (const it of items) if (it.kind === 'header') m.set(it.project.id, it.depth);
    return m;
  }, [items]);

  const createIn = useCallback(
    async (cwd: string, group?: ProjectNode) => {
      setError(null);
      try {
        const chat = await ops.create(cwd);
        store.chat(chat);
        if (group && group.id !== chat.projectId) {
          await pops.organize({ chatId: chat.id, groupProjectId: group.id });
        }
        await data.refetch([chat.id]);
        onSelect(chat.id);
      } catch (e) {
        setError(errorText(e));
      }
    },
    [ops, store, pops, data, onSelect],
  );

  const create = async () => {
    const cwd = folder.trim();
    if (!cwd) {
      setError('Enter a folder for the chat to work in.');
      return;
    }
    localStorage.setItem(LAST_FOLDER_KEY, cwd);
    await createIn(cwd);
  };

  const newChatIn = (p: ProjectNode) => {
    const primary = p.primaryFolderProjectId ? projectsById.get(p.primaryFolderProjectId) : undefined;
    const cwd = p.path ?? primary?.path ?? folder.trim();
    if (!cwd) {
      setError(`Set a primary folder for ${p.name}, or enter a folder above.`);
      return;
    }
    void createIn(cwd, p);
  };

  const updateProject = async (p: ProjectNode, patch: Record<string, unknown>) => {
    try {
      data.project(await pops.update({ projectId: p.id, revision: p.revision, ...patch }));
    } catch (e) {
      setError(errorText(e));
      void data.loadProjects();
    }
  };

  const toggleChat = (id: string) =>
    setExpanded((s) => {
      const n = new Set(s);
      if (n.has(id)) n.delete(id);
      else n.add(id);
      return n;
    });

  const paging = snap.paging[view];
  const switchView = (v: SidebarView) => {
    setView(v);
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

  return (
    <div className="sb-root" data-view={view} data-compact={compact ? 'true' : 'false'}>
      <div className="sb-brand">Helm</div>
      <form
        className="sb-new-chat"
        onSubmit={(e) => {
          e.preventDefault();
          void create();
        }}
      >
        <input
          data-testid="folder-input"
          aria-label="Folder"
          placeholder="C:\path\to\folder"
          value={folder}
          onChange={(e) => setFolder(e.target.value)}
          spellCheck={false}
        />
        <button type="submit" data-testid="new-chat" className="sb-nav">
          New chat
        </button>
      </form>
      {(error ?? snap.error) && <p className="sb-error">{error ?? snap.error}</p>}
      <div className="sb-section">
        {view === 'archived' ? (
          <span>Archived chats</span>
        ) : (
          <div className="sb-switch" role="group" aria-label="Sidebar view">
            <button
              type="button"
              aria-pressed={view === 'projects'}
              title="Chats grouped by project"
              onClick={() => switchView('projects')}
            >
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
      <div
        className="sb-list"
        ref={listRef}
        tabIndex={0}
        aria-label={view === 'activity' ? 'Recent activity' : 'Projects and chats'}
        onScroll={(e) => setScrollTop(e.currentTarget.scrollTop)}
      >
        <div className="sb-list-inner" style={{ height: lay.total }}>
          {items.slice(start, end).map((it, k) => (
            <div
              key={it.key}
              className="sb-slot"
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
          {view === 'archived' ? 'Back to projects' : 'Archived chats'}
        </button>
        {openSettings && (
          <button type="button" className="sb-foot" onClick={openSettings}>
            Settings
          </button>
        )}
        <button type="button" className="sb-foot" onClick={() => void api('quit_request')}>
          Quit
        </button>
      </div>
    </div>
  );

  function renderItem(it: ListItem) {
    if (it.kind === 'pinBreak') return <div className="sb-pin-break" aria-hidden="true" />;
    if (it.kind === 'header') {
      return (
        <GroupHeader
          item={it}
          current={selected ? data.row(selected)?.groupProjectId === it.project.id : false}
          onToggle={() => void updateProject(it.project, { collapsed: !it.collapsed })}
          onPin={() => void updateProject(it.project, { pinned: !it.project.pinned })}
          onNew={() => newChatIn(it.project)}
        />
      );
    }
    const project = projectsById.get(it.row.groupProjectId);
    return (
      <ChatRow
        item={it}
        indent={view === 'activity' ? 0 : (groupDepth.get(it.row.groupProjectId) ?? 0) * 16}
        project={project}
        selected={it.row.chatId === selected}
        now={now}
        onOpen={() => onSelect(it.row.chatId)}
        onToggle={() => toggleChat(it.row.chatId)}
      />
    );
  }
}

function GroupHeader({
  item,
  current,
  onToggle,
  onPin,
  onNew,
}: {
  item: Extract<ListItem, { kind: 'header' }>;
  current: boolean;
  onToggle: () => void;
  onPin: () => void;
  onNew: () => void;
}) {
  const p = item.project;
  const colour = colourCss(p.colour);
  const subtitle = p.board?.stale ? 'Not on the board' : null;
  return (
    <div
      className="sb-header"
      data-testid="sidebar-group"
      data-project-id={p.id}
      data-kind={p.kind}
      data-depth={item.depth}
      data-current={current ? 'true' : undefined}
      data-coloured={colour ? 'true' : undefined}
      style={{ marginLeft: item.depth * 16, ...(colour ? { ['--group-colour' as string]: colour } : {}) }}
      title={[p.path ?? p.board?.title ?? p.name, p.board?.editedLocally ? 'Edited in Helm' : null].filter(Boolean).join('\n')}
    >
      <button
        type="button"
        className="sb-toggle"
        aria-label={`${item.collapsed ? 'Expand' : 'Collapse'} ${p.name}`}
        aria-expanded={!item.collapsed}
        onClick={onToggle}
      >
        <span aria-hidden="true">{item.collapsed ? '›' : '⌄'}</span>
      </button>
      <button type="button" className="sb-name" onClick={onToggle} aria-expanded={!item.collapsed}>
        <span className="sb-icon" aria-hidden="true">
          {p.icon && !p.icon.startsWith('lib:') ? p.icon : p.kind === 'board' ? '◆' : '▢'}
        </span>
        <span className="sb-title">{p.name}</span>
        {p.board?.editedLocally && (
          <span className="sb-edited" title="Edited in Helm" aria-label="Edited in Helm">
            ✎
          </span>
        )}
        <span className="sb-count" title={`${p.chatCount} non-archived chats`}>
          {p.chatCount}
        </span>
      </button>
      {subtitle && <span className="sb-subtitle">{subtitle}</span>}
      <span className="sb-actions">
        {!p.archived && (
          <button
            type="button"
            className="sb-act"
            aria-label={`${p.pinned ? 'Unpin' : 'Pin'} project ${p.name}`}
            aria-pressed={p.pinned}
            title={p.pinned ? 'Unpin project' : 'Pin project'}
            onClick={onPin}
          >
            {p.pinned ? '●' : '○'}
          </button>
        )}
        {!p.archived && (
          <button type="button" className="sb-act" aria-label={`New chat in ${p.name}`} title="New chat" onClick={onNew}>
            +
          </button>
        )}
      </span>
    </div>
  );
}

function Indicator({ row }: { row: SidebarRow }) {
  const a = row.attention;
  if (!a) return <span className="sb-ind" aria-hidden="true" />;
  return (
    <span className={`sb-ind sb-ind-${a}`} data-state={a} role="img" aria-label={ATTENTION_LABEL[a]} title={ATTENTION_LABEL[a]}>
      {a === 'pinned' ? '⚲' : null}
    </span>
  );
}

function ChatRow({
  item,
  indent,
  project,
  selected,
  now,
  onOpen,
  onToggle,
}: {
  item: Extract<ListItem, { kind: 'chat' }>;
  indent: number;
  project: ProjectNode | undefined;
  selected: boolean;
  now: number;
  onOpen: () => void;
  onToggle: () => void;
}) {
  const r = item.row;
  const colour = colourCss(project?.colour);
  const trailing = trailingIndicator(r.attention);
  return (
    <div
      className="sb-chat-wrap"
      data-selected={selected ? 'true' : undefined}
      data-trailing={trailing ? 'true' : undefined}
      data-coloured={colour ? 'true' : undefined}
      style={{
        marginLeft: indent + item.depth * 16,
        ...(colour ? { ['--group-colour' as string]: colour } : {}),
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
          {item.expanded ? '⌄' : '›'}
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
        aria-current={selected ? 'true' : undefined}
        onClick={onOpen}
      >
        <Indicator row={r} />
        <span className="sb-chat-text">
          <span className="sb-chat-title">{r.title}</span>
          {item.twoLine && <span className="sb-chat-sub">{project?.name ?? r.folderPath}</span>}
        </span>
        {!item.expanded && item.children > 0 && <span className="sb-agg">{item.children}</span>}
        <span className="sb-age">{age(r.updatedAt, now)}</span>
      </button>
      <span className="sb-row-actions">
        <button type="button" className="sb-act" aria-label={`Options for ${r.title}`} title="Chat options" disabled>
          …
        </button>
      </span>
    </div>
  );
}

/** A clock that ticks once a minute, for row ages. */
function useNow(): number {
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    const t = setInterval(() => setNow(Date.now()), 30_000);
    return () => clearInterval(t);
  }, []);
  return now;
}
