// The temporary chat list: a folder input whose last value is remembered, "New
// chat", and the chats newest first. The grouped sidebar replaces it.

import { useEffect, useState, useSyncExternalStore } from 'react';
import type { ChatOps } from '../api/ops.ts';
import type { ChatStore } from '../state/store.ts';

export const LAST_FOLDER_KEY = 'helm.lastFolder';

export interface ChatListProps {
  store: ChatStore;
  ops: ChatOps;
  selected: string | null;
  onSelect: (chatId: string) => void;
}

export function ChatList({ store, ops, selected, onSelect }: ChatListProps) {
  const chats = useSyncExternalStore(store.subscribe, () => store.chatList());
  const [folder, setFolder] = useState(() => localStorage.getItem(LAST_FOLDER_KEY) ?? '');
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    ops
      .list()
      .then((list) => store.setChatList(list))
      .catch((e: unknown) => setError(String((e as Error).message ?? e)));
  }, [ops, store]);

  const create = async () => {
    const cwd = folder.trim();
    if (!cwd) {
      setError('Enter a folder for the chat to work in.');
      return;
    }
    setError(null);
    localStorage.setItem(LAST_FOLDER_KEY, cwd);
    try {
      const chat = await ops.create(cwd);
      store.chat(chat);
      onSelect(chat.id);
    } catch (e) {
      setError(String((e as Error).message ?? e));
    }
  };

  return (
    <nav className="chat-list" aria-label="Chats">
      <form
        className="new-chat"
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
        <button type="submit" data-testid="new-chat">
          New chat
        </button>
      </form>
      {error && <p className="error small-text">{error}</p>}
      <ul>
        {chats.map((c) => (
          <li key={c.id}>
            <button
              type="button"
              className={`chat-item ${c.id === selected ? 'selected' : ''}`}
              data-testid="chat-item"
              data-chat-id={c.id}
              data-status={c.status}
              onClick={() => onSelect(c.id)}
            >
              <span className="chat-title">{c.title}</span>
              <span className="muted small-text">{c.status === 'idle' ? (c.draft ? 'Draft' : '') : c.status}</span>
            </button>
          </li>
        ))}
      </ul>
    </nav>
  );
}
