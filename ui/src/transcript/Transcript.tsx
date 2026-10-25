// The transcript: rows laid out into turns with folded work, rendered in the
// virtualised list.

import { useEffect, useMemo, useRef, useState } from 'react';
import type { MessageRow } from '../api/types.ts';
import { FoldRow, Row, type RowContext } from './Rows.tsx';
import { VirtualList } from './VirtualList.tsx';
import { turnTimes } from './rowMeta.ts';
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
}

function itemKey(i: Item): string {
  return i.kind === 'row' ? `r:${i.row.id}` : i.key;
}

export function Transcript({ rows, live, hasOlder, ctx, onLoadOlder, onJumpLatest, onFollowChange, jumpToRowId }: TranscriptProps) {
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
  const keys = useMemo(() => items.map(itemKey), [items]);
  const times = useMemo(() => turnTimes(items), [items]);

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

  if (rows.length === 0) {
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
      estimate={(k) => estimateItem(byKey.get(k))} // height guesses for unmeasured rows
      render={(k) => {
        const item = byKey.get(k);
        if (!item) return null;
        if (item.kind === 'row') return (
            <Row row={item.row} ctx={ctx} final={item.final} afterTool={item.afterTool} indent={item.indent} afterFold={item.afterFold} hideThinking={item.hideThinking} turnMs={times.get(item.row.id)} />
          );
        if (item.kind === 'agents') return <AgentGroup rows={item.rows} ctx={ctx} />;
        return (
          <FoldRow
            count={item.rows.length}
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
