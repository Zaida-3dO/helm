// The transcript: rows laid out into turns with folded work, rendered in the
// virtualised list.

import { useMemo, useState } from 'react';
import type { MessageRow } from '../api/types.ts';
import { FoldRow, Row, type RowContext } from './Rows.tsx';
import { VirtualList } from './VirtualList.tsx';
import { turnTimes } from './rowMeta.ts';
import { layout, type Item } from './workBlocks.ts';

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
  const byKey = useMemo(() => new Map(items.map((i) => [itemKey(i), i])), [items]);
  const keys = useMemo(() => items.map(itemKey), [items]);
  const times = useMemo(() => turnTimes(items), [items]);

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
      jumpTo={jumpToRowId != null ? `r:${jumpToRowId}` : null}
      render={(k) => {
        const item = byKey.get(k);
        if (!item) return null;
        if (item.kind === 'row') return (
            <Row row={item.row} ctx={ctx} final={item.final} afterTool={item.afterTool} indent={item.indent} afterFold={item.afterFold} turnMs={times.get(item.row.id)} />
          );
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
