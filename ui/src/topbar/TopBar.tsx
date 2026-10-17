// The top bar: breadcrumb on the left, chat actions on the right.
// This first version draws the frame: breadcrumb, the panel toggle and the action-row / chat-menu
// slots. Fork, Pin and Archive land with the chat ops.

import { useSyncExternalStore, type ReactNode } from 'react';
import { panelStore, togglePanel } from '../panel/index.ts';
import { useSelection, useShell } from '../shell/context.ts';
import { folderLabel } from './label.ts';

export interface TopBarProps {
  /** The <ActionRow chatId/> (ui/src/actions). */
  actionRow?: ReactNode;
  /** The sidebar's <ChatMenu chatId/> trigger. */
  chatMenu?: ReactNode;
}

export { folderLabel };

export function TopBar({ actionRow, chatMenu }: TopBarProps) {
  const shell = useShell();
  const { chatId } = useSelection();
  const panel = useSyncExternalStore(panelStore.subscribe, () => panelStore.model.visible(chatId));
  const chat = useSyncExternalStore(shell.store.subscribe, () =>
    chatId ? (shell.store.view(chatId).chat ?? shell.store.chatList().find((c) => c.id === chatId) ?? null) : null,
  );
  const project = folderLabel(chat?.cwd);
  return (
    <header className="topbar" data-testid="topbar">
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
        <span className="crumb-title" data-testid="topbar-title">
          {chat?.title ?? (chatId ? '' : 'New chat')}
        </span>
      </nav>
      <div className="topbar-actions">
        {actionRow}
        {chatId && (
          <button type="button" className="icon" data-testid="panel-toggle" aria-pressed={panel} title="Toggle side panel · Ctrl+Shift+I" aria-label="Toggle side panel" onClick={togglePanel}>
            <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" aria-hidden="true">
              <rect x="3" y="3" width="18" height="18" rx="2" />
              <path d="M15 3v18" />
            </svg>
          </button>
        )}
        {chatMenu}
      </div>
    </header>
  );
}
