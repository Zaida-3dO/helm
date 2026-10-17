import { useEffect, useMemo, useState } from 'react';
import type { Api } from './api/client.ts';
import type { EventBus } from './api/events.ts';
import { chatOps } from './api/ops.ts';
import { ChatList } from './chatlist/ChatList.tsx';
import { ChatView } from './ChatView.tsx';
import type { SendShortcut } from './composer/keymap.ts';
import type { Flushers } from './shell/quit.ts';
import { ChatStore } from './state/store.ts';

/** The temporary chat list beside one chat, until the app shell replaces this frame. */
export function App({ api, bus, flushers }: { api: Api; bus: EventBus; flushers: Flushers }) {
  const ops = useMemo(() => chatOps(api), [api]);
  const store = useMemo(() => new ChatStore(), []);
  const [selected, setSelected] = useState<string | null>(null);
  const [sendShortcut, setSendShortcut] = useState<SendShortcut>('mod-enter');

  useEffect(() => store.connect(bus), [store, bus]);
  useEffect(() => {
    // The send shortcut setting; missing or unreadable means the default.
    api<unknown>('preference_get', { key: 'sendShortcut' })
      .then((v) => {
        if (v === 'enter' || v === 'mod-enter') setSendShortcut(v);
      })
      .catch(() => undefined);
  }, [api]);

  return (
    <div className="shell">
      <aside className="sidebar" aria-label="Sidebar">
        <div className="brand">Helm</div>
        <ChatList store={store} ops={ops} selected={selected} onSelect={setSelected} />
        <button type="button" className="small quit" onClick={() => void api('quit_request')}>
          Quit
        </button>
      </aside>
      <main className="main">
        {selected ? (
          <ChatView key={selected} chatId={selected} store={store} ops={ops} flushers={flushers} sendShortcut={sendShortcut} />
        ) : (
          <div className="empty-main muted">Pick a chat, or enter a folder and press New chat.</div>
        )}
      </main>
    </div>
  );
}
