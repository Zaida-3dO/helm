// One transcript row per kind: user, assistant (with thinking preview), tool result,
// context rows, and error/retry rows. Every row carries the stable `transcript-row` hook with its
// status and role.

import { memo, useState } from 'react';
import type { MessageRow } from '../api/types.ts';
import { EditMessage, type EditState } from './EditMessage.tsx';
import { Markdown, type LinkTarget } from './Markdown.tsx';
import { messageActionsFor, rowFootersFor, type SlotContext } from './slots.ts';
import { ToolCard } from './ToolCard.tsx';
import { usePacedText, useThinkingPreview } from './usePaced.ts';
import { formatElapsed } from './workBlocks.ts';

export interface RowContext {
  canStopTool: (row: MessageRow) => boolean;
  onStopTool: (toolCallId: string) => void;
  onLink: (target: LinkTarget) => void;
  /** For the transcript slots (slots.ts). */
  chatId: string;
  latestAssistantId: number | null;
  running: boolean;
  interactionOpen: boolean;
  /** Transcript element toggles; a missing key is shown. */
  elements?: { tokens?: 'shown' | 'hidden'; turnTime?: 'shown' | 'hidden'; fileChanges?: 'shown' | 'hidden'; timeline?: 'shown' | 'hidden' };
  /** Edit and resend: the message being edited, if any. */
  editing?: EditState | null;
  onEdit?: (row: MessageRow) => void;
  onResend?: (text: string) => Promise<void>;
  onCancelEdit?: () => void;
}

function slotContext(row: MessageRow, ctx: RowContext): SlotContext {
  return { chatId: ctx.chatId, isLatestAssistant: row.id === ctx.latestAssistantId, running: ctx.running, interactionOpen: ctx.interactionOpen };
}

/** Registered message actions (hover buttons) and footers for one row. */
function Slots({ row, ctx }: { row: MessageRow; ctx: RowContext }) {
  const sctx = slotContext(row, ctx);
  const actions = messageActionsFor(row);
  const footers = rowFootersFor(row, sctx);
  if (!actions.length && !footers.length) return null;
  return (
    <>
      {actions.length > 0 && (
        <div className="row-actions" data-testid="message-actions">
          {actions.map((a) => (
            <button key={a.id} type="button" className="small link" data-action={a.id} onClick={() => a.run(row, sctx)}>
              {a.label}
            </button>
          ))}
        </div>
      )}
      {footers.map((f) => (
        <div key={f.id} className="row-footer" data-footer={f.id}>
          {f.render(row, sctx)}
        </div>
      ))}
    </>
  );
}

const RETRY_TEXT: Record<string, string> = {
  retrying: 'Request failed, retrying…',
  waiting: 'Waiting for the connection to retry…',
  recovered: 'Recovered after a retry.',
  retried: 'Retried.',
  cancelled: 'Retry cancelled.',
  failed: 'Request failed.',
};

const CONTEXT_LABEL: Record<string, string> = {
  compaction: 'Context compacted',
  modelChange: 'Model changed',
  effortChange: 'Effort changed',
  notice: 'Notice',
  restart: 'Helm restarted',
  fork: 'Fork',
  edit: 'Edited',
};

function Thinking({ text, streaming }: { text: string; streaming: boolean }) {
  const preview = useThinkingPreview(text);
  const [open, setOpen] = useState(false);
  if (!text) return null;
  return (
    <div className="thinking">
      <button type="button" className="link" onClick={() => setOpen((o) => !o)} aria-expanded={open}>
        {streaming ? 'Thinking' : 'Thought'}
      </button>
      {open ? <pre className="thinking-full">{text}</pre> : <span className="thinking-preview muted"> {preview}</span>}
    </div>
  );
}

function AssistantBody({ row, ctx }: { row: MessageRow; ctx: RowContext }) {
  const streaming = row.status === 'streaming';
  const text = usePacedText(row.text, streaming);
  return (
    <>
      <Thinking text={row.thinking} streaming={streaming} />
      {text && <Markdown text={text} streaming={streaming} interactive onLink={ctx.onLink} />}
    </>
  );
}

export const Row = memo(function Row({ row, ctx }: { row: MessageRow; ctx: RowContext }) {
  const retry = row.details?.retry;
  const common = { 'data-testid': 'transcript-row', 'data-status': row.status, 'data-role': row.role, 'data-row-id': row.id };
  if (row.role === 'user') {
    return (
      <div className={`row row-user status-${row.status}`} {...common} title={new Date(row.timestamp).toLocaleString()}>
        {ctx.editing?.rowId === row.id && ctx.onResend && ctx.onCancelEdit ? (
          <EditMessage edit={ctx.editing} onResend={ctx.onResend} onCancel={ctx.onCancelEdit} />
        ) : (
          <div className="bubble">
            <Markdown text={row.text} onLink={ctx.onLink} />
          </div>
        )}
        {row.status !== 'complete' && <div className="row-meta muted">{row.status}</div>}
        {row.status === 'complete' && !ctx.running && ctx.onEdit && ctx.editing?.rowId !== row.id && (
          <button type="button" className="small link row-edit" data-testid="edit-message-button" title="Edit and resend" onClick={() => ctx.onEdit?.(row)}>
            Edit
          </button>
        )}
        <Slots row={row} ctx={ctx} />
      </div>
    );
  }
  if (row.role === 'toolResult') {
    return (
      <div className="row row-tool" {...common}>
        <ToolCard row={row} canStop={ctx.canStopTool(row)} onStop={ctx.onStopTool} />
      </div>
    );
  }
  if (row.role === 'context') {
    const label = CONTEXT_LABEL[row.details?.contextKind ?? 'notice'] ?? 'Notice';
    return (
      <div className="row row-context" {...common}>
        <span className="context-label">{label}</span>
        {row.text && <span className="muted"> · {row.text}</span>}
      </div>
    );
  }
  const usage = row.details?.usage;
  return (
    <div className={`row row-assistant status-${row.status}`} {...common}>
      <AssistantBody row={row} ctx={ctx} />
      {row.status === 'error' && (
        <div className="error-row" data-testid="error-row">
          {row.details?.errorMessage ?? 'The request failed.'}
        </div>
      )}
      {row.status === 'interrupted' && <div className="muted">Interrupted.</div>}
      {retry && (
        <div className={`retry-row retry-${retry.state}`} data-testid="retry-row" data-state={retry.state}>
          {RETRY_TEXT[retry.state]} {retry.attempt > 1 ? `(attempt ${retry.attempt})` : ''}
        </div>
      )}
      {usage && row.status === 'complete' && ctx.elements?.tokens !== 'hidden' && (
        <div className="row-meta muted">
          {usage.inputTokens + usage.cacheReadTokens} in · {usage.outputTokens} out
        </div>
      )}
      <Slots row={row} ctx={ctx} />
    </div>
  );
});

export function FoldRow({ count, elapsedMs, onOpen }: { count: number; elapsedMs: number; onOpen: () => void }) {
  return (
    <div className="row row-fold">
      <button type="button" className="link" onClick={onOpen} data-testid="work-fold">
        Worked for {formatElapsed(elapsedMs)} · {count} {count === 1 ? 'step' : 'steps'}
      </button>
    </div>
  );
}
