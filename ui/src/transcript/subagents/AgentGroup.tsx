// Consecutive Agent calls in a transcript: one "N subagents" header over a grid of side-by-side
// cards. An Agent call with no subagent state to show (an old row) falls back to its tool row.

import type { RowContext } from '../Rows.tsx';
import { Row } from '../Rows.tsx';
import type { MessageRow } from '../../api/types.ts';
import { BranchGlyph, SubagentCard, useThread } from './SubagentCard.tsx';
import { subagentFor } from './model.ts';
import './subagents.css';

export function AgentGroup({ rows, ctx }: { rows: MessageRow[]; ctx: RowContext }) {
  const thread = useThread(ctx.chatId);
  const cards = rows.filter((r) => subagentFor(r, thread));
  const plain = rows.filter((r) => !subagentFor(r, thread));
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
      {plain.map((r) => (
        <Row key={r.id} row={r} ctx={ctx} />
      ))}
    </div>
  );
}
