// One transcript row per kind: user, assistant (with thinking preview), tool result,
// context rows, and error/retry rows. Every row carries the stable `transcript-row` hook with its
// status and role.

import { memo, useState } from 'react';
import type { MessageRow } from '../api/types.ts';
import { EditMessage, type EditState } from './EditMessage.tsx';
import { Markdown, type LinkTarget } from './Markdown.tsx';
import { MessageImages } from './MessageImages.tsx';
import { MessageRefs } from './MessageRefs.tsx';
import { bubbleText, fromTelegram } from './origin.ts';
import { IconPaperPlane } from '../ui/icons.tsx';
import { ReplyActions, UserActions } from './MessageActions.tsx';
import { rowFootersFor, type SlotContext } from './slots.ts';
import { Glyph } from './icons.tsx';
import { ToolCard } from './ToolCard.tsx';
import { usePacedText, useThinkingPreview } from './usePaced.ts';
import { formatElapsed, type RowFlags } from './workBlocks.ts';

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

/** Registered footers for one row (the reply's change card, option buttons). */
function Footers({ row, ctx }: { row: MessageRow; ctx: RowContext }) {
  const sctx = slotContext(row, ctx);
  const footers = rowFootersFor(row, sctx);
  if (!footers.length) return null;
  return (
    <>
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
  orchestrator: 'Orchestrator',
};

function Thinking({ text, streaming, onLink }: { text: string; streaming: boolean; onLink: RowContext['onLink'] }) {
  const preview = useThinkingPreview(text);
  const [open, setOpen] = useState(false);
  if (!text) return null;
  return (
    <div className={`thinking${open ? ' open' : ''}`} data-testid="thinking">
      <button type="button" className="thinking-toggle" onClick={() => setOpen((o) => !o)} aria-expanded={open}>
        <Glyph name="chevron" size={12} className="thinking-chevron" />
        <span className={`thinking-label${streaming ? ' live' : ''}`}>{streaming ? 'Thinking' : 'Thought process'}</span>
        {!open && preview && <span className="thinking-preview">· {preview}</span>}
      </button>
      {open && (
        <div className="thinking-body">
          <Markdown text={text} onLink={onLink} />
        </div>
      )}
    </div>
  );
}

function AssistantBody({ row, ctx, hideThinking }: { row: MessageRow; ctx: RowContext; hideThinking?: boolean }) {
  const streaming = row.status === 'streaming';
  const text = usePacedText(row.text, streaming);
  return (
    <>
      <Thinking text={hideThinking ? '' : row.thinking} streaming={streaming} onLink={ctx.onLink} />
      {text && <Markdown text={text} streaming={streaming} interactive onLink={ctx.onLink} />}
    </>
  );
}

export const Row = memo(function Row({ row, ctx, final, afterTool, indent, afterFold, hideThinking, beforeTool, turnMs }: { row: MessageRow; ctx: RowContext; turnMs?: number } & RowFlags) {
  const retry = row.details?.retry;
  const common = { 'data-testid': 'transcript-row', 'data-status': row.status, 'data-role': row.role, 'data-row-id': row.id };
  const place = `${afterTool ? ' after-tool' : ''}${final ? ' final-answer' : ''}${indent ? ' indented' : ''}${afterFold ? ' after-fold' : ''}${beforeTool ? ' before-tool' : ''}`;
  if (row.role === 'user') {
    return (
      <div className={`row row-user status-${row.status}`} {...common} title={new Date(row.timestamp).toLocaleString()}>
        {fromTelegram(row) && (
          <div className="row-origin muted" data-testid="row-origin" data-origin="telegram" title="Sent from Telegram">
            <IconPaperPlane size={12} /> Telegram
          </div>
        )}
        {ctx.editing?.rowId === row.id && ctx.onResend && ctx.onCancelEdit ? (
          <EditMessage edit={ctx.editing} onResend={ctx.onResend} onCancel={ctx.onCancelEdit} />
        ) : (
          <div className="bubble">
            <MessageImages chatId={ctx.chatId} images={row.details?.images} />
            <MessageRefs refs={row.details?.refs} />
            <Markdown text={bubbleText(row)} onLink={ctx.onLink} />
          </div>
        )}
        {row.status !== 'complete' && <div className="row-meta muted">{row.status}</div>}
        {ctx.editing?.rowId !== row.id && (
          <UserActions
            row={row}
            sctx={slotContext(row, ctx)}
            onEdit={row.status === 'complete' && !ctx.running && ctx.onEdit ? () => ctx.onEdit?.(row) : undefined}
          />
        )}
        <Footers row={row} ctx={ctx} />
      </div>
    );
  }
  if (row.role === 'toolResult') {
    return (
      <div className={`row row-tool${place}`} {...common}>
        <ToolCard row={row} canStop={ctx.canStopTool(row)} onStop={ctx.onStopTool} />
      </div>
    );
  }
  if (row.role === 'context') {
    const label = CONTEXT_LABEL[row.details?.contextKind ?? 'notice'] ?? 'Notice';
    return (
      <div className={`row row-context${place}`} {...common}>
        <span className="context-label">{label}</span>
        {row.text && <span className="muted"> · {row.text}</span>}
      </div>
    );
  }
  return (
    <div className={`row row-assistant status-${row.status}${place}`} {...common} data-final={final ? 'true' : undefined}>
      <AssistantBody row={row} ctx={ctx} hideThinking={hideThinking} />
      {row.status === 'error' && (
        <div className="error-row" data-testid="error-row">
          <Glyph name="alert" size={14} />
          <span>{row.details?.errorMessage ?? 'The request failed.'}</span>
        </div>
      )}
      {row.status === 'interrupted' && (
        <div className="interrupted-row muted" data-testid="interrupted-row">
          <Glyph name="pause" size={14} />
          <span>Interrupted</span>
        </div>
      )}
      {retry && (
        <div className={`retry-row retry-${retry.state}`} data-testid="retry-row" data-state={retry.state}>
          {RETRY_TEXT[retry.state]} {retry.attempt > 1 ? `(attempt ${retry.attempt})` : ''}
        </div>
      )}
      <ReplyActions
        row={row}
        sctx={slotContext(row, ctx)}
        turnMs={turnMs}
        showTurnTime={ctx.elements?.turnTime !== 'hidden'}
        showTokens={ctx.elements?.tokens !== 'hidden'}
      />
      <Footers row={row} ctx={ctx} />
    </div>
  );
});

export function FoldRow({ count, elapsedMs, failed, open, onToggle }: { count: number; elapsedMs: number; failed: number; open: boolean; onToggle: () => void }) {
  return (
    <div className={`row row-fold${open ? ' open' : ''}`}>
      <button
        type="button"
        className="fold-toggle"
        // The closed variant keeps the original hook the e2e harness clicks to unfold; the open one is separate so that a loop clicking "the first fold" cannot close it again.
        data-testid={open ? 'work-fold-open' : 'work-fold'}
        aria-expanded={open}
        title={open ? 'Hide thinking and tool activity' : 'Show thinking and tool activity'}
        onClick={onToggle}
      >
        <Glyph name="chevron" size={14} className="fold-chevron" />
        <span>Worked for {formatElapsed(elapsedMs)}</span>
        <span className="visually-hidden">
          {' '}
          · {count} {count === 1 ? 'step' : 'steps'}
        </span>
      </button>
      {failed > 0 && (
        <span className="fold-failed" data-testid="work-failed">
          {failed} failed
        </span>
      )}
    </div>
  );
}
