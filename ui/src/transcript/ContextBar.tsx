// The chat's context size, quietly, and a prominent Roll over button once it is large. Rolling
// over hands the work to a fresh chat (see sidebar/actions.ts); it is offered only on an idle,
// unarchived chat. With the model's real window known the line says how full the chat is against
// it, and large means near that window (which for a small local window comes well before 250k).

import type { ChatRecord, MessageRow } from '../api/types.ts';
import { Button } from '../ui/index.ts';
import { compactCount, CONTEXT_SHOWN_TOKENS, contextSize, ROLL_OVER_TOKENS, type ChatContextReading } from './rowMeta.ts';

export interface ContextBarProps {
  chat: ChatRecord;
  rows: MessageRow[];
  /** The host's reading: the last measured turn and the model's real window. */
  host: ChatContextReading | null;
  /** The newest rows are not loaded (the user scrolled back): the size would be old. */
  hasNewer: boolean;
  busy: boolean;
  onRollOver: () => void;
}

/** Below this share of the model's window the chat does not read as large. */
const NEAR_WINDOW_PCT = 90;
/** With no window stated, a percent that still earns showing the line. */
const SHOWN_PCT = 60;

export function ContextBar({ chat, rows, host, hasNewer, busy, onRollOver }: ContextBarProps) {
  if (hasNewer || chat.archived || chat.sideSourceId) return null;
  const size = contextSize(rows, host);
  if (!size || (size.tokens < CONTEXT_SHOWN_TOKENS && (size.pct ?? 0) < SHOWN_PCT)) return null;
  const { tokens, window, pct } = size;
  const large = pct !== null ? pct >= NEAR_WINDOW_PCT : tokens >= ROLL_OVER_TOKENS;
  const where = pct !== null ? ` of ${compactCount(window!)} (${Math.round(pct)}%)` : '';
  const label = `${compactCount(tokens)} tokens of context${where}`;
  const title =
    `About ${tokens.toLocaleString()} tokens are in this chat's context` +
    (pct !== null
      ? ` — ${Math.round(pct)}% of the model's ${window!.toLocaleString()}-token window. Over 100% means it no longer fits.`
      : '') +
    `. Long chats get slower and costlier; "Roll over" in the chat menu continues the work in a fresh chat.`;
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
