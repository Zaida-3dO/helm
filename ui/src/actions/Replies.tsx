// Composer chips. They read the chat's thread view and send as actions, so they never touch the draft.

import { useState, useSyncExternalStore } from 'react';
import { useShell } from '../shell/context.ts';
import { Button } from '../ui/index.ts';
import type { ChatStore } from '../state/store.ts';
import { quickReplies, type QuickReplyContext } from './quickReplies.ts';
import { sendActionText } from './send.ts';
import './actions.css';

function useReplyContext(store: ChatStore, chatId: string): QuickReplyContext | null {
  const view = useSyncExternalStore(store.subscribe, () => store.view(chatId));
  if (!view.chat) return null;
  return { status: view.chat.status, rows: view.rows, interactionOpen: (view.thread?.interactions.length ?? 0) > 0 };
}

interface Props {
  chatId: string;
}

/** Continue (only after a stopped or errored turn), and Yes and No (only after a question). */
export function QuickReplies({ chatId }: Props) {
  const { api, store } = useShell();
  const ctx = useReplyContext(store, chatId);
  const [error, setError] = useState<string | null>(null);
  const chips = ctx ? quickReplies(ctx) : [];
  if (chips.length === 0) return null;
  return (
    <div className="quick-replies" data-testid="quick-replies">
      {chips.map((q) => (
        <Button
          key={q.id}
          className="quick-reply"
          data-testid="quick-reply"
          data-reply={q.id}
          label={q.hint}
          onClick={() => {
            setError(null);
            sendActionText(api, chatId, q.text).catch((e: unknown) => setError(String((e as Error).message ?? e)));
          }}
        >
          {q.label}
        </Button>
      ))}
      {error && (
        <span className="error" role="status">
          {error}
        </span>
      )}
    </div>
  );
}
