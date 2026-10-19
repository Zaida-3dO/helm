// The hover action rows. A reply's row: Copy response, then the registered actions
// (Fork from here, Save, Remind, Explain plainly…) as 24 × 23 icon buttons, then the turn time and
// the token chip. A user message's row, at the bubble's bottom right: Copy message and the Edit
// pencil. Both stay invisible until the row is hovered or one of their buttons has focus.

import { useEffect, useRef, useState } from 'react';
import type { MessageRow } from '../api/types.ts';
import { IconCheck, IconCopy, IconEdit } from '../ui/icons.tsx';
import { formatTurnTime, tokenChip } from './rowMeta.ts';
import { messageActionsFor, type MessageAction, type SlotContext } from './slots.ts';

/** Copy to the clipboard; the icon turns into a check for 1.3 s. */
function CopyButton({ text, label }: { text: string; label: string }) {
  const [done, setDone] = useState(false);
  const timer = useRef<number | undefined>(undefined);
  useEffect(() => () => window.clearTimeout(timer.current), []);
  const copy = () => {
    void navigator.clipboard?.writeText(text).then(() => {
      setDone(true);
      window.clearTimeout(timer.current);
      timer.current = window.setTimeout(() => setDone(false), 1300);
    });
  };
  return (
    <button type="button" className={`row-action${done ? ' is-done' : ''}`} data-action="copy" title={done ? 'Copied' : label} aria-label={label} onClick={copy}>
      {done ? <IconCheck size={13} /> : <IconCopy size={13} />}
    </button>
  );
}

function ActionButton({ action, row, sctx }: { action: MessageAction; row: MessageRow; sctx: SlotContext }) {
  const Icon = action.icon;
  return (
    <button
      type="button"
      className={`row-action${Icon ? '' : ' row-action-text'}`}
      data-action={action.id}
      title={action.label}
      aria-label={Icon ? action.label : undefined}
      onClick={() => action.run(row, sctx)}
    >
      {Icon ? <Icon size={13} /> : action.label}
    </button>
  );
}

/** Registered actions in their declared order (Fork, Save, Remind first), then the rest as registered. */
function ordered(actions: MessageAction[]): MessageAction[] {
  return actions.map((a, i) => ({ a, i })).sort((x, y) => (x.a.order ?? 100) - (y.a.order ?? 100) || x.i - y.i).map((x) => x.a);
}

export interface ReplyActionsProps {
  row: MessageRow;
  sctx: SlotContext;
  /** The turn's time, on the reply that ends a turn with folded work. */
  turnMs?: number;
  showTurnTime: boolean;
  showTokens: boolean;
}

export function ReplyActions({ row, sctx, turnMs, showTurnTime, showTokens }: ReplyActionsProps) {
  if (row.status === 'streaming') return null;
  const actions = ordered(messageActionsFor(row));
  const usage = row.status === 'complete' && showTokens ? row.details?.usage : undefined;
  const chip = usage ? tokenChip(usage) : null;
  const time = showTurnTime && turnMs !== undefined ? formatTurnTime(turnMs) : null;
  const copyable = row.text.trim() !== '';
  if (!copyable && !actions.length && !chip && !time) return null;
  return (
    <div className="row-actions" data-testid="message-actions">
      {copyable && <CopyButton text={row.text} label="Copy response" />}
      {actions.map((a) => (
        <ActionButton key={a.id} action={a} row={row} sctx={sctx} />
      ))}
      {time && (
        <span className="row-turn-time" title="Time spent on this turn">
          {time}
        </span>
      )}
      {chip && (
        <span className="row-token-chip" title={chip.title} data-testid="token-chip">
          {chip.text}
        </span>
      )}
    </div>
  );
}

export interface UserActionsProps {
  row: MessageRow;
  sctx: SlotContext;
  /** Set when the message can be edited now (complete, no turn running, not already open). */
  onEdit?: () => void;
}

export function UserActions({ row, sctx, onEdit }: UserActionsProps) {
  const actions = ordered(messageActionsFor(row));
  const copyable = row.text.trim() !== '';
  if (!copyable && !onEdit && !actions.length) return null;
  return (
    <div className="row-user-actions">
      {copyable && <CopyButton text={row.text} label="Copy message" />}
      {actions.map((a) => (
        <ActionButton key={a.id} action={a} row={row} sctx={sctx} />
      ))}
      {onEdit && (
        <button type="button" className="row-action row-edit" data-testid="edit-message-button" title="Edit and resend from here" aria-label="Edit and resend from here" onClick={onEdit}>
          <IconEdit size={13} />
        </button>
      )}
    </div>
  );
}
