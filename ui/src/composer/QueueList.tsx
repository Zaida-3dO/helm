// The queue below the transcript: Pending steers with Withdraw, queued
// follow-ups with Steer now, Send now and reorder, Recover to draft for unsent ones, and Resume for a
// held queue.

import type { ChatOps } from '../api/ops.ts';
import type { Submission, ThreadState } from '../api/types.ts';
import { HOME_SEND_BEHAVIOR } from '../api/types.ts';
import { movePosition, queueView } from './queue.ts';

export interface QueueListProps {
  pending: Submission[];
  thread: ThreadState | null;
  running: boolean;
  ops: ChatOps;
  chatId: string;
}

const DEFAULT_CAPS = { withdraw: false, deliverNow: false, stopTool: false, compact: false };

export function QueueList({ pending, thread, running, ops, chatId }: QueueListProps) {
  const items = queueView(pending, {
    running,
    capabilities: thread?.capabilities ?? DEFAULT_CAPS,
    showChoices: (thread?.sendBehavior ?? HOME_SEND_BEHAVIOR).showChoices,
  });
  const held = Boolean(thread?.queueHeld);
  if (!items.length && !held) return null;

  // Withdraw and Recover: the host moves the text into the stored draft and sends a `chat` event
  // with the newer draft revision, which the composer picks up.
  const act = (s: Submission, action: 'withdraw' | 'steerNow' | 'sendNow' | 'recover') =>
    ops.submissionAction(s.id, action).catch(() => undefined);

  const move = (s: Submission, delta: -1 | 1) => {
    const pos = movePosition(pending, s.id, delta);
    if (pos !== null) void ops.submissionAction(s.id, 'move', pos).catch(() => undefined);
  };

  return (
    <div className="queue" data-testid="queue">
      {held && (
        <div className="queue-held">
          <span className="muted">The queue is paused after the restart.</span>
          <button type="button" data-testid="queue-resume" onClick={() => void ops.resumeQueue(chatId)}>
            Resume
          </button>
        </div>
      )}
      {items.map(({ submission: s, state, label, actions }) => (
        <div key={s.id} className={`queue-item q-${state}`} data-testid="queue-item" data-state={state} data-submission={s.id}>
          <span className="queue-text">{s.text}</span>
          <span className={`queue-label q-label-${state}`} data-testid={state === 'pending' ? 'queue-pending' : undefined}>
            {label}
          </span>
          {actions.includes('withdraw') && (
            <button type="button" className="small" data-testid="queue-withdraw" onClick={() => void act(s, 'withdraw')}>
              Withdraw
            </button>
          )}
          {actions.includes('steerNow') && (
            <button type="button" className="small" data-testid="queue-steer-now" onClick={() => void act(s, 'steerNow')}>
              Steer now
            </button>
          )}
          {actions.includes('sendNow') && (
            <button type="button" className="small" data-testid="queue-send-now" onClick={() => void act(s, 'sendNow')}>
              Send now
            </button>
          )}
          {actions.includes('move_up') && (
            <button type="button" className="small" aria-label="Move up (Alt+Up)" onClick={() => move(s, -1)}>
              ↑
            </button>
          )}
          {actions.includes('move_down') && (
            <button type="button" className="small" aria-label="Move down (Alt+Down)" onClick={() => move(s, 1)}>
              ↓
            </button>
          )}
          {actions.includes('recover') && (
            <button type="button" className="small" data-testid="queue-recover" onClick={() => void act(s, 'recover')}>
              Recover to draft
            </button>
          )}
        </div>
      ))}
    </div>
  );
}
