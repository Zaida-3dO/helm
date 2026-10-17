// The transcript: rows laid out into turns with folded work, rendered in the
// virtualised list.

import { useMemo, useState } from 'react';
import type { MessageRow } from '../api/types.ts';
import { FoldRow, Row, type RowContext } from './Rows.tsx';
import { VirtualList } from './VirtualList.tsx';
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

  if (rows.length === 0) {
    return (
      <div className="transcript-empty muted" data-testid="transcript">
        No messages yet. Write below to start.
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
        if (item.kind === 'row') return <Row row={item.row} ctx={ctx} />;
        return (
          <FoldRow
            count={item.rows.length}
            elapsedMs={item.elapsedMs}
            onOpen={() => setExpanded((s) => new Set([...s, item.turnKey]))}
          />
        );
      }}
    />
  );
}
