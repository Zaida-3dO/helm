// In-flight messages: messages on their way (Sending), messages Claude holds unread (Pending, with
// the reason it waits) with Withdraw, and messages known unsent (Not sent / Not delivered) with
// Recover to the composer. Nothing sits here waiting for a turn to end. Each bubble shows the
// message's pictures and file references, as a sent bubble does: a message's attachments are
// visible from the moment it is sent to the moment it settles. They are drawn as the last items of
// the transcript (QueueItem); QueueList stacks the same items under it for a transcript that is
// not at its newest page.

import type { ChatOps } from '../api/ops.ts';
import type { Submission, ThreadState } from '../api/types.ts';
import type { RemoteChatOps } from '../api/devices.ts';
import { Glyph } from '../transcript/icons.tsx';
import { MessageImages } from '../transcript/MessageImages.tsx';
import { MessageRefs } from '../transcript/MessageRefs.tsx';
import { inFlightView, type InFlightView } from './inFlight.ts';
import { composerApi } from './slots.ts';

export interface QueueListProps {
  pending: Submission[];
  thread: ThreadState | null;
  ops: ChatOps;
  chatId: string;
  /** A chat on another device whose host cannot take queue actions right now: the queue shows
   * unchanged. With a live host pass `devOps` instead and the actions cross the wire. */
  readOnly?: boolean;
  /** A chat on another device, and its live host answers: Withdraw and Recover travel there. */
  devOps?: RemoteChatOps;
  /** Why an unread message waits (`waitReason`). */
  reason?: string;
}

const DEFAULT_CAPS = { withdraw: false, deliverNow: false, stopTool: false, compact: false };

/** Withdraw and Recover: the host moves the text into the stored draft and sends a `chat` event
 * with the newer draft revision, which the composer picks up. The message's images and files
 * go back to the chat's unstaged set, and restore() stages them in the composer from there.
 * For a chat that runs on another PC the action travels there; Withdraw and Recover put the message's own
 * text into this PC's composer (the pictures already sit on the other PC, next to the chat). */
function queueAction(ops: ChatOps, chatId: string, devOps?: RemoteChatOps) {
  return (s: Submission, action: 'withdraw' | 'recover') =>
    (devOps ? devOps.submissionAction(chatId, s.id, action) : ops.submissionAction(s.id, action))
      .then(() => {
        if (devOps) {
          composerApi(chatId).insert(s.text);
        } else {
          composerApi(chatId).restore();
        }
      })
      .catch(() => undefined);
}

export function QueueItem({ item, ops, chatId, readOnly = false, devOps }: { item: InFlightView; ops: ChatOps; chatId: string; readOnly?: boolean; devOps?: RemoteChatOps }) {
  const act = queueAction(ops, chatId, devOps);
  const { submission: s, state, label, reason, tone, actions } = item;
  return (
    <div className={`queue-item q-${state} tone-${tone}`} data-testid="queue-item" data-state={state} data-tone={tone} data-submission={s.id}>
      <div className="queue-pill">
        <Glyph name={tone === 'error' ? 'alert' : 'clock'} size={12} />
        <span className="queue-label" data-testid={state === 'pending' ? 'queue-pending' : undefined}>
          {label}
        </span>
        {reason && (
          <span className="queue-reason" data-testid="queue-reason">
            {reason}
          </span>
        )}
        {!readOnly && actions.includes('withdraw') && (
          <button type="button" className="pill-btn" data-testid="queue-withdraw" onMouseDown={(e) => e.preventDefault()} onClick={() => void act(s, 'withdraw')}>
            Withdraw
          </button>
        )}
        {!readOnly && actions.includes('recover') && (
          <button type="button" className="pill-btn" data-testid="queue-recover" title="Move the text back to the composer" onMouseDown={(e) => e.preventDefault()} onClick={() => void act(s, 'recover')}>
            Recover
          </button>
        )}
      </div>
      <div className="queue-bubble">
        <span className="queue-text">{s.text}</span>
        <MessageImages chatId={chatId} images={s.images} />
        <MessageRefs refs={s.refs} />
      </div>
    </div>
  );
}

/** The in-flight messages of a chat, ready to show; empty when none is. */
export function inFlightItems(pending: Submission[], thread: ThreadState | null, reason?: string): InFlightView[] {
  return inFlightView(pending, { capabilities: thread?.capabilities ?? DEFAULT_CAPS, reason });
}

export function QueueList({ pending, thread, ops, chatId, readOnly = false, devOps, reason }: QueueListProps) {
  const items = inFlightItems(pending, thread, reason);
  if (!items.length) return null;
  return (
    <div className="queue" data-testid="queue">
      {items.map((item) => (
        <QueueItem key={item.submission.id} item={item} ops={ops} chatId={chatId} readOnly={readOnly} devOps={devOps} />
      ))}
    </div>
  );
}
