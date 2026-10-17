// Interaction cards. Approvals offer exactly the options the worker
// sent (so an over-64 KiB request, which the worker sends with only Deny and Stop turn, shows just
// those). Questions go one step at a time with free text when allowed and a way back to earlier
// steps. Plans offer approve or keep planning. A dismissal is the worker removing the request from
// the thread, which removes the card.

import { useState } from 'react';
import type { Interaction } from '../api/types.ts';
import type { InteractionAnswer } from '../api/ops.ts';
import { Markdown } from './Markdown.tsx';

export interface InteractionCardProps {
  interaction: Interaction;
  onAnswer: (requestId: string, answer: InteractionAnswer) => void;
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

export function InteractionCard({ interaction: i, onAnswer }: InteractionCardProps) {
  const [other, setOther] = useState(i.prefill ?? '');
  const answer = (a: InteractionAnswer) => onAnswer(i.id, a);
  const kind = i.attention ?? 'approval';
  const options = i.options ?? [];

  return (
    <div className={`ia-card ia-${kind}`} data-testid="interaction-card" data-attention={kind}>
      <div className="ia-title">
        {i.header && <span className="muted">{i.header} · </span>}
        {i.title}
        {kind === 'question' && i.steps && i.steps > 1 && (
          <span className="muted"> · step {(i.step ?? 0) + 1} of {i.steps}</span>
        )}
      </div>
      {i.message && <Markdown text={i.message} />}
      <Context i={i} />

      {kind === 'plan' && (
        <div className="ia-buttons">
          {options.map((o) => (
            <button
              key={o}
              type="button"
              data-testid="plan-button"
              data-option={o}
              className={o === 'Approve plan' ? 'primary' : ''}
              onClick={() => answer(o === 'Approve plan' ? { value: o, confirmed: true } : { value: o, confirmed: false })}
            >
              {o}
            </button>
          ))}
        </div>
      )}

      {kind === 'question' && (
        <>
          <div className="ia-buttons">
            {options.map((o, n) => (
              <button
                key={o}
                type="button"
                data-testid="question-button"
                data-option={o}
                className={i.prefill === o ? 'primary' : ''}
                title={i.optionDescriptions?.[n]}
                onClick={() => answer({ value: o })}
              >
                {o}
              </button>
            ))}
          </div>
          {i.allowOther && (
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
              />
              <button type="submit" data-testid="question-button" data-option="(other)">
                Answer
              </button>
            </form>
          )}
          <div className="ia-buttons">
            {(i.step ?? 0) > 0 && (
              <button type="button" className="small" data-testid="question-back" onClick={() => answer({ goto: (i.step ?? 0) - 1, value: other || undefined })}>
                Back
              </button>
            )}
            <button type="button" className="small" data-testid="question-cancel" onClick={() => answer({ cancelled: true })}>
              Skip
            </button>
          </div>
        </>
      )}

      {kind !== 'plan' && kind !== 'question' && (
        <div className="ia-buttons">
          {options.map((o, n) => (
            <button
              key={o}
              type="button"
              data-testid="approval-button"
              data-option={o}
              className={n === 0 ? 'primary' : o === 'Deny' || o === 'Stop turn' ? 'danger' : ''}
              title={i.optionDescriptions?.[n]}
              onClick={() => answer({ value: o })}
            >
              {o}
            </button>
          ))}
          {i.method === 'confirm' && options.length === 0 && (
            <>
              <button type="button" data-testid="approval-button" data-option="yes" onClick={() => answer({ confirmed: true })}>
                Yes
              </button>
              <button type="button" data-testid="approval-button" data-option="no" onClick={() => answer({ confirmed: false })}>
                No
              </button>
            </>
          )}
        </div>
      )}
    </div>
  );
}
