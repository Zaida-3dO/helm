// The live activity dock: a rounded card in the column
// above the composer. Left, a 64 x 48 glyph cell (the still ship's wheel, 40 px, while a turn runs; a check
// once it settles); then two lines (the state, then the latest tool with its elapsed time and, when
// per-tool Stop is on and the call can be stopped, Stop); right, Details › and the counts ("N live",
// the background-shells chip with its hover list, the subagent roster, the turn's clock). Details
// opens a list of what is running now.

import { useEffect, useRef, useState } from 'react';
import type { ChatRecord, MessageRow, ThreadState } from '../api/types.ts';
import { IconBrowser, IconCheck, IconChevronRight, IconFileText, IconSearch, IconStop, IconTerminal, IconWand } from '../ui/icons.tsx';
import { WheelMark } from '../ui/status/index.ts';
import { useLayer } from '../ui/hooks.ts';
import { toolKind, toolLabel, toolStatus, toolTarget, toolVerb } from './toolKinds.ts';
import { formatElapsed } from './workBlocks.ts';
import { SubagentRosterChip } from './subagents/SubagentRoster.tsx';
import { backgroundShells, dockActivity, dockShown, dockState, finishedCount, finishedTurn, type FinishedTurn } from './dockModel.ts';
import '../composer/dock.css';
import './activityDock.css';

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
  /** The chat's loaded rows, for the settled line after a turn ends. */
  rows?: readonly MessageRow[];
}

function ToolIcon({ name }: { name: string }) {
  const kind = toolKind(name);
  if (kind === 'shell') return <IconTerminal size={13} />;
  if (kind === 'read') return name === 'Grep' || name === 'Glob' ? <IconSearch size={13} /> : <IconFileText size={13} />;
  if (kind === 'edit') return <IconFileText size={13} />;
  if (kind === 'web') return <IconBrowser size={13} />;
  return <IconWand size={13} />;
}

/**
 * When the dock's current phase began. A phase that is already under way when the dock appears
 * counts from its own start; once the phase changes (thinking, then writing), the time starts over.
 */
function usePhaseSince(label: string | null, since: number | null): number | null {
  const seen = useRef<{ label: string | null; at: number | null }>({ label, at: since });
  if (seen.current.label !== label) seen.current = { label, at: label === null ? null : Date.now() };
  return label === null ? null : (seen.current.at ?? since);
}

export function ActivityDock({ chat, thread, turnStartedAt, onStopTool, rows }: DockProps) {
  const live = chat.status === 'running' || chat.status === 'starting' || chat.status === 'stopping';
  const bg = backgroundShells(thread);
  const now = useNow(live || bg.length > 0);
  const [open, setOpen] = useState(false);
  const [details, setDetails] = useState(false);
  const shown = dockShown(live, thread);
  const root = useRef<HTMLDivElement>(null);
  const toggle = useRef<HTMLButtonElement>(null);
  // Escape closes the Details list through the layer stack and gives focus back to its button;
  // so does a click outside the dock. A dock that goes away takes its open list with it.
  useLayer('popover', details, () => {
    setDetails(false);
    toggle.current?.focus();
  });
  useEffect(() => {
    if (!shown) setDetails(false);
  }, [shown]);
  useEffect(() => {
    if (!details) return;
    const away = (e: MouseEvent) => {
      if (!root.current?.contains(e.target as Node)) setDetails(false);
    };
    document.addEventListener('mousedown', away);
    return () => document.removeEventListener('mousedown', away);
  }, [details]);
  const latest = thread.liveTools.at(-1);
  // With no tool running, say what the turn is doing instead: thinking, writing, waiting.
  const activity = shown && !latest && live && chat.status === 'running' && rows ? dockActivity(rows, thread, turnStartedAt) : null;
  const phaseSince = usePhaseSince(activity?.label ?? null, activity?.since ?? null);
  if (!shown) {
    const done = chat.status === 'idle' && rows ? finishedTurn(rows) : null;
    return done ? <FinishedLine turn={done} /> : null;
  }
  const canStop = Boolean(latest && thread.sendBehavior.toolStop && thread.capabilities.stopTool && latest.stoppable);
  const state = dockState(chat.status, thread);
  const working = live || (thread.subagentsRunning ?? 0) > 0;
  const narration = state.key === 'working' ? (activity?.narration ?? state.detail) : state.detail;

  return (
    <div className="activity-dock" aria-live="off" data-testid="activity-dock" data-state={state.key} data-phase={activity?.phase} ref={root}>
      <div className="ad-surface">
        <span className="ad-glyph" aria-hidden="true">
          {working ? (
            <WheelMark size={40} state={chat.status === 'stopping' ? 'stopping' : live ? 'working' : 'calm'} />
          ) : (
            <IconCheck size={20} />
          )}
        </span>
        <div className="ad-lines">
          <div className="ad-line1">
            <span className="ad-state">{state.label}</span>
            {narration && <span className="ad-preview">{narration}</span>}
          </div>
          <div className="ad-line2">
            {latest ? (
              <span className="ad-tool">
                <ToolIcon name={latest.name} />
                <span className="ad-tool-name">{toolLabel(latest.name)}</span>
                <span className="ad-tool-time">{formatElapsed(now - latest.startedAt)}</span>
                {canStop && (
                  <button
                    type="button"
                    className="ad-tool-stop"
                    data-testid="dock-tool-stop"
                    aria-label={`Stop ${toolLabel(latest.name)}`}
                    title="Stop this command; the turn carries on"
                    onClick={() => onStopTool(latest.toolCallId)}
                  >
                    <IconStop size={9} filled />
                  </button>
                )}
              </span>
            ) : activity ? (
              <span className="ad-phase" data-phase={activity.phase} data-testid="dock-phase">
                <span>{activity.label}</span>
                <span className="ad-tool-time">{formatElapsed(Math.max(0, now - (phaseSince ?? activity.since)))}</span>
              </span>
            ) : (
              <span className="ad-idle">{live ? 'Working' : 'Nothing running in this turn'}</span>
            )}
          </div>
        </div>
        <div className="ad-side">
          <button
            ref={toggle}
            type="button"
            className={`ad-details${details ? ' open' : ''}`}
            aria-expanded={details}
            data-testid="dock-details"
            onClick={() => setDetails((d) => !d)}
          >
            Details <IconChevronRight size={12} />
          </button>
          <div className="ad-counts">
            {thread.liveTools.length > 0 && <span>{thread.liveTools.length} live</span>}
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
                <span className="bg-dot" aria-hidden="true" />
                {bg.length} background
                {open && (
                  <span className="bg-card" role="tooltip">
                    {bg.map((t) => (
                      <span key={t.id} className="bg-item">
                        <IconTerminal size={12} />
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
            {live && turnStartedAt !== null && <span className="ad-clock">{formatElapsed(now - turnStartedAt)}</span>}
          </div>
        </div>
      </div>
      {details && (
        <div className="ad-details-pop" role="region" aria-label="Activity" data-testid="dock-details-pop">
          <div className="ad-details-head">
            <strong>Activity</strong>
            <span className="muted">Latest turn{live && turnStartedAt !== null ? ` · ${formatElapsed(now - turnStartedAt)}` : ''}</span>
          </div>
          {thread.liveTools.length === 0 && bg.length === 0 ? (
            <div className="ad-details-empty muted">Nothing is running right now.</div>
          ) : (
            <ul className="ad-details-list">
              {thread.liveTools.map((t) => (
                <li key={t.toolCallId}>
                  <ToolIcon name={t.name} />
                  <span>{toolLabel(t.name)}</span>
                  <span className="ad-details-status">Live</span>
                  <span className="muted mono">{formatElapsed(now - t.startedAt)}</span>
                </li>
              ))}
              {bg.map((t) => (
                <li key={t.id}>
                  <IconTerminal size={13} />
                  <span>{t.title || t.command || 'Background shell'}</span>
                  <span className="ad-details-status">Background</span>
                  <span className="muted mono">{formatElapsed(now - t.startedAt)}</span>
                </li>
              ))}
            </ul>
          )}
        </div>
      )}
    </div>
  );
}

function firstLine(s: string | null): string {
  return (s ?? '').split(String.fromCharCode(10))[0] ?? '';
}

/**
 * What stays after a turn ends: a check, "Finished", how long it worked, a count, and Details,
 * which lists the tool calls the turn made. It stays for the chat, so a reopened chat shows it too.
 */
function FinishedLine({ turn }: { turn: FinishedTurn }) {
  const [details, setDetails] = useState(false);
  const root = useRef<HTMLDivElement>(null);
  const toggle = useRef<HTMLButtonElement>(null);
  useLayer('popover', details, () => {
    setDetails(false);
    toggle.current?.focus();
  });
  useEffect(() => {
    if (!details) return;
    const away = (e: MouseEvent) => {
      if (!root.current?.contains(e.target as Node)) setDetails(false);
    };
    document.addEventListener('mousedown', away);
    return () => document.removeEventListener('mousedown', away);
  }, [details]);
  const count = finishedCount(turn);
  return (
    <div className="activity-dock settled" data-testid="activity-dock" data-state="finished" ref={root}>
      <div className="ad-surface">
        <span className="ad-glyph" aria-hidden="true">
          <IconCheck size={20} />
        </span>
        <div className="ad-lines">
          <div className="ad-line1">
            <span className="ad-state">Finished</span>
            {turn.elapsedMs >= 1000 && <span className="ad-preview">Worked for {formatElapsed(turn.elapsedMs)}</span>}
          </div>
        </div>
        <div className="ad-side">
          <button
            ref={toggle}
            type="button"
            className={`ad-details${details ? ' open' : ''}`}
            aria-expanded={details}
            data-testid="dock-details"
            onClick={() => setDetails((d) => !d)}
          >
            Details <IconChevronRight size={12} />
          </button>
          {count && (
            <div className="ad-counts">
              <span data-testid="finished-count">{count}</span>
            </div>
          )}
        </div>
      </div>
      {details && (
        <div className="ad-details-pop" role="region" aria-label="Activity" data-testid="dock-details-pop">
          <div className="ad-details-head">
            <strong>Activity</strong>
            <span className="muted">Latest turn · {formatElapsed(turn.elapsedMs)}</span>
          </div>
          {turn.tools.length === 0 ? (
            <div className="ad-details-empty muted">This turn made no tool calls.</div>
          ) : (
            <ul className="ad-details-list">
              {turn.tools.map((t) => (
                <li key={t.id}>
                  <ToolIcon name={t.toolName ?? ''} />
                  <span>
                    {toolVerb(t)} <span className="muted">{firstLine(toolTarget(t))}</span>
                  </span>
                  {toolStatus(t) === 'complete' && <IconCheck size={12} />}
                  <span className="muted mono">{new Date(t.timestamp).toLocaleTimeString([], { hour12: false })}</span>
                </li>
              ))}
            </ul>
          )}
        </div>
      )}
    </div>
  );
}
