// The top bar. Left: the breadcrumb (project ›
// chat title, double-click to rename). Right, in order: [update-pill slot] [Swarm slot] Fork, Pin,
// Archive/Restore, the action row, Toggle inspector, [Chat view slot], "…", and at the far right
// the Windows caption buttons (Helm draws its own caption). Below it, a slot for
// the Project / Workspace tags strip (the sidebar fills it).
//
// The bar is a window drag region: a left press that is not on a control starts a native drag, and
// a double-click toggles maximise.

import { useLayoutEffect, useMemo, useRef, useState, useSyncExternalStore, type ReactNode } from 'react';
import { projectOps } from '../api/projects.ts';
import { panelStore, togglePanel } from '../panel/index.ts';
import { useSelection, useShell } from '../shell/context.ts';
import { Button, layers, TextInput, toast, useBinding, useHint } from '../ui/index.ts';
import { IconArchive, IconChevronRight, IconFolder, IconFork, IconPanelLeft, IconPanelRight, IconPin, IconRestore } from '../ui/icons.tsx';
import { sidebarLayout } from '../shell/layout.ts';
import { SidebarActions } from '../sidebar/actions.ts';
import { SidebarData } from '../sidebar/data.ts';
import { sidebarUndo } from '../sidebar/undo.ts';
import { StopArchiveDialog } from '../sidebar/StopArchiveDialog.tsx';
import { UsageMeter } from '../usage/UsageMeter.tsx';
import { CaptionButtons, dragHandlers } from './caption.tsx';
import { topBarTier, type TopBarTier } from './fold.ts';
import { folderLabel, RenameEdit } from './label.ts';

export interface TopBarProps {
  /** The <ActionRow chatId/> (ui/src/actions). */
  actionRow?: ReactNode;
  /** The sidebar's <ChatMenu chatId/> trigger. */
  chatMenu?: ReactNode;
  /** The sidebar's Project / Workspace tags strip, drawn under the bar. */
  tags?: ReactNode;
  /** Reserved: the one-click update pill and the Swarm toggle. */
  updatePill?: ReactNode;
  swarm?: ReactNode;
  /** Reserved: the chat-view picker (Settings → Chat). */
  chatView?: ReactNode;
}

export { folderLabel };

const err = (e: unknown) => toast.show({ kind: 'error', text: String((e as Error).message ?? e) });

export function TopBar({ actionRow, chatMenu, tags, updatePill, swarm, chatView }: TopBarProps) {
  const shell = useShell();
  const { chatId } = useSelection();
  const panel = useSyncExternalStore(panelStore.subscribe, () => panelStore.model.visible(chatId));
  const chat = useSyncExternalStore(shell.store.subscribe, () =>
    chatId ? (shell.store.view(chatId).chat ?? shell.store.chatList().find((c) => c.id === chatId) ?? null) : null,
  );
  const [renaming, setRenaming] = useState<string | null>(null);
  const [confirmStop, setConfirmStop] = useState(false);
  // Archiving from here is the sidebar's archive: it pushes the undo entry and shows the Undo toast.
  const act = useMemo(() => {
    const pops = projectOps(shell.api);
    return new SidebarActions({ api: shell.api, pops, ops: shell.ops, data: new SidebarData(pops), undo: sidebarUndo, onError: (m) => toast.show({ kind: 'error', text: m }) });
  }, [shell.api, shell.ops]);
  const project = folderLabel(chat?.cwd);
  const busy = chat ? chat.status === 'starting' || chat.status === 'running' || chat.status === 'stopping' : false;
  const rename = (title: string) => {
    setRenaming(null);
    const t = title.trim();
    if (!chat || !t || t === chat.title) return;
    shell.api('chat_rename', { chatId: chat.id, title: t }).catch(err);
  };
  const edit = useRef<RenameEdit | null>(null);
  const startRename = (title: string) => {
    edit.current = new RenameEdit(rename);
    setRenaming(title);
  };
  const fork = () => {
    if (chat) shell.ops.fork(chat.id).then((c) => shell.select(c.id)).catch(err);
  };
  // The chat shortcuts the bar advertises. Hints read the live chord. Forking or
  // renaming behind an open dialog would surprise, so the shortcuts wait for it to close.
  const chatShortcutsOn = () => !!chat && !layers.has('dialog');
  useBinding({ id: 'chat.fork', title: 'Fork chat', defaultKeys: 'Ctrl+Shift+F', enabled: chatShortcutsOn, run: fork });
  useBinding({ id: 'chat.rename', title: 'Rename chat', defaultKeys: 'Ctrl+Alt+R', enabled: chatShortcutsOn, run: () => chat && startRename(chat.title) });
  const forkHint = useHint('Fork chat', 'chat.fork');
  const renameHint = useHint('Double-click to rename', 'chat.rename');
  const panelHint = useHint('Toggle inspector', 'panel.toggle');
  const showSidebarHint = useHint('Show sidebar', 'sidebar.toggle');
  // The bar sheds optional parts as the main pane narrows, so the title and the window buttons
  // always fit (they used to be pushed under the side panel).
  const barRef = useRef<HTMLElement | null>(null);
  const [tier, setTier] = useState<TopBarTier>('wide');
  useLayoutEffect(() => {
    const el = barRef.current;
    if (!el) return;
    const measure = () => setTier(topBarTier(el.getBoundingClientRect().width));
    measure();
    const ro = new ResizeObserver(measure);
    ro.observe(el);
    return () => ro.disconnect();
  }, []);
  const sidebarVisible = useSyncExternalStore(sidebarLayout.subscribe, () => sidebarLayout.get().visible);
  return (
    <>
      <header ref={barRef} className="topbar" data-testid="topbar" data-tier={tier} {...dragHandlers()}>
        {!sidebarVisible && (
          <Button variant="icon" label={showSidebarHint} data-testid="show-sidebar" onClick={() => sidebarLayout.setVisible(true)}>
            <IconPanelLeft size={16} />
          </Button>
        )}
        <nav className="crumbs" aria-label="Breadcrumb">
          {project && (
            <>
              <IconFolder className="crumb-folder" size={13} />
              <span className="crumb-project" title={chat?.cwd ?? ''}>
                {project}
              </span>
              <IconChevronRight className="crumb-chevron" size={11} />
            </>
          )}
          {renaming !== null ? (
            <TextInput
              className="crumb-rename"
              autoFocus
              aria-label="Chat title"
              value={renaming}
              onChange={(e) => setRenaming(e.target.value)}
              onBlur={() => edit.current?.commit(renaming)}
              onKeyDown={(e) => {
                if (e.key === 'Enter') edit.current?.commit(renaming);
                if (e.key === 'Escape') {
                  e.stopPropagation();
                  edit.current?.cancel();
                  setRenaming(null);
                }
              }}
            />
          ) : (
            <button
              type="button"
              className="crumb-title"
              data-testid="topbar-title"
              title={chat ? renameHint : undefined}
              onDoubleClick={() => chat && startRename(chat.title)}
              onKeyDown={(e) => {
                if (chat && (e.key === 'Enter' || e.key === ' ')) {
                  e.preventDefault();
                  startRename(chat.title);
                }
              }}
            >
              {chat?.title ?? (chatId ? '' : 'New chat')}
            </button>
          )}
        </nav>
        <div className="topbar-actions">
          <UsageMeter />
          {updatePill}
          {swarm}
          {chat && (
            <span className="topbar-quick">
              <Button variant="icon" label={forkHint} data-testid="topbar-fork" onClick={fork}>
                <IconFork size={16} />
              </Button>
              <Button
                variant="icon"
                label={chat.pinned ? 'Unpin chat' : 'Pin chat'}
                data-testid="topbar-pin"
                onClick={() => shell.api('chat_organize', { chatId: chat.id, pinned: !chat.pinned }).catch(err)}
              >
                <IconPin size={15} filled={chat.pinned} />
              </Button>
              {chat.archived ? (
                <Button variant="icon" label="Restore chat" data-testid="topbar-archive" onClick={() => shell.ops.restore(chat.id).catch(err)}>
                  <IconRestore size={16} />
                </Button>
              ) : (
                <Button
                  variant="icon"
                  label={busy ? 'Stop and archive task…' : 'Archive chat'}
                  data-testid="topbar-archive"
                  onClick={() => (busy ? setConfirmStop(true) : void act.archive({ chatId: chat.id, title: chat.title }))}
                >
                  <IconArchive size={16} />
                </Button>
              )}
            </span>
          )}
          {actionRow}
          {chat && (
            <Button variant="icon" label={panelHint} pressed={panel} data-testid="panel-toggle" onClick={togglePanel}>
              <IconPanelRight size={16} />
            </Button>
          )}
          {chatView}
          {chatMenu}
        </div>
        <CaptionButtons />
      </header>
      {confirmStop && chat && (
        <StopArchiveDialog title={chat.title} onClose={() => setConfirmStop(false)} onConfirm={() => void act.stopAndArchive({ chatId: chat.id, title: chat.title })} />
      )}
      {tags && <div className="tags-strip">{tags}</div>}
    </>
  );
}
