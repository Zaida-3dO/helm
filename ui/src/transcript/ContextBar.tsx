// The chat's context size, quietly, and a prominent bar once it is large — with a Roll over
// button, and for an outgrown local model first a Switch to the model Settings suggests. Rolling
// over hands the work to a fresh chat (see sidebar/actions.ts); it is offered only on an idle,
// unarchived chat. With the model's real window known the line says how full the chat is against
// it, and large means near that window (which for a small local window comes well before the
// roll-over token setting). "Works well up to" sizes and suggested step-up models are the user's,
// set in Settings → Local models; nothing here guesses them.

import type { ChatRecord, MessageRow } from '../api/types.ts';
import { isLocalModelId } from '../state/localModel.ts';
import { Button } from '../ui/index.ts';
import { compactCount, CONTEXT_SHOWN_TOKENS, contextSize, contextVerdict, type ChatContextReading } from './rowMeta.ts';
import { useRolloverTokens } from './rolloverPref.ts';

export interface ContextBarProps {
  chat: ChatRecord;
  rows: MessageRow[];
  /** The host's reading: the last measured turn, the model's real window, and what the user said
   * this model works well up to and suggests in its place. */
  host: ChatContextReading | null;
  /** The newest rows are not loaded (the user scrolled back): the size would be old. */
  hasNewer: boolean;
  busy: boolean;
  onRollOver: () => void;
  /** Move this chat to another model, keeping the conversation. Offered first: it costs no
   * hand-off. Only passed when a switch makes sense (the host knows both models). */
  onSwitchModel?: (modelId: string) => void;
  /** The display names of the chat's own model and the suggested one, as the picker shows them. */
  modelLabel?: string | null;
  suggestLabel?: string | null;
}

/** With no window stated, a percent that still earns showing the line. */
const SHOWN_PCT = 60;

export function ContextBar({ chat, rows, host, hasNewer, busy, onRollOver, onSwitchModel, modelLabel, suggestLabel }: ContextBarProps) {
  const rollOverAt = useRolloverTokens();
  if (hasNewer || chat.archived || chat.sideSourceId) return null;
  const size = contextSize(rows, host, chat.id);
  if (!size || (size.tokens < CONTEXT_SHOWN_TOKENS && (size.pct ?? 0) < SHOWN_PCT)) return null;
  const { tokens, window, pct } = size;
  const verdict = contextVerdict(size, host, isLocalModelId(chat.model), rollOverAt);
  const where = pct !== null ? ` of ${compactCount(window!)} (${Math.round(pct)}%)` : '';
  const label = `${compactCount(tokens)} tokens of context${where}`;
  const title =
    `About ${tokens.toLocaleString()} tokens are in this chat's context` +
    (pct !== null
      ? ` — ${Math.round(pct)}% of the model's ${window!.toLocaleString()}-token window. Over 100% means it no longer fits.`
      : '') +
    `. Long chats get slower and costlier; "Roll over" in the chat menu continues the work in a fresh chat.`;
  if (verdict.kind === 'small') {
    return (
      <div className="context-size" data-testid="context-size" title={title}>
        {label}
      </div>
    );
  }
  if (verdict.kind === 'tooBig' && onSwitchModel) {
    const mine = modelLabel ?? chat.model;
    const stepUp = suggestLabel ?? verdict.suggest;
    return (
      <div className="context-bar" data-testid="context-too-big" role="status">
        <span title={title}>
          This chat is bigger than {mine} handles well ({compactCount(tokens)} of about{' '}
          {compactCount(verdict.comfort)} tokens). Consider {stepUp}.
        </span>
        <Button variant="primary" disabled={busy} onClick={() => onSwitchModel(verdict.suggest)} data-testid="switch-model-button">
          Switch to {stepUp}
        </Button>
        <Button variant="secondary" disabled={busy} onClick={onRollOver} data-testid="roll-over-button">
          Roll over
        </Button>
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
