// Interaction cards in the look of the question, approval and plan
// dialog: a 520 px card in the column above the composer, radius
// 18, a soft accent glow in its top-left corner and an accent hairline on top, an eyebrow badge,
// and the choices as numbered option cards. It stays inline rather than modal (no backdrop),
// so the transcript stays readable and clickable while it waits.
//
// Approvals offer exactly the options the worker sent (an over-64 KiB request comes with only Deny
// and Stop turn). Questions go one step at a time with free text when allowed and a way back to
// earlier steps. Plans offer approve or keep planning. With focus in the card, 1–9 pick an option;
// keys in the first 400 ms after the card appears are ignored, so typing cannot answer by accident.
// A dismissal is the worker removing the request from the thread, which removes the card.

import { useEffect, useRef, useState, type KeyboardEvent } from 'react';
import type { Interaction } from '../api/types.ts';
import type { InteractionAnswer } from '../api/ops.ts';
import { IconChevronRight, IconHelp, IconPlan, IconSend, IconShield } from '../ui/icons.tsx';
import { Markdown } from './Markdown.tsx';
import { defaultIndex, eyebrowOf, optionAnswer, pickIndex, splitRecommended, KEY_GUARD_MS } from './interactionModel.ts';
import '../composer/dock.css';

export interface InteractionCardProps {
  interaction: Interaction;
  onAnswer: (requestId: string, answer: InteractionAnswer) => void;
  /** Shown inside the answer popup: a question's default option takes focus, so Enter sends it. */
  popup?: boolean;
}

function Context({ i }: { i: Interaction }) {
  if (!i.context?.length) return null;
  return (
    <dl className="ia-context">
      {i.context.map((c, n) => (
        <div key={n}>
          <dt>{c.label}</dt>
          <dd>{c.markdown ? <Markdown text={c.text} /> : <pre className="mono">{c.text}</pre>}</dd>
        </div>
      ))}
    </dl>
  );
}

function Badge({ kind }: { kind: string }) {
  return (
    <span className="ia-badge" aria-hidden="true">
      {kind === 'question' ? <IconHelp size={12} /> : kind === 'plan' ? <IconPlan size={12} /> : <IconShield size={12} />}
    </span>
  );
}

export function InteractionCard({ interaction: i, onAnswer, popup }: InteractionCardProps) {
  const [other, setOther] = useState(i.prefill ?? '');
  const shownAt = useRef(performance.now());
  const card = useRef<HTMLDivElement>(null);
  useEffect(() => {
    shownAt.current = performance.now();
  }, [i.id, i.step]);
  const answer = (a: InteractionAnswer) => onAnswer(i.id, a);
  const kind = i.attention ?? 'approval';
  const options = i.options ?? [];
  const testId = kind === 'plan' ? 'plan-button' : kind === 'question' ? 'question-button' : 'approval-button';
  const focusAt = popup ? defaultIndex(kind, options) : null;
  const confirmOnly = kind !== 'plan' && kind !== 'question' && i.method === 'confirm' && options.length === 0;

  const onKey = (e: KeyboardEvent<HTMLDivElement>) => {
    if (performance.now() - shownAt.current < KEY_GUARD_MS) {
      e.preventDefault();
      return;
    }
    const target = e.target as HTMLElement;
    if (target.closest('input, textarea')) return;
    const n = pickIndex(e.key, options.length);
    if (n === null || e.ctrlKey || e.altKey || e.metaKey) return;
    e.preventDefault();
    answer(optionAnswer(kind, options[n]!));
  };

  return (
    <div className={`ia-wrap${popup ? ' ia-in-popup' : ''}`}>
      <div ref={card} className={`ia-card ia-${kind}`} data-testid="interaction-card" data-attention={kind} onKeyDown={onKey}>
        <div className="ia-head">
          <Badge kind={kind} />
          <span className="ia-eyebrow">{eyebrowOf(kind)}</span>
          {i.header && <span className="ia-chip">{i.header}</span>}
          {kind === 'question' && i.steps && i.steps > 1 && (
            <span className="ia-step">
              {(i.step ?? 0) + 1} of {i.steps}
            </span>
          )}
        </div>
        <div className="ia-body">
          <div className="ia-title">{i.title || 'Your agent needs input'}</div>
          {i.message && (
            <div className="ia-message">
              <Markdown text={i.message} />
            </div>
          )}
          <Context i={i} />

          {options.length > 0 && (
            <div className="ia-options" role="group" aria-label="Choices">
              {options.map((o, n) => {
                const chosen = kind === 'question' && i.prefill === o;
                const tag = splitRecommended(o);
                const danger = kind !== 'plan' && kind !== 'question' && (o === 'Deny' || o === 'Stop turn');
                return (
                  <button
                    key={o}
                    type="button"
                    className={`ia-option${chosen ? ' chosen' : ''}${danger ? ' ia-danger' : ''}`}
                    data-testid={testId}
                    data-option={o}
                    data-autofocus={focusAt === n ? '' : undefined}
                    title={i.optionDescriptions?.[n]}
                    onClick={() => answer(optionAnswer(kind, o))}
                  >
                    <span className="ia-num">{n < 9 ? n + 1 : ''}</span>
                    <span className="ia-option-text">
                      <span className="ia-option-label">{tag.label}</span>
                      {i.optionDescriptions?.[n] && <span className="ia-option-desc">{i.optionDescriptions[n]}</span>}
                    </span>
                    {tag.recommended && <span className="ia-pill">Recommended</span>}
                    {chosen && <span className="ia-pill">Your answer</span>}
                    <span className="ia-arrow" aria-hidden="true">
                      <IconChevronRight size={14} />
                    </span>
                  </button>
                );
              })}
            </div>
          )}

          {kind === 'question' && i.allowOther && (
            <form
              className="ia-other"
              onSubmit={(e) => {
                e.preventDefault();
                if (other.trim()) answer({ value: other });
              }}
            >
              <input
                value={other}
                onChange={(e) => setOther(e.target.value)}
                placeholder="Something else…"
                type={i.secret ? 'password' : 'text'}
                data-testid="question-other"
                aria-label="Your own answer"
              />
              <button type="submit" className="ia-send" data-testid="question-button" data-option="(other)" aria-label="Answer" disabled={!other.trim()}>
                <IconSend size={14} />
              </button>
            </form>
          )}

          {confirmOnly && (
            <div className="ia-footer-buttons">
              <button type="button" className="ui-btn" data-testid="approval-button" data-option="no" onClick={() => answer({ confirmed: false })}>
                No
              </button>
              <button type="button" className="ui-btn ui-btn-primary" data-testid="approval-button" data-option="yes" onClick={() => answer({ confirmed: true })}>
                Confirm
              </button>
            </div>
          )}
        </div>
        <div className="ia-hints">
          <span>
            {options.length > 1 ? `1–${Math.min(9, options.length)} choose` : options.length === 1 ? '1 choose' : ''}
          </span>
          {kind === 'question' && (
            <span className="ia-hint-actions">
              {(i.step ?? 0) > 0 && (
                <button type="button" className="ia-link" data-testid="question-back" onClick={() => answer({ goto: (i.step ?? 0) - 1, value: other || undefined })}>
                  Back
                </button>
              )}
              <button type="button" className="ia-link" data-testid="question-cancel" onClick={() => answer({ cancelled: true })}>
                Skip
              </button>
            </span>
          )}
        </div>
      </div>
    </div>
  );
}
