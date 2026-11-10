// Someone else's chat from their computer, read only. It is fetched page by page through the hub and
// held in this window; nothing is saved. When they have allowed household messages, a box sends
// one into the chat; it runs on their computer, under their sign-in, and shows as written by you.

import { useCallback, useEffect, useRef, useState } from 'react';
import type { PeerChat, PeerMessage, PeerTarget } from '../api/profiles.ts';
import { Button, Dialog, TextArea } from '../ui/index.ts';
import type { ProfilesStore } from './store.ts';
import { errorText } from './store.ts';

const PAGE = 30;
const REFRESH_MS = 15_000;

export interface RemoteChat extends PeerTarget {
  chat: PeerChat;
  personName: string;
  machineName: string;
  /** Whether they let household members message their chats. */
  canSend: boolean;
}

const mergeById = (older: PeerMessage[], newer: PeerMessage[]): PeerMessage[] => {
  const seen = new Set(newer.map((m) => m.messageId));
  return [...older.filter((m) => !seen.has(m.messageId)), ...newer];
};

export function RemoteChatDialog({ store, target, onClose }: { store: ProfilesStore; target: RemoteChat; onClose: () => void }) {
  const [messages, setMessages] = useState<PeerMessage[]>([]);
  const [before, setBefore] = useState<number | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [draft, setDraft] = useState('');
  const [sending, setSending] = useState(false);
  const live = useRef(true);
  const { personId, machineId } = target;
  const chatId = target.chat.chatId;

  /** The newest page; keeps any earlier pages already loaded. */
  const refresh = useCallback(async () => {
    try {
      const page = await store.household.readChat({ personId, machineId, chatId, limit: PAGE });
      if (!live.current) return;
      setMessages((prev) => mergeById(prev, page.messages ?? []));
      setBefore((prev) => prev ?? page.beforeMessageId ?? null);
      setError(null);
    } catch (e) {
      if (live.current) setError(errorText(e));
    } finally {
      if (live.current) setLoading(false);
    }
  }, [store, personId, machineId, chatId]);

  useEffect(() => {
    live.current = true;
    void refresh();
    const t = setInterval(() => void refresh(), REFRESH_MS);
    return () => {
      live.current = false;
      clearInterval(t);
    };
  }, [refresh]);

  const earlier = async () => {
    if (before === null) return;
    setLoading(true);
    try {
      const page = await store.household.readChat({ personId, machineId, chatId, beforeMessageId: before, limit: PAGE });
      setMessages((prev) => mergeById(page.messages ?? [], prev));
      setBefore(page.beforeMessageId ?? null);
      setError(null);
    } catch (e) {
      setError(errorText(e));
    } finally {
      setLoading(false);
    }
  };

  const send = async () => {
    const text = draft.trim();
    if (!text || sending) return;
    setSending(true);
    try {
      await store.household.send({ personId, machineId, chatId, message: text });
      setDraft('');
      setError(null);
      void refresh();
    } catch (e) {
      setError(errorText(e));
    } finally {
      setSending(false);
    }
  };

  return (
    <Dialog title={target.chat.title || 'Chat'} onClose={onClose} wide testId="remote-chat" padded>
      <div className="pf-remote">
        <div className="pf-remote-banner" data-testid="remote-chat-banner">
          On {target.machineName}. Read only.
        </div>
        {error && (
          <div className="ui-field-error" role="alert" data-testid="remote-chat-error">
            {error}
          </div>
        )}
        {before !== null && (
          <Button variant="text" disabled={loading} onClick={() => void earlier()} data-testid="remote-chat-earlier">
            Load earlier messages
          </Button>
        )}
        <ol className="pf-remote-list" data-testid="remote-chat-messages">
          {messages.map((m) => (
            <li key={m.messageId} className="pf-remote-msg" data-role={m.role}>
              <div className="pf-remote-who">{m.role === 'user' ? target.personName : m.role === 'assistant' ? 'Assistant' : m.role}</div>
              <div className="pf-remote-text">
                {m.text}
                {m.truncated ? ' …' : ''}
              </div>
            </li>
          ))}
          {!loading && messages.length === 0 && !error && <li className="pf-remote-empty">No messages to show.</li>}
        </ol>
        {target.canSend && (
          <div className="pf-remote-send" data-testid="remote-chat-send">
            <TextArea
              value={draft}
              rows={3}
              placeholder="Write a message into this chat"
              aria-label="Household message"
              onChange={(e) => setDraft(e.target.value)}
              disabled={sending}
            />
            <div className="pf-remote-send-row">
              <span className="pf-remote-hint">It shows as written by you, and the chat runs on {target.machineName}.</span>
              <Button variant="primary" disabled={sending || !draft.trim()} busy={sending} onClick={() => void send()}>
                Send
              </Button>
            </div>
          </div>
        )}
      </div>
    </Dialog>
  );
}
