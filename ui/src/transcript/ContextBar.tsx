// The chat's context size, quietly, and a prominent Roll over button once it is large. Rolling
// over hands the work to a fresh chat (see sidebar/actions.ts); it is offered only on an idle,
// unarchived chat.

import type { ChatRecord, MessageRow } from '../api/types.ts';
import { Button } from '../ui/index.ts';
import { compactCount, CONTEXT_SHOWN_TOKENS, contextTokens, ROLL_OVER_TOKENS } from './rowMeta.ts';

export interface ContextBarProps {
  chat: ChatRecord;
  rows: MessageRow[];
  /** The newest rows are not loaded (the user scrolled back): the size would be old. */
  hasNewer: boolean;
  busy: boolean;
  onRollOver: () => void;
}

export function ContextBar({ chat, rows, hasNewer, busy, onRollOver }: ContextBarProps) {
  if (hasNewer || chat.archived || chat.sideSourceId) return null;
  const tokens = contextTokens(rows);
  if (tokens === null || tokens < CONTEXT_SHOWN_TOKENS) return null;
  const large = tokens >= ROLL_OVER_TOKENS;
  const label = `${compactCount(tokens)} tokens of context`;
  const title = `About ${tokens.toLocaleString()} tokens are in this chat's context. Long chats get slower and costlier; "Roll over" in the chat menu continues the work in a fresh chat.`;
  if (!large) {
    return (
      <div className="context-size" data-testid="context-size" title={title}>
        {label}
      </div>
    );
  }
  return (
    <div className="context-bar" data-testid="context-bar" role="status">
      <span title={title}>This chat is large ({label}). A fresh chat with a hand-off keeps the work moving faster.</span>
      <Button variant="secondary" disabled={busy} onClick={onRollOver} data-testid="roll-over-button">
        Roll over
      </Button>
    </div>
  );
}
