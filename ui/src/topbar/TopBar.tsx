// The top bar. Left: the breadcrumb (project ›
// chat title, double-click to rename). Right, in order: [update-pill slot] [Swarm slot] Fork, Pin,
// Archive/Restore, the action row, Toggle inspector, [Chat view slot], "…", and at the far right
// the Windows caption buttons (Helm draws its own caption). Below it, a slot for
// the Project / Workspace tags strip (the sidebar fills it).
//
// The bar is a window drag region: a left press that is not on a control starts a native drag, and
// a double-click toggles maximise.

import { useRef, useState, useSyncExternalStore, type ReactNode } from 'react';
import { panelStore, togglePanel } from '../panel/index.ts';
import { useSelection, useShell } from '../shell/context.ts';
import { Button, TextInput, toast } from '../ui/index.ts';
import { IconArchive, IconChevronRight, IconFork, IconPanelRight, IconPin, IconRestore } from '../ui/icons.tsx';
import { UsageMeter } from '../usage/UsageMeter.tsx';
import { CaptionButtons, dragHandlers } from './caption.tsx';
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
  return (
    <>
      <header className="topbar" data-testid="topbar" {...dragHandlers()}>
        <nav className="crumbs" aria-label="Breadcrumb">
          {project && (
            <>
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
              title={chat ? 'Double-click to rename' : undefined}
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
            <>
              <Button variant="icon" label="Fork chat · Ctrl+Shift+F" data-testid="topbar-fork" onClick={() => shell.ops.fork(chat.id).then((c) => shell.select(c.id)).catch(err)}>
                <IconFork size={16} />
              </Button>
              <Button
                variant="icon"
                label={chat.pinned ? 'Unpin chat' : 'Pin chat'}
                pressed={chat.pinned}
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
                  onClick={() => shell.ops.archive(chat.id).catch(err)}
                >
                  <IconArchive size={16} />
                </Button>
              )}
            </>
          )}
          {actionRow}
          {chat && (
            <Button variant="icon" label="Toggle inspector · Ctrl+Shift+I" pressed={panel} data-testid="panel-toggle" onClick={togglePanel}>
              <IconPanelRight size={16} />
            </Button>
          )}
          {chatView}
          {chatMenu}
        </div>
        <CaptionButtons />
      </header>
      {tags && <div className="tags-strip">{tags}</div>}
    </>
  );
}
