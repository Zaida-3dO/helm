// The queue below the transcript: Pending steers with Withdraw, queued
// follow-ups with Steer now, Send now and reorder, Recover to draft for unsent ones, and Resume for a
// held queue.

import { useState, type DragEvent, type KeyboardEvent } from 'react';
import type { ChatOps } from '../api/ops.ts';
import type { Submission, ThreadState } from '../api/types.ts';
import { HOME_SEND_BEHAVIOR } from '../api/types.ts';
import { Glyph } from '../transcript/icons.tsx';
import { dropPosition, movePosition, queueView } from './queue.ts';
import { takeSent } from './attachments.ts';
import { composerApi } from './slots.ts';

export interface QueueListProps {
  pending: Submission[];
  thread: ThreadState | null;
  running: boolean;
  ops: ChatOps;
  chatId: string;
}

const DEFAULT_CAPS = { withdraw: false, deliverNow: false, stopTool: false, compact: false };

export function QueueList({ pending, thread, running, ops, chatId }: QueueListProps) {
  const [drag, setDrag] = useState<{ id: string; over: string | null } | null>(null);
  const items = queueView(pending, {
    running,
    capabilities: thread?.capabilities ?? DEFAULT_CAPS,
    showChoices: (thread?.sendBehavior ?? HOME_SEND_BEHAVIOR).showChoices,
    sendMode: (thread?.sendBehavior ?? HOME_SEND_BEHAVIOR).mode,
  });
  const held = Boolean(thread?.queueHeld);
  if (!items.length && !held) return null;

  // Withdraw and Recover: the host moves the text into the stored draft and sends a `chat` event
  // with the newer draft revision, which the composer picks up.
  // A taken-back message also gives its images and files back to the composer.
  const act = (s: Submission, action: 'withdraw' | 'steerNow' | 'sendNow' | 'recover') =>
    ops
      .submissionAction(s.id, action)
      .then(() => {
        if (action !== 'withdraw' && action !== 'recover') return;
        const sent = takeSent(s.id);
        if (sent) composerApi(chatId).restore(sent);
      })
      .catch(() => undefined);

  const move = (s: Submission, delta: -1 | 1) => {
    const pos = movePosition(pending, s.id, delta);
    if (pos !== null) void ops.submissionAction(s.id, 'move', pos).catch(() => undefined);
  };

  const drop = (overId: string) => {
    if (!drag) return;
    const pos = dropPosition(pending, drag.id, overId);
    setDrag(null);
    if (pos !== null) void ops.submissionAction(drag.id, 'move', pos).catch(() => undefined);
  };

  const gripKey = (e: KeyboardEvent, s: Submission) => {
    if (!e.altKey || (e.key !== 'ArrowUp' && e.key !== 'ArrowDown')) return;
    e.preventDefault();
    move(s, e.key === 'ArrowUp' ? -1 : 1);
  };

  const gripDrag = (e: DragEvent<HTMLButtonElement>, s: Submission) => {
    e.dataTransfer.effectAllowed = 'move';
    e.dataTransfer.setData('text/plain', s.id);
    const item = e.currentTarget.closest('.queue-item');
    if (item) e.dataTransfer.setDragImage(item, 14, 10);
    setDrag({ id: s.id, over: null });
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
      {items.map(({ submission: s, state, label, tone, actions }) => {
        const movable = actions.includes('move_up') || actions.includes('move_down');
        const dropTarget = drag !== null && movable && drag.id !== s.id;
        return (
          <div
            key={s.id}
            className={`queue-item q-${state} tone-${tone}${drag?.id === s.id ? ' dragging' : ''}${dropTarget && drag.over === s.id ? ' drop-over' : ''}`}
            data-testid="queue-item"
            data-state={state}
            data-tone={tone}
            data-submission={s.id}
            onDragOver={
              dropTarget
                ? (e) => {
                    e.preventDefault();
                    if (drag.over !== s.id) setDrag({ ...drag, over: s.id });
                  }
                : undefined
            }
            onDrop={
              dropTarget
                ? (e) => {
                    e.preventDefault();
                    drop(s.id);
                  }
                : undefined
            }
          >
            <div className="queue-pill">
              <Glyph name={tone === 'error' ? 'alert' : 'clock'} size={12} />
              <span className="queue-label" data-testid={state === 'pending' ? 'queue-pending' : undefined}>
                {label}
              </span>
              {actions.includes('withdraw') && (
                <button type="button" className="pill-btn" data-testid="queue-withdraw" onMouseDown={(e) => e.preventDefault()} onClick={() => void act(s, 'withdraw')}>
                  Withdraw
                </button>
              )}
              {actions.includes('edit') && (
                <button type="button" className="pill-btn" data-testid="queue-edit" title="Move the text back to the composer to change it" onMouseDown={(e) => e.preventDefault()} onClick={() => void act(s, 'recover')}>
                  Edit
                </button>
              )}
              {actions.includes('steerNow') && (
                <button type="button" className="pill-btn" data-testid="queue-steer-now" title="Guide the current turn now" onMouseDown={(e) => e.preventDefault()} onClick={() => void act(s, 'steerNow')}>
                  Steer now
                </button>
              )}
              {actions.includes('sendNow') && (
                <button type="button" className="pill-btn" data-testid="queue-send-now" onMouseDown={(e) => e.preventDefault()} onClick={() => void act(s, 'sendNow')}>
                  Send now
                </button>
              )}
              {actions.includes('recover') && (
                <button type="button" className="pill-btn" data-testid="queue-recover" title="Move the text back to the composer" onMouseDown={(e) => e.preventDefault()} onClick={() => void act(s, 'recover')}>
                  Recover draft
                </button>
              )}
              {movable && (
                <button
                  type="button"
                  className="pill-grip"
                  data-testid="queue-grip"
                  draggable
                  aria-label="Reorder (Alt+Up or Alt+Down)"
                  title="Drag to reorder · Alt+Up / Alt+Down"
                  onDragStart={(e) => gripDrag(e, s)}
                  onDragEnd={() => setDrag(null)}
                  onKeyDown={(e) => gripKey(e, s)}
                >
                  <Glyph name="grip" size={14} />
                </button>
              )}
            </div>
            <div className="queue-bubble">
              <span className="queue-text">{s.text}</span>
            </div>
          </div>
        );
      })}
    </div>
  );
}
