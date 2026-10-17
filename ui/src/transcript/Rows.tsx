// One transcript row per kind: user, assistant (with thinking preview), tool result,
// context rows, and error/retry rows. Every row carries the stable `transcript-row` hook with its
// status and role.

import { memo, useState } from 'react';
import type { MessageRow } from '../api/types.ts';
import { Markdown, type LinkTarget } from './Markdown.tsx';
import { ToolCard } from './ToolCard.tsx';
import { usePacedText, useThinkingPreview } from './usePaced.ts';
import { formatElapsed } from './workBlocks.ts';

export interface RowContext {
  canStopTool: (row: MessageRow) => boolean;
  onStopTool: (toolCallId: string) => void;
  onLink: (target: LinkTarget) => void;
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
        <div className="bubble">
          <Markdown text={row.text} onLink={ctx.onLink} />
        </div>
        {row.status !== 'complete' && <div className="row-meta muted">{row.status}</div>}
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
      {usage && row.status === 'complete' && (
        <div className="row-meta muted">
          {usage.inputTokens + usage.cacheReadTokens} in · {usage.outputTokens} out
        </div>
      )}
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
