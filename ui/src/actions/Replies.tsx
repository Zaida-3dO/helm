// Composer chips and inline option buttons. Both read the chat's thread
// view and send as actions, so neither touches the draft. The pencil on an option puts the answer
// into the draft instead of sending it, to be amended.

import { useState, useSyncExternalStore } from 'react';
import { composerApi } from '../composer/slots.ts';
import { useShell } from '../shell/context.ts';
import { Button } from '../ui/index.ts';
import type { ChatStore } from '../state/store.ts';
import { optionCaption, optionDraftText, optionSendText, parseOptions } from './options.ts';
import { answerableReply, quickReplies, type QuickReplyContext } from './quickReplies.ts';
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

/** Option 1, Option A, ... under the latest reply. The pencil puts the answer in the composer. */
export function OptionButtons({ chatId }: Props) {
  const { api, store } = useShell();
  const insertDraft = (text: string) => composerApi(chatId).insert(text);
  const ctx = useReplyContext(store, chatId);
  const [error, setError] = useState<string | null>(null);
  const reply = ctx ? answerableReply(ctx) : null;
  const options = reply ? parseOptions(reply.text) : null;
  if (!options) return null;
  return (
    <div className="option-buttons" data-testid="option-buttons" role="group" aria-label="Pick an option">
      {options.map((o) => (
        <span key={o.label} className="option-pair">
          <Button
            className="option-button"
            data-testid="option-button"
            data-option={o.label}
            title={`${o.title}\nShift+click to amend before sending`}
            onClick={(e) => {
              if (e.shiftKey) {
                insertDraft(optionDraftText(o));
                return;
              }
              setError(null);
              sendActionText(api, chatId, optionSendText(o)).catch((err: unknown) => setError(String((err as Error).message ?? err)));
            }}
          >
            {optionCaption(o)}
          </Button>
          <Button
            variant="icon"
            className="option-amend"
            data-testid="option-amend"
            label={`Amend ${optionCaption(o)} in the draft`}
            title="Put in the draft to amend"
            onClick={() => insertDraft(optionDraftText(o))}
          >
            {'✎'}
          </Button>
        </span>
      ))}
      {error && (
        <span className="error" role="status">
          {error}
        </span>
      )}
    </div>
  );
}
