// Tool cards.

import { useEffect, useState } from 'react';
import type { MessageRow } from '../api/types.ts';
import { changeLines, toolKind, toolLabel, toolStatus, toolTarget } from './toolKinds.ts';
import { formatElapsed } from './workBlocks.ts';

export interface ToolCardProps {
  row: MessageRow;
  /** The Stop button shows only when the setting is on, the worker can stop one call, and this call is stoppable. */
  canStop: boolean;
  onStop: (toolCallId: string) => void;
}

function useElapsed(since: number, live: boolean): number {
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    if (!live) return;
    const t = setInterval(() => setNow(Date.now()), 1000);
    return () => clearInterval(t);
  }, [live]);
  return Math.max(0, now - since);
}

export function ToolCard({ row, canStop, onStop }: ToolCardProps) {
  const kind = toolKind(row.toolName);
  const status = toolStatus(row);
  const target = toolTarget(row);
  const elapsed = useElapsed(row.timestamp, status === 'running');
  const [wrap, setWrap] = useState(true);
  const output = row.text;

  const copy = () => void navigator.clipboard?.writeText(output);

  return (
    <div className={`tool-card tool-${kind} status-${status}`} data-testid="tool-card" data-tool={row.toolName ?? ''} data-status={status}>
      <div className="tool-head">
        <span className="tool-label">{toolLabel(row.toolName)}</span>
        {target && <span className="tool-target mono">{target}</span>}
        <span className={`tool-status ${status}`}>{status}</span>
        {status === 'running' && <span className="muted">{formatElapsed(elapsed)}</span>}
        {status === 'running' && canStop && row.toolCallId && (
          <button type="button" className="small" data-testid="tool-stop" onClick={() => onStop(row.toolCallId!)}>
            Stop
          </button>
        )}
      </div>
      {kind === 'edit' ? <ChangeView row={row} /> : null}
      {kind !== 'edit' && output && (
        <div className="tool-output-wrap">
          <pre className={`tool-output mono ${wrap ? 'wrap' : ''}`}>{output}</pre>
          <div className="tool-actions">
            <button type="button" className="small" onClick={copy}>
              Copy
            </button>
            <button type="button" className="small" onClick={() => setWrap((w) => !w)}>
              {wrap ? 'No wrap' : 'Wrap'}
            </button>
            {row.artifact && <span className="muted">Full output saved</span>}
          </div>
        </div>
      )}
    </div>
  );
}

function ChangeView({ row }: { row: MessageRow }) {
  const { lines, cut } = changeLines(row);
  if (!lines.length) return null;
  return (
    <div className="change-view mono">
      {lines.map((l, i) => (
        <div key={i} className={`change-line sign-${l.sign === '+' ? 'add' : l.sign === '-' ? 'del' : 'ctx'}`}>
          <span className="sign">{l.sign}</span>
          {l.text}
        </div>
      ))}
      {cut && <div className="muted">Excerpt: the change is longer than shown.</div>}
    </div>
  );
}
