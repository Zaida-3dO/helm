// "Answer question": one button above the composer when the chat is waiting on an answer, and the
// popup it opens.
//
// Three things can be waiting: a question, plan or permission card from the agent (the popup then
// hosts that same card, answered through the same call, so nothing about how an approval is
// answered changes), or the options in the latest reply (the popup writes the question and every
// option out in full). A reply's answer is sent as an action, like the chips.

import { useEffect, useMemo, useState, useSyncExternalStore, type KeyboardEvent } from 'react';
import { chatOps } from '../api/ops.ts';
import { useShell } from '../shell/context.ts';
import type { ChatStore } from '../state/store.ts';
import { InteractionCard } from '../transcript/InteractionCard.tsx';
import { KEY_GUARD_MS, pickIndex } from '../transcript/interactionModel.ts';
import { Markdown } from '../transcript/Markdown.tsx';
import { Button, Dialog } from '../ui/index.ts';
import { IconHelp, IconSend } from '../ui/icons.tsx';
import { answerLabel, answerText, defaultOption, parseReplyQuestion, type ReplyQuestion } from './options.ts';
import { answerableReply } from './quickReplies.ts';
import { sendActionText } from './send.ts';
import '../composer/dock.css';
import './actions.css';

interface Props {
  chatId: string;
}

function useWaiting(store: ChatStore, chatId: string) {
  const view = useSyncExternalStore(store.subscribe, () => store.view(chatId));
  const interactions = view.thread?.interactions ?? [];
  const reply = view.chat
    ? answerableReply({ status: view.chat.status, rows: view.rows, interactionOpen: interactions.length > 0 })
    : null;
  const replyText = reply?.text ?? null;
  const asked = useMemo(() => (replyText ? parseReplyQuestion(replyText) : null), [replyText]);
  return { interactions, asked };
}

export function AnswerButton({ chatId }: Props) {
  const { api, store } = useShell();
  const { interactions, asked } = useWaiting(store, chatId);
  const [open, setOpen] = useState(false);
  const first = interactions[0];
  const waiting = !!first || !!asked;
  // Once nothing is waiting the popup is gone; a later question must not open it by itself.
  useEffect(() => {
    if (!waiting) setOpen(false);
  }, [waiting]);
  if (!waiting) return null;
  const count = interactions.reduce((n, i) => n + (i.attention === 'question' ? Math.max(1, i.steps ?? 1) : 1), 0) || 1;
  return (
    <div className="answer-bar" data-testid="answer-bar">
      <Button className="answer-button" data-testid="answer-button" onClick={() => setOpen(true)}>
        <IconHelp size={14} />
        {answerLabel(count)}
      </Button>
      {open && first && (
        <Dialog title="Answer question" wide testId="answer-popup" onClose={() => setOpen(false)}>
          <InteractionCard
            key={first.id}
            interaction={first}
            popup
            onAnswer={(id, a) => void chatOps(api).answer(chatId, id, a).catch(() => undefined)}
          />
        </Dialog>
      )}
      {open && !first && asked && <ReplyAnswerDialog chatId={chatId} asked={asked} onClose={() => setOpen(false)} />}
    </div>
  );
}

function ReplyAnswerDialog({ chatId, asked, onClose }: Props & { asked: ReplyQuestion; onClose: () => void }) {
  const { api } = useShell();
  const { options, multi } = asked;
  const [picked, setPicked] = useState<string[]>(() => {
    const d = defaultOption(options);
    return d ? [d.label] : [];
  });
  const [other, setOther] = useState('');
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [shownAt] = useState(() => performance.now());

  const chosen = options.filter((o) => picked.includes(o.label));
  const text = answerText(chosen, other);
  const focusAt = Math.max(0, options.findIndex((o) => picked.includes(o.label)));

  const choose = (label: string) => {
    if (multi) {
      setPicked((p) => (p.includes(label) ? p.filter((x) => x !== label) : [...p, label]));
    } else {
      setPicked([label]);
      setOther('');
    }
  };

  const send = () => {
    if (!text || busy) return;
    setBusy(true);
    setError(null);
    sendActionText(api, chatId, text).then(
      () => onClose(),
      (e: unknown) => {
        setBusy(false);
        setError(String((e as Error).message ?? e));
      },
    );
  };

  const onKey = (e: KeyboardEvent<HTMLDivElement>) => {
    if (performance.now() - shownAt < KEY_GUARD_MS) {
      e.preventDefault();
      return;
    }
    if (e.ctrlKey || e.altKey || e.metaKey) return;
    if (e.key === 'Enter') {
      e.preventDefault();
      // With nothing chosen yet, Enter on an option chooses it; otherwise it sends.
      const label = (e.target as HTMLElement).closest<HTMLElement>('[data-label]')?.dataset.label;
      if (!text && label) choose(label);
      else send();
      return;
    }
    if ((e.target as HTMLElement).closest('input, textarea')) return;
    const n = pickIndex(e.key, options.length);
    if (n === null) return;
    e.preventDefault();
    const o = options[n]!;
    choose(o.label);
    // Keep focus on the option just picked, so the focus ring follows the choice.
    e.currentTarget.querySelector<HTMLElement>(`[data-label="${o.label}"]`)?.focus();
  };

  return (
    <Dialog
      title="Answer question"
      wide
      busy={busy}
      testId="answer-popup"
      onClose={onClose}
      footer={
        <>
          <span className="aq-hints">
            {options.length > 1 ? `1–${Math.min(9, options.length)} ${multi ? 'toggle' : 'choose'} · ` : ''}Enter sends · Esc closes
          </span>
          <Button variant="primary" data-testid="answer-send" disabled={!text || busy} busy={busy} onClick={send}>
            <IconSend size={13} />
            Send
          </Button>
        </>
      }
    >
      <div className="aq-body" onKeyDown={onKey}>
        {asked.question && (
          <div className="aq-question" data-testid="answer-question">
            <Markdown text={asked.question} />
          </div>
        )}
        <div className="ia-options" role={multi ? 'group' : 'radiogroup'} aria-label="Choices">
          {options.map((o, n) => {
            const on = picked.includes(o.label);
            return (
              <button
                key={o.label}
                type="button"
                className="ia-option aq-option"
                role={multi ? 'checkbox' : 'radio'}
                aria-checked={on}
                data-testid="answer-option"
                data-label={o.label}
                data-autofocus={n === focusAt ? '' : undefined}
                onClick={() => choose(o.label)}
              >
                <span className="ia-num">{n < 9 ? n + 1 : ''}</span>
                <span className="ia-option-text">
                  <span className="ia-option-label">{o.text || `Option ${o.label}`}</span>
                </span>
                {o.recommended && <span className="ia-pill">Recommended</span>}
              </button>
            );
          })}
        </div>
        <input
          className="ui-input aq-other"
          value={other}
          placeholder="Something else…"
          aria-label="Your own answer"
          data-testid="answer-other"
          onChange={(e) => {
            setOther(e.target.value);
            if (!multi && e.target.value) setPicked([]);
          }}
        />
        {error && (
          <span className="error" role="status">
            {error}
          </span>
        )}
      </div>
    </Dialog>
  );
}
