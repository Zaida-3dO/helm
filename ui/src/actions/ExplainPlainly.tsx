// "Explain plainly": a message action on each finished assistant reply.
// It sends the fixed prompt as an action, so the draft is left alone.

import { useState } from 'react';
import type { Api } from '../api/client.ts';
import type { MessageRow } from '../api/types.ts';
import { Button } from '../ui/index.ts';
import { IconExplain } from '../ui/icons.tsx';
import { EXPLAIN_PLAINLY_TEXT, sendActionText } from './send.ts';
import './actions.css';

/** The action's icon. A message-action registration with an icon field passes this. */
export const ExplainPlainlyIcon = IconExplain;

/** Which rows get the action: a finished assistant reply with text. */
export function explainAppliesTo(row: Pick<MessageRow, 'role' | 'status' | 'text'>): boolean {
  return row.role === 'assistant' && row.status === 'complete' && row.text.trim() !== '';
}

export function ExplainPlainly({ api, chatId }: { api: Api; chatId: string }) {
  const [error, setError] = useState<string | null>(null);
  return (
    <>
      <Button
        variant="text"
        className="message-action explain-plainly"
        data-testid="explain-plainly"
        title={EXPLAIN_PLAINLY_TEXT}
        onClick={() => {
          setError(null);
          sendActionText(api, chatId, EXPLAIN_PLAINLY_TEXT).catch((e: unknown) => setError(String((e as Error).message ?? e)));
        }}
      >
        <IconExplain />
        Explain plainly
      </Button>
      {error && (
        <span className="error" role="status">
          {error}
        </span>
      )}
    </>
  );
}
