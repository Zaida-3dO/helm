// The Cost dialog: what a chat, or every chat in a folder or project, has used. Tokens and the
// cost come from Helm's own records of each turn (subagents included); the weekly share is an estimate the host works
// out from the meter's weekly reading (`usage_cost`).

import { useEffect, useMemo, useState } from 'react';
import type { Api } from '../api/client.ts';
import { usageOps, type CostReport } from '../api/usage.ts';
import { Button, Dialog } from '../ui/index.ts';
import { percentText, shareText, tokensText, usdText } from './model.ts';
import './usage.css';

export type CostTarget = { chatId: string } | { projectId: string };

function day(ms: number): string {
  return new Date(ms).toLocaleString(undefined, { weekday: 'short', day: 'numeric', month: 'short', hour: '2-digit', minute: '2-digit' });
}

export function CostDialog({ api, target, onClose }: { api: Api; target: CostTarget; onClose: () => void }) {
  const ops = useMemo(() => usageOps(api), [api]);
  const [report, setReport] = useState<CostReport | null>(null);
  const [error, setError] = useState<string | null>(null);
  const key = 'chatId' in target ? `c:${target.chatId}` : `p:${target.projectId}`;
  useEffect(() => {
    let live = true;
    ops
      .cost(target)
      .then((r) => live && setReport(r))
      .catch((e) => live && setError(String((e as Error)?.message ?? e)));
    return () => {
      live = false;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [ops, key]);

  const chat = 'chatId' in target;
  const title = report ? `Cost · ${report.name || (chat ? 'Untitled chat' : 'Untitled')}` : 'Cost';
  const t = report?.totals;
  const w = report?.weekly;
  return (
    <Dialog title={title} onClose={onClose} wide={!chat} padded testId="cost-dialog" footer={<Button onClick={onClose}>Close</Button>}>
      {error && <p className="cost-error">{error}</p>}
      {!report && !error && <p className="cost-note">Adding it up…</p>}
      {report && t && (
        <div className="cost-body">
          <div className="cost-headline">
            <div className="cost-figure" data-testid="cost-usd">
              <span className="cost-big">{usdText(t.costUsd)}</span>
              <span className="cost-caption">estimated cost</span>
            </div>
            <div className="cost-figure" data-testid="cost-weekly">
              <span className="cost-big">{w ? shareText(w.sharePct) : '—'}</span>
              <span className="cost-caption">of the weekly limit</span>
            </div>
          </div>
          <table className="cost-tokens">
            <tbody>
              <tr><th>Input</th><td>{tokensText(t.inputTokens)}</td></tr>
              <tr><th>Output</th><td>{tokensText(t.outputTokens)}</td></tr>
              <tr><th>Cache read</th><td>{tokensText(t.cacheReadTokens)}</td></tr>
              <tr><th>Cache write</th><td>{tokensText(t.cacheWriteTokens)}</td></tr>
              <tr className="cost-total"><th>Total tokens</th><td>{tokensText(t.totalTokens)}</td></tr>
            </tbody>
          </table>
          {!chat && report.chats.length > 0 && (
            <table className="cost-chats" data-testid="cost-chats">
              <thead>
                <tr><th>Chat</th><th>Cost</th><th>Share</th><th>Weekly</th></tr>
              </thead>
              <tbody>
                {report.chats.map((c) => (
                  <tr key={c.chatId}>
                    <td className="cost-chat-title" title={c.title}>
                      {c.title || 'Untitled chat'}
                      {c.archived && <span className="cost-tag">archived</span>}
                    </td>
                    <td>{usdText(c.totals.costUsd)}</td>
                    <td>{shareText(t.costUsd > 0 ? (c.totals.costUsd / t.costUsd) * 100 : 0)}</td>
                    <td>{c.weeklyPct === undefined ? '—' : shareText(c.weeklyPct)}</td>
                  </tr>
                ))}
              </tbody>
            </table>
          )}
          {!chat && report.chats.length === 0 && <p className="cost-note">No chat in here has used anything yet.</p>}
          <div className="cost-notes">
            <p>The dollar figure is Claude Code’s estimate of what this would cost at API prices. It is not what your plan bills you.</p>
            {w ? (
              <p>
                The weekly share is an estimate. The plan reports one figure for everything ({percentText(w.weekPct)} of this week used so far), so Helm shares it out by
                cost since the window began on {day(w.windowStart)}: {chat ? 'this chat' : 'these chats'} cost {usdText(w.costUsd)} of the {usdText(w.helmCostUsd)} all
                Helm chats cost since then. It assumes all of this week’s usage came through Helm.
              </p>
            ) : (
              <p>There is no current weekly reading from the plan meter, so there is no weekly share. Refresh the meter in the top bar and open this again.</p>
            )}
            {report.uncountedReplies && (
              <p data-testid="cost-uncounted">
                Helm began recording what each turn costs {report.countingSince === undefined ? 'with this version' : `on ${day(report.countingSince)}`}, so replies {chat ? 'in this chat' : 'in these chats'} from
                before then are not included.
              </p>
            )}
            <p>
              A forked chat counts only what it used after the fork, not the history it copied.
              {!chat && ` Archived chats are included${report.archivedChats ? ` (${report.archivedChats} here)` : ''}, and so are chats in folders inside this one.`}
              {!chat && ' Share is each chat’s part of the total here; Weekly is its part of the weekly limit.'}
            </p>
          </div>
        </div>
      )}
    </Dialog>
  );
}
