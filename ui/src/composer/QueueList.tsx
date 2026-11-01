// The list below the transcript: messages on their way (Sending), messages Claude holds unread
// (Pending) with Withdraw, and messages known unsent (Not sent / Not delivered) with Recover to
// the composer. Nothing sits here waiting for a turn to end.

import type { ChatOps } from '../api/ops.ts';
import type { Submission, ThreadState } from '../api/types.ts';
import { Glyph } from '../transcript/icons.tsx';
import { inFlightView } from './inFlight.ts';
import { takeSent } from './attachments.ts';
import { composerApi } from './slots.ts';

export interface QueueListProps {
  pending: Submission[];
  thread: ThreadState | null;
  ops: ChatOps;
  chatId: string;
}

const DEFAULT_CAPS = { withdraw: false, deliverNow: false, stopTool: false, compact: false };

export function QueueList({ pending, thread, ops, chatId }: QueueListProps) {
  const items = inFlightView(pending, {
    capabilities: thread?.capabilities ?? DEFAULT_CAPS,
  });
  if (!items.length) return null;

  // Withdraw and Recover: the host moves the text into the stored draft and sends a `chat` event
  // with the newer draft revision, which the composer picks up.
  // A taken-back message also gives its images and files back to the composer.
  const act = (s: Submission, action: 'withdraw' | 'recover') =>
    ops
      .submissionAction(s.id, action)
      .then(() => {
        const sent = takeSent(s.id);
        if (sent) composerApi(chatId).restore(sent);
      })
      .catch(() => undefined);

  return (
    <div className="queue" data-testid="queue">
      {items.map(({ submission: s, state, label, tone, actions }) => (
        <div
          key={s.id}
          className={`queue-item q-${state} tone-${tone}`}
          data-testid="queue-item"
          data-state={state}
          data-tone={tone}
          data-submission={s.id}
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
            {actions.includes('recover') && (
              <button type="button" className="pill-btn" data-testid="queue-recover" title="Move the text back to the composer" onMouseDown={(e) => e.preventDefault()} onClick={() => void act(s, 'recover')}>
                Recover
              </button>
            )}
          </div>
          <div className="queue-bubble">
            <span className="queue-text">{s.text}</span>
          </div>
        </div>
      ))}
    </div>
  );
}
