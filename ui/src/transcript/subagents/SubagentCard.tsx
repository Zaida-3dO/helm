// The subagent card under an Agent/Task tool row: name,
// assignment, live latest step, elapsed time and status, and the result once it settles. Live
// state comes from the chat's thread; after a restart, from the tool row's saved details.

import { useEffect, useState, useSyncExternalStore } from 'react';
import type { Subagent } from '../../api/generated/index.ts';
import type { MessageRow, ThreadState } from '../../api/types.ts';
import { useShell } from '../../shell/context.ts';
import { formatElapsed } from '../workBlocks.ts';
import { elapsedMs, formatTokens, isAgentRow, previewLine, STATUS_LABEL, subagentFor, titleOf } from './model.ts';
import './subagents.css';

function useTicker(live: boolean): number {
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    if (!live) return;
    setNow(Date.now()); // the clock may have stood still since mount
    const t = setInterval(() => setNow(Date.now()), 1000);
    return () => clearInterval(t);
  }, [live]);
  return now;
}

function useThread(chatId: string): ThreadState | null {
  const { store } = useShell();
  return useSyncExternalStore(store.subscribe, () => store.view(chatId).thread);
}

/** A small branch glyph (⑂) for the agent avatar. */
export function BranchGlyph({ size = 14 }: { size?: number }) {
  return (
    <svg className="glyph" width={size} height={size} viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
      <circle cx="6" cy="5" r="2" />
      <circle cx="6" cy="19" r="2" />
      <circle cx="18" cy="7" r="2" />
      <path d="M6 7v10M18 9c0 4-4 5-8 6l-4 1" />
    </svg>
  );
}

export function StatusMark({ s }: { s: Subagent }) {
  return (
    <span className={`subagent-status status-${s.status}`} data-testid="subagent-status" data-status={s.status}>
      {s.status === 'running' && <span className="subagent-spinner" aria-hidden="true" />}
      {s.status === 'completed' && (
        <svg className="glyph" width={11} height={11} viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.4" aria-hidden="true">
          <path d="M20 6 9 17l-5-5" />
        </svg>
      )}
      {s.status === 'failed' && (
        <svg className="glyph" width={11} height={11} viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.4" aria-hidden="true">
          <path d="M12 8v5M12 16.5v.5M10.3 3.9 1.8 18a2 2 0 0 0 1.7 3h17a2 2 0 0 0 1.7-3L13.7 3.9a2 2 0 0 0-3.4 0" />
        </svg>
      )}
      {s.status === 'stopped' && (
        <svg className="glyph" width={11} height={11} viewBox="0 0 24 24" fill="currentColor" aria-hidden="true">
          <rect x="6" y="5" width="4" height="14" rx="1" />
          <rect x="14" y="5" width="4" height="14" rx="1" />
        </svg>
      )}
      {STATUS_LABEL[s.status]}
    </span>
  );
}

export function SubagentCard({ row }: { row: MessageRow }) {
  if (!isAgentRow(row)) return null;
  return <AgentCard row={row} />;
}

function AgentCard({ row }: { row: MessageRow }) {
  const shell = useShell();
  const thread = useThread(row.chatId);
  const s = subagentFor(row, thread);
  const [open, setOpen] = useState(false);
  const [stopping, setStopping] = useState(false);
  const now = useTicker(s?.status === 'running');
  if (!s) return null;
  const { name, type } = titleOf(s);
  const running = s.status === 'running';
  const canStop = running && Boolean(thread?.capabilities.stopSubagent) && Boolean(row.toolCallId);
  const tokens = formatTokens(s.tokens);
  const stop = () => {
    if (!row.toolCallId) return;
    setStopping(true);
    void shell.ops
      .stopTool(row.chatId, row.toolCallId)
      .catch(() => undefined)
      .finally(() => setStopping(false));
  };

  return (
    <div className={`subagent-card status-${s.status}${open ? ' open' : ''}`} data-testid="subagent-card" data-status={s.status} data-tool-call={s.toolCallId}>
      <button type="button" className="subagent-head" aria-expanded={open} onClick={() => setOpen((o) => !o)}>
        <span className="subagent-avatar" aria-hidden="true">
          <BranchGlyph size={14} />
        </span>
        <span className="subagent-main">
          <span className="subagent-title">
            <span className="subagent-name">{name}</span>
            {type && <span className="subagent-type">{type}</span>}
            <span className="subagent-spacer" />
            <span className="subagent-elapsed muted">{formatElapsed(elapsedMs(s, now))}</span>
            <StatusMark s={s} />
          </span>
          {s.assignment && <span className="subagent-assignment">{s.assignment.split('\n')[0]}</span>}
          <span className={`subagent-preview${s.status === 'failed' ? ' is-error' : ''}`} data-testid="subagent-preview">
            {previewLine(s)}
          </span>
        </span>
        <svg className="glyph subagent-chevron" width={13} height={13} viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" aria-hidden="true">
          <path d="m9 18 6-6-6-6" />
        </svg>
      </button>
      {canStop && (
        <button type="button" className="subagent-stop" data-testid="subagent-stop" title="Stop this subagent; the chat carries on" disabled={stopping} onClick={stop}>
          <span className="subagent-stop-square" />
        </button>
      )}
      {open && (
        <div className="subagent-detail" data-testid="subagent-detail">
          <div className="subagent-meta muted">
            <span>{STATUS_LABEL[s.status]}</span>
            <span>{formatElapsed(elapsedMs(s, now))}</span>
            {s.agentType && <span>{s.agentType}</span>}
            {tokens && <span>{tokens}</span>}
            {s.toolUses != null && <span>{s.toolUses === 1 ? '1 tool call' : `${s.toolUses} tool calls`}</span>}
          </div>
          {s.assignment && (
            <>
              <div className="subagent-label">Assignment</div>
              <div className="subagent-assignment-full">{s.assignment}</div>
            </>
          )}
          {running && (
            <>
              <div className="subagent-label">Now</div>
              <div className="subagent-now">{previewLine(s)}</div>
            </>
          )}
          {s.error && <div className="subagent-error">{s.error}</div>}
          {s.report && (
            <>
              <div className="subagent-label">Result</div>
              <pre className="subagent-report">{s.report}</pre>
              {s.reportTruncated && <div className="muted subagent-note">Excerpt: the full report is longer.</div>}
            </>
          )}
        </div>
      )}
    </div>
  );
}
