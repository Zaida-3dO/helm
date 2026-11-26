// Consecutive Agent calls in a transcript: one "N subagents" header over a grid of side-by-side
// cards. Only an Agent call that cannot be read as a card yet (still streaming with no live state)
// falls back to its tool row; a settled row always shows a card, from saved state or from itself.

import type { RowContext } from '../Rows.tsx';
import { Row } from '../Rows.tsx';
import type { RowFlags } from '../workBlocks.ts';
import type { MessageRow } from '../../api/types.ts';
import { BranchGlyph, SubagentCard, useThread } from './SubagentCard.tsx';
import { subagentFor } from './model.ts';
import './subagents.css';

export function AgentGroup({ rows, flags, ctx }: { rows: MessageRow[]; flags: RowFlags[]; ctx: RowContext }) {
  // The subagents' state is kept by the session that ran them: an older session's group reads its own.
  const thread = useThread(rows[0]?.chatId ?? ctx.chatId);
  const shown = rows.map((r, i) => ({ r, flags: flags[i], card: subagentFor(r, thread) !== null }));
  const cards = shown.filter((x) => x.card).map((x) => x.r);
  const plain = shown.filter((x) => !x.card);
  return (
    <div className="row agent-group" data-testid="agent-group" data-count={cards.length}>
      {cards.length > 0 && (
        <>
          <div className="agent-group-head">
            <BranchGlyph size={13} />
            <span className="agent-group-count">
              {cards.length} {cards.length === 1 ? 'subagent' : 'subagents'}
            </span>
            <span className="agent-group-spacer" />
            <span className="agent-group-note">Delegated work</span>
          </div>
          <div className="agent-grid">
            {cards.map((r) => (
              <SubagentCard key={r.id} row={r} />
            ))}
          </div>
        </>
      )}
      {plain.map((x) => (
        <Row key={x.r.id} row={x.r} ctx={ctx} {...x.flags} />
      ))}
    </div>
  );
}
