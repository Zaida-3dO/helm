// The dock's subagent chip and its "Subagents · Across this chat" roster: "2 subagents running", opening a list of up to 30, running first.

import { useEffect, useRef, useState } from 'react';
import type { ThreadState } from '../../api/types.ts';
import { useLayer } from '../../ui/hooks.ts';
import { formatElapsed } from '../workBlocks.ts';
import { chipLabel, elapsedMs, previewLine, rosterOrder, titleOf } from './model.ts';
import { BranchGlyph, StatusMark } from './SubagentCard.tsx';
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

export function SubagentRosterChip({ thread }: { thread: ThreadState }) {
  const [open, setOpen] = useState(false);
  const ref = useRef<HTMLSpanElement>(null);
  const running = thread.subagentsRunning ?? 0;
  const now = useTicker(open && running > 0);
  // Esc goes through the app's one Escape stack: the first Esc closes the roster, and only a
  // second one reaches the chat (where it stops a running turn).
  useLayer('popover', open, () => setOpen(false));
  useEffect(() => {
    if (!open) return;
    const onDown = (e: MouseEvent) => {
      if (ref.current && !ref.current.contains(e.target as Node)) setOpen(false);
    };
    document.addEventListener('mousedown', onDown);
    return () => document.removeEventListener('mousedown', onDown);
  }, [open]);
  const label = chipLabel(thread);
  if (!label) return null;
  const list = rosterOrder(thread.subagents);

  return (
    <span className="subagent-chip-wrap" ref={ref}>
      <button
        type="button"
        className={`subagent-chip${running > 0 ? ' is-running' : ''}`}
        data-testid="subagent-chip"
        data-running={running}
        aria-expanded={open}
        onClick={() => setOpen((o) => !o)}
      >
        {running > 0 && <span className="subagent-chip-dot" aria-hidden="true" />}
        {label}
      </button>
      {open && (
        <span className="subagent-roster" role="dialog" aria-label="Subagents across this chat" data-testid="subagent-roster">
          <span className="subagent-roster-head">
            <span className="subagent-roster-title">Subagents</span>
            <span className="muted">Across this chat</span>
          </span>
          {list.map((s) => {
            const { name } = titleOf(s);
            return (
              <span key={s.toolCallId} className={`subagent-roster-row status-${s.status}`} data-testid="subagent-roster-row" data-status={s.status}>
                <span className="subagent-avatar small" aria-hidden="true">
                  <BranchGlyph size={12} />
                </span>
                <span className="subagent-roster-main">
                  <span className="subagent-roster-name">{name}</span>
                  <span className="subagent-roster-preview muted">{previewLine(s)}</span>
                </span>
                <span className="muted subagent-elapsed">{formatElapsed(elapsedMs(s, now))}</span>
                <StatusMark s={s} />
              </span>
            );
          })}
        </span>
      )}
    </span>
  );
}
