// The transcript: rows laid out into turns with folded work, rendered in the
// virtualised list.

import { useEffect, useMemo, useRef, useState } from 'react';
import type { ChatOps } from '../api/ops.ts';
import type { MessageRow, Submission, ThreadState } from '../api/types.ts';
import { waitReason } from '../composer/inFlight.ts';
import { QueueItem, inFlightItems } from '../composer/QueueList.tsx';
import { FoldRow, Row, type RowContext } from './Rows.tsx';
import { VirtualList } from './VirtualList.tsx';
import { skillsByReply, turnTimes } from './rowMeta.ts';
import { estimateItem, layout, type Item } from './workBlocks.ts';
import { AgentGroup } from './subagents/AgentGroup.tsx';

export interface TranscriptProps {
  rows: MessageRow[];
  live: boolean;
  hasOlder: boolean;
  ctx: RowContext;
  onLoadOlder: () => Promise<void>;
  onJumpLatest: () => Promise<void>;
  onFollowChange: (following: boolean) => void;
  jumpToRowId?: number | null;
  /** Changes when the transcript should jump to the newest message and follow it (the reader sent one). */
  scrollSignal?: number;
  /**
   * Messages in flight, shown as the last items of the flow (their keys start `q:`). Leave out for
   * a transcript that is not at its newest page: they could not sit at the end of it.
   */
  queue?: { pending: Submission[]; thread: ThreadState | null; ops: ChatOps; readOnly: boolean };
}

/** A queued message's height before it is measured: a pill and a short bubble. */
const QUEUED_ESTIMATE = 96;

function itemKey(i: Item): string {
  return i.kind === 'row' ? `r:${i.row.id}` : i.key;
}

export function Transcript({ rows, live, hasOlder, ctx, onLoadOlder, onJumpLatest, onFollowChange, jumpToRowId, scrollSignal, queue }: TranscriptProps) {
  const [expanded, setExpanded] = useState<ReadonlySet<string>>(new Set());
  const items = useMemo(() => layout(rows, live, expanded), [rows, live, expanded]);
  // A row inside a subagent group is found under the group's key.
  const byKey = useMemo(() => {
    const m = new Map<string, Item>();
    for (const i of items) {
      m.set(itemKey(i), i);
      if (i.kind === 'agents') i.rows.forEach((r) => m.set(`r:${r.id}`, i));
    }
    return m;
  }, [items]);
  const queued = useMemo(
    () => (queue ? inFlightItems(queue.pending, queue.thread, waitReason(rows)) : []),
    [queue, rows],
  );
  const keys = useMemo(() => [...items.map(itemKey), ...queued.map((q) => `q:${q.submission.id}`)], [items, queued]);
  const times = useMemo(() => turnTimes(items), [items]);
  const skills = useMemo(() => skillsByReply(rows), [rows]);

  // A jump into folded work (find in chat) opens that turn first, once per target, so the reader can
  // fold it again; the list scrolls once the row exists.
  const revealed = useRef<number | null>(null);
  useEffect(() => {
    if (jumpToRowId == null) {
      revealed.current = null;
      return;
    }
    if (revealed.current === jumpToRowId) return;
    revealed.current = jumpToRowId;
    if (byKey.has(`r:${jumpToRowId}`)) return;
    const at = rows.findIndex((r) => r.id === jumpToRowId);
    for (let j = at; j >= 0; j--) {
      const r = rows[j]!;
      if (r.role === 'user' && r.details?.mode !== 'steer') {
        const turn = r.sourceKey;
        setExpanded((s) => (s.has(turn) ? s : new Set(s).add(turn)));
        return;
      }
    }
  }, [jumpToRowId, rows, byKey]);
  const jumpItem = jumpToRowId != null ? byKey.get(`r:${jumpToRowId}`) : undefined;
  const jumpKey = jumpItem ? itemKey(jumpItem) : null;

  if (rows.length === 0 && queued.length === 0) {
    return (
      <div className="transcript-empty" data-testid="transcript">
        {/* An empty chat: the same heading as the new-chat page, vertically centred. */}
        <h1 className="transcript-empty-title">What are we building?</h1>
      </div>
    );
  }

  return (
    <VirtualList
      keys={keys}
      hasOlder={hasOlder}
      onLoadOlder={onLoadOlder}
      onJumpLatest={onJumpLatest}
      onFollowChange={onFollowChange}
      jumpTo={jumpKey}
      scrollSignal={scrollSignal}
      estimate={(k) => (k.startsWith('q:') ? QUEUED_ESTIMATE : estimateItem(byKey.get(k)))} // height guesses for unmeasured rows
      render={(k) => {
        if (k.startsWith('q:')) {
          const q = queued.find((i) => `q:${i.submission.id}` === k);
          return q && queue ? (
            <div className="row row-user row-queued">
              <QueueItem item={q} ops={queue.ops} chatId={ctx.chatId} readOnly={queue.readOnly} />
            </div>
          ) : null;
        }
        const item = byKey.get(k);
        if (!item) return null;
        if (item.kind === 'row') return (
            <Row row={item.row} ctx={ctx} final={item.final} afterTool={item.afterTool} indent={item.indent} afterFold={item.afterFold} hideThinking={item.hideThinking} beforeTool={item.beforeTool} noticeLive={item.noticeLive} recap={item.recap} turnMs={times.get(item.row.id)} skills={skills.get(item.row.id)} />
          );
        if (item.kind === 'agents') return <AgentGroup rows={item.rows} flags={item.flags} ctx={ctx} />;
        return (
          <FoldRow
            count={item.steps}
            elapsedMs={item.elapsedMs}
            failed={item.failed}
            open={item.open}
            onToggle={() =>
              setExpanded((s) => {
                const next = new Set(s);
                if (!next.delete(item.turnKey)) next.add(item.turnKey);
                return next;
              })
            }
          />
        );
      }}
    />
  );
}
