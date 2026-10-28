// Commits (grouped by day) and Checks (grouped by outcome, polled while something is pending).

import { useEffect } from 'react';
import { IconExternalLink } from '../ui/icons.tsx';
import { formatDuration, groupChecks } from './parse.ts';
import { POLL_MS } from './store.ts';
import type { PrEntry } from './store.ts';
import { handleLinkOpen } from './open.ts';
import type { CheckGroup, Commit } from './types.ts';
import { Avatar, ErrorBlock, relTime, useEntry } from './util.tsx';

function dayKey(iso: string): string {
  const t = Date.parse(iso);
  return Number.isFinite(t) ? new Date(t).toDateString() : 'Date unknown';
}

export function CommitsView({ entry }: { entry: PrEntry }) {
  useEntry(entry);
  useEffect(() => {
    if (!entry.commits && !entry.commitsError) void entry.loadCommits();
  }, [entry, entry.commits, entry.commitsError]);

  if (!entry.commits) {
    if (entry.commitsError) return <ErrorBlock message={entry.commitsError} onRetry={() => void entry.loadCommits(true)} />;
    return <div className="pr-loading">Loading the commits…</div>;
  }
  const days = new Map<string, Commit[]>();
  for (const c of entry.commits.data) days.set(dayKey(c.date), [...(days.get(dayKey(c.date)) ?? []), c]);
  return (
    <div className="pr-commits" data-testid="pr-commits">
      {[...days].map(([day, list]) => (
        <section key={day}>
          <h4 className="pr-day">{day === 'Date unknown' ? day : `Commits on ${day}`}</h4>
          {list.map((c) => (
            <div key={c.sha} className="pr-commit-row" data-testid="pr-commit">
              <Avatar login={c.author} size={20} />
              <span className="pr-commit-msg">{c.headline || 'No commit message'}</span>
              <span className="pr-muted">{c.date ? relTime(c.date) : ''}</span>
              <code className="pr-sha">{c.sha.slice(0, 7)}</code>
              <span className={`pr-verify pr-verify-${c.verification.toLowerCase()}`} title={c.verificationReason} tabIndex={0}>
                {c.verification}
              </span>
            </div>
          ))}
        </section>
      ))}
      {entry.commits.truncated && <div className="pr-fine">Only the first {entry.commits.data.length} commits are shown. See all of them on GitHub.</div>}
    </div>
  );
}

const GROUP_TITLE: Record<CheckGroup, string> = { failing: 'Failing', pending: 'In progress', passed: 'Successful', skipped: 'Skipped' };

export function ChecksView({ entry, focused }: { entry: PrEntry; focused: boolean }) {
  useEntry(entry);
  // First load, then every interval while something is pending and the tab is shown and focused.
  useEffect(() => {
    if (!focused) return;
    if (!entry.checks && !entry.checksError && Date.now() - entry.checksAttempt >= POLL_MS) void entry.loadChecks();
    const t = setInterval(() => {
      if (entry.checksPending() && !entry.checksError) void entry.loadChecks();
    }, POLL_MS);
    return () => clearInterval(t);
  }, [entry, focused, entry.checks, entry.checksError]);

  if (!entry.checks) {
    if (entry.checksError) return <ErrorBlock message={entry.checksError} onRetry={() => void entry.loadChecks()} />;
    return <div className="pr-loading">Loading the checks…</div>;
  }
  const groups = groupChecks(entry.checks.data);
  if (groups.length === 0) return <div className="pr-empty" data-testid="pr-checks">No checks have run on this pull request.</div>;
  return (
    <div className="pr-checks" data-testid="pr-checks">
      {entry.checksError && <div className="pr-fine pr-bad">{entry.checksError}</div>}
      {groups.map((g) => (
        <section key={g.group}>
          <h4 className="pr-day">
            {GROUP_TITLE[g.group]} ({g.rows.length})
          </h4>
          {g.rows.map((r, i) => (
            <div key={`${r.name}-${i}`} className={`pr-check-row pr-check-${r.group}`} data-testid="pr-check">
              <span className={`pr-dot pr-dot-${r.group === 'passed' ? 'ok' : r.group === 'failing' ? 'bad' : r.group === 'pending' ? 'wait' : 'none'}`} aria-hidden="true" />
              <span className="pr-check-name">{r.name}</span>
              {r.app && <span className="pr-muted">{r.app}</span>}
              {r.description && <span className="pr-muted">{r.description}</span>}
              <span className="pr-check-state">{r.state}</span>
              {r.durationSec !== null && <span className="pr-muted">{formatDuration(r.durationSec)}</span>}
              {r.url && (
                <button type="button" className="pr-link-btn" title="Open the details on GitHub" aria-label={`Details of ${r.name}`} onClick={() => handleLinkOpen(r.url!)}>
                  <IconExternalLink size={12} />
                </button>
              )}
            </div>
          ))}
        </section>
      ))}
      {entry.checks.truncated && <div className="pr-fine">Only the first {entry.checks.data.length} checks are shown.</div>}
    </div>
  );
}
