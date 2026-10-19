// The minimal activity dock: a phase label, "N live", elapsed time, the
// background-tasks chip with its hover list, and Stop beside the latest tool line when per-tool
// Stop is on and that call can be stopped.

import { useEffect, useState } from 'react';
import type { ChatRecord, ThreadState } from '../api/types.ts';
import { toolLabel } from './toolKinds.ts';
import { formatElapsed } from './workBlocks.ts';
import { SubagentRosterChip } from './subagents/SubagentRoster.tsx';

function useNow(live: boolean): number {
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    if (!live) return;
    const t = setInterval(() => setNow(Date.now()), 1000);
    return () => clearInterval(t);
  }, [live]);
  return now;
}

export interface DockProps {
  chat: ChatRecord;
  thread: ThreadState;
  turnStartedAt: number | null;
  onStopTool: (toolCallId: string) => void;
}

function phase(chat: ChatRecord, thread: ThreadState): string {
  if (chat.status === 'starting') return 'Starting the agent';
  if (chat.status === 'stopping') return 'Stopping safely';
  if (thread.compacting) return thread.compacting.retrying ? 'Compacting context (retrying)' : 'Compacting context';
  if (thread.interactions.length) return 'Waiting for you';
  if (thread.liveTools.length) return 'Following the request';
  return 'Waiting for a response';
}

export function ActivityDock({ chat, thread, turnStartedAt, onStopTool }: DockProps) {
  const live = chat.status === 'running' || chat.status === 'starting' || chat.status === 'stopping';
  const bg = thread.backgroundTasks;
  const now = useNow(live || bg.length > 0);
  const [open, setOpen] = useState(false);
  if (!live && bg.length === 0) return null;
  const latest = thread.liveTools.at(-1);
  const canStop = Boolean(latest && thread.sendBehavior.toolStop && thread.capabilities.stopTool && latest.stoppable);

  return (
    <div className="dock" data-testid="activity-dock">
      {live && <span className="dock-phase">{phase(chat, thread)}</span>}
      {latest && (
        <span className="dock-tool mono">
          {toolLabel(latest.name)} · {formatElapsed(now - latest.startedAt)}
          {canStop && (
            <button type="button" className="small" data-testid="dock-tool-stop" onClick={() => onStopTool(latest.toolCallId)}>
              Stop
            </button>
          )}
        </span>
      )}
      <span className="dock-spacer" />
      {thread.liveTools.length > 0 && <span className="muted">{thread.liveTools.length} live</span>}
      {bg.length > 0 && (
        <span
          className="bg-chip"
          tabIndex={0}
          data-testid="background-chip"
          data-count={bg.length}
          onMouseEnter={() => setOpen(true)}
          onMouseLeave={() => setOpen(false)}
          onFocus={() => setOpen(true)}
          onBlur={() => setOpen(false)}
        >
          {bg.length} background
          {open && (
            <span className="bg-card" role="tooltip">
              {bg.map((t) => (
                <span key={t.id} className="bg-item">
                  <span>{t.kind === 'shell' ? '$' : '◆'}</span>
                  <span>{t.title || 'Untitled'}</span>
                  {t.command && <span className="mono muted">{t.command}</span>}
                  <span className="muted">{formatElapsed(now - t.startedAt)}</span>
                </span>
              ))}
              <span className="muted">The agent continues when each one exits.</span>
            </span>
          )}
        </span>
      )}
      <SubagentRosterChip thread={thread} />
      {live && turnStartedAt !== null && <span className="muted">{formatElapsed(now - turnStartedAt)}</span>}
    </div>
  );
}
