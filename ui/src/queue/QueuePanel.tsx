// The queue panel: a modal over the host's queue summary, opened from the sidebar's Queue
// tile. One row per chat whose turn start waits: open it, move it (to the top, up, down),
// park it by hand, release a parked one, or send it back (typed text returns to the draft).
// The rows come straight from the host; every action answers with the fresh queue.

import type { QueueEntry, QueueSummary } from '../api/generated/index.ts';
import type { QueueStore } from './store.ts';
import { aheadOf, CLASS_LABEL, claudeLine, queueCount, serverLine, untilText, waitLine } from './model.ts';
import { Button, Dialog, toast } from '../ui/index.ts';
import { IconChevronDown, IconChevronUp, IconExternalLink, IconPause, IconPlay, IconX } from '../ui/icons.tsx';
import './queue.css';

type MoveAction = 'top' | 'up' | 'down' | 'hold' | 'release' | 'cancel';

/** How many rows stand on this entry's line, so Down can stop at the end. */
function lineSize(entries: readonly QueueEntry[], mine: QueueEntry): number {
  if (mine.reason === 'manual') return 1;
  return entries.filter((e) => e.reason !== 'manual' && e.serverId === mine.serverId).length;
}

/** A short readable name for a server id; unknown ids show as they are. */
function serverName(id: string): string {
  return id === 'strata' ? 'Strata' : id === 'ollama' ? 'Ollama' : id;
}

export function QueuePanel({ store, onOpen, onClose }: { store: QueueStore; onOpen: (chatId: string) => void; onClose: () => void }) {
  const summary = store.getSnapshot();
  const now = Date.now();
  const claudeUntil = summary != null && summary.claudeFull && summary.claudeUntil != null ? untilText(summary.claudeUntil, now) : null;
  const act = (chatId: string, action: MoveAction) => {
    store.act(chatId, action).catch((e: unknown) => {
      toast.show({ kind: 'error', text: String((e as Error)?.message ?? e) });
    });
  };
  return (
    <Dialog
      title="Queue"
      onClose={onClose}
      wide
      testId="queue-panel"
      intro="Chats whose turn is waiting to open. A waiting chat keeps its message saved and opens on its own when its place frees."
    >
      {summary === null ? (
        <p className="queue-empty">Checking the queue…</p>
      ) : summary.entries.length === 0 ? (
        <p className="queue-empty" data-testid="queue-empty">Nothing is waiting.</p>
      ) : (
        summary.entries.map((e) => <QueueRow key={e.chatId} entry={e} summary={summary} claudeUntil={claudeUntil} act={act} onOpen={onOpen} onClose={onClose} />)
      )}
      {summary != null && (summary.servers.length > 0 || summary.claudeQueued > 0 || summary.claudeFull) && (
        <div className="queue-servers" data-testid="queue-servers">
          {summary.servers.map((s) => (
            <span key={s.serverId} className="queue-server" data-server-id={s.serverId}>
              {serverName(s.serverId)}: {serverLine(s.working, s.queued)}
            </span>
          ))}
          <span className="queue-claude">{claudeLine(summary, now)}</span>
        </div>
      )}
      <span className="queue-foot" data-testid="queue-foot">
        {queueCount(summary)} waiting
      </span>
    </Dialog>
  );
}

function QueueRow({
  entry: e,
  summary,
  claudeUntil,
  act,
  onOpen,
  onClose,
}: {
  entry: QueueEntry;
  summary: QueueSummary;
  claudeUntil: string | null;
  act: (chatId: string, action: MoveAction) => void;
  onOpen: (chatId: string) => void;
  onClose: () => void;
}) {
  const ahead = aheadOf(summary.entries, e.chatId);
  const last = ahead >= lineSize(summary.entries, e) - 1;
  return (
    <div className="queue-row" data-testid="queue-row" data-chat-id={e.chatId} data-reason={e.reason}>
      <button type="button" className="queue-open" title="Open this chat" onClick={() => { onClose(); onOpen(e.chatId); }}>
        <span className="queue-title">{e.title}</span>
        <IconExternalLink size={13} />
      </button>
      <span className="queue-line">{waitLine(e, ahead, e.reason === 'claude_capacity' ? claudeUntil : null)}</span>
      <span className="queue-class" title={`This start is from ${CLASS_LABEL[e.class] ?? e.class}`}>{e.class}</span>
      <div className="queue-actions">
        <Button variant="ghost" className="queue-btn" title="Push to the front of its line" onClick={() => act(e.chatId, 'top')} data-testid="queue-top">
          To top
        </Button>
        <Button
          variant="ghost"
          className="queue-btn"
          title="Move one place up"
          onClick={() => act(e.chatId, 'up')}
          disabled={e.reason === 'manual' || ahead === 0}
          data-testid="queue-up"
        >
          <IconChevronUp size={13} />
        </Button>
        <Button
          variant="ghost"
          className="queue-btn"
          title="Move one place down"
          onClick={() => act(e.chatId, 'down')}
          disabled={e.reason === 'manual' || last}
          data-testid="queue-down"
        >
          <IconChevronDown size={13} />
        </Button>
        {e.reason === 'manual' ? (
          <Button variant="ghost" className="queue-btn" title="End the hand-parked wait" onClick={() => act(e.chatId, 'release')} data-testid="queue-release">
            <IconPlay size={13} />
          </Button>
        ) : (
          <Button variant="ghost" className="queue-btn" title="Park this chat until you release it" onClick={() => act(e.chatId, 'hold')} data-testid="queue-hold">
            <IconPause size={13} />
          </Button>
        )}
        <Button variant="ghost" className="queue-btn" title="Send it back: typed text returns to the draft" onClick={() => act(e.chatId, 'cancel')} data-testid="queue-cancel">
          <IconX size={13} />
        </Button>
      </div>
    </div>
  );
}
