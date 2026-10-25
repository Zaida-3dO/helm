// "Side panel context on." inside the box, under the attachments, while the side panel's eye is
// on for this chat. Read-only: the toggle itself lives in the toolbar.

import { useSyncExternalStore } from 'react';
import { panelStore } from '../panel/registry.ts';
import { contextOn } from '../panel/context/store.ts';

export function ContextStatus({ chatId }: { chatId: string }) {
  useSyncExternalStore(panelStore.subscribe, panelStore.getVersion);
  if (!contextOn.get(chatId)) return null;
  return (
    <div className="composer-context-status" data-testid="composer-context-status">
      Side panel context on.
    </div>
  );
}
