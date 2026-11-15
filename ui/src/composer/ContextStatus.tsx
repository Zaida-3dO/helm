// "Side panel context on." inside the box, under the attachments, while the side panel's eye is
// on for this chat. Read-only: the toggle itself lives in the toolbar. A chat that runs on
// another PC asks that PC, through the intercepted `panel_context_get`.

import { useEffect, useState, useSyncExternalStore } from 'react';
import type { Api } from '../api/client.ts';
import { panelStore } from '../panel/registry.ts';
import { contextOn } from '../panel/context/store.ts';

export function ContextStatus({ chatId }: { chatId: string }) {
  useSyncExternalStore(panelStore.subscribe, panelStore.getVersion);
  if (!contextOn.get(chatId)) return null;
  return <Note />;
}

/** The same line for a chat that runs on another PC: the eye is the one there. */
export function ContextStatusRemote({ api, chatId }: { api: Api; chatId: string }) {
  const [on, setOn] = useState(false);
  useEffect(() => {
    let live = true;
    setOn(false);
    api<{ on: boolean }>('panel_context_get', { chatId })
      .then((r) => live && setOn(r.on))
      .catch(() => undefined);
    return () => {
      live = false;
    };
  }, [api, chatId]);
  if (!on) return null;
  return <Note />;
}

function Note() {
  return (
    <div className="composer-context-status" data-testid="composer-context-status">
      Side panel context on.
    </div>
  );
}
