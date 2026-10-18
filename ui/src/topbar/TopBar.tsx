// The top bar. Left: the breadcrumb (project ›
// chat title, double-click to rename). Right, in order: [update-pill slot] [Swarm slot] Fork, Pin,
// Archive/Restore, the action row, Toggle inspector, [Chat view slot], "…", and at the far right
// the Windows caption buttons (Helm draws its own caption). Below it, a slot for
// the Project / Workspace tags strip (the sidebar fills it).
//
// The bar is a window drag region: a left press that is not on a control starts a native drag, and
// a double-click toggles maximise.

import { useState, useSyncExternalStore, type ReactNode } from 'react';
import { panelStore, togglePanel } from '../panel/index.ts';
import { useSelection, useShell } from '../shell/context.ts';
import { Button, TextInput, toast } from '../ui/index.ts';
import { UsageMeter } from '../usage/UsageMeter.tsx';
import { CaptionButtons, dragHandlers } from './caption.tsx';
import { folderLabel } from './label.ts';

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

const I = {
  fork: (
    <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" aria-hidden="true">
      <circle cx="6" cy="5" r="2" />
      <circle cx="6" cy="19" r="2" />
      <circle cx="18" cy="7" r="2" />
      <path d="M6 7v10M18 9c0 5-6 4-11 8" />
    </svg>
  ),
  pin: (filled: boolean) => (
    <svg width="15" height="15" viewBox="0 0 24 24" fill={filled ? 'currentColor' : 'none'} stroke="currentColor" strokeWidth="2" strokeLinejoin="round" aria-hidden="true">
      <path d="M12 17v5M9 3h6l-1 6 4 4H6l4-4z" />
    </svg>
  ),
  archive: (
    <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinejoin="round" aria-hidden="true">
      <rect x="3" y="4" width="18" height="4" rx="1" />
      <path d="M5 8v11a1 1 0 0 0 1 1h12a1 1 0 0 0 1-1V8M10 12h4" />
    </svg>
  ),
  restore: (
    <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" aria-hidden="true">
      <path d="M3 12a9 9 0 1 0 3-6.7L3 8M3 3v5h5" />
    </svg>
  ),
  panel: (
    <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" aria-hidden="true">
      <rect x="3" y="3" width="18" height="18" rx="2" />
      <path d="M15 3v18" />
    </svg>
  ),
};

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
  return (
    <>
      <header className="topbar" data-testid="topbar" {...dragHandlers()}>
        <nav className="crumbs" aria-label="Breadcrumb">
          {project && (
            <>
              <span className="crumb-project" title={chat?.cwd ?? ''}>
                {project}
              </span>
              <svg className="crumb-chevron" width="11" height="11" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" aria-hidden="true">
                <path d="m9 18 6-6-6-6" />
              </svg>
            </>
          )}
          {renaming !== null ? (
            <TextInput
              className="crumb-rename"
              autoFocus
              aria-label="Chat title"
              value={renaming}
              onChange={(e) => setRenaming(e.target.value)}
              onBlur={() => rename(renaming)}
              onKeyDown={(e) => {
                if (e.key === 'Enter') rename(renaming);
                if (e.key === 'Escape') {
                  e.stopPropagation();
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
              onDoubleClick={() => chat && setRenaming(chat.title)}
              onKeyDown={(e) => {
                if (chat && (e.key === 'Enter' || e.key === ' ')) {
                  e.preventDefault();
                  setRenaming(chat.title);
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
                {I.fork}
              </Button>
              <Button
                variant="icon"
                label={chat.pinned ? 'Unpin chat' : 'Pin chat'}
                pressed={chat.pinned}
                data-testid="topbar-pin"
                onClick={() => shell.api('chat_organize', { chatId: chat.id, pinned: !chat.pinned }).catch(err)}
              >
                {I.pin(chat.pinned)}
              </Button>
              {chat.archived ? (
                <Button variant="icon" label="Restore chat" data-testid="topbar-archive" onClick={() => shell.ops.restore(chat.id).catch(err)}>
                  {I.restore}
                </Button>
              ) : (
                <Button
                  variant="icon"
                  label={busy ? 'Stop and archive task…' : 'Archive chat'}
                  data-testid="topbar-archive"
                  onClick={() => shell.ops.archive(chat.id).catch(err)}
                >
                  {I.archive}
                </Button>
              )}
            </>
          )}
          {actionRow}
          {chat && (
            <Button variant="icon" label="Toggle inspector · Ctrl+Shift+I" pressed={panel} data-testid="panel-toggle" onClick={togglePanel}>
              {I.panel}
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
