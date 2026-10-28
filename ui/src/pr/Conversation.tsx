// Conversation: the description, the read-only sidebar, the status card with the merge box, and the
// timeline (comments, reviews with their threads, commits and one-line events).

import { useEffect, useState } from 'react';
import { Button } from '../ui/index.ts';
import { attempt, clickFrom } from './client.ts';
import { METHOD_LABEL } from './client.ts';
import type { PrWriteResult } from './types.ts';
import type { PrEntry } from './store.ts';
import { CommentBody, ThreadCard } from './Threads.tsx';
import type { PrAnchor, PrSummary, TimelineItem } from './types.ts';
import { AssociationBadge, Avatar, ErrorBlock, fullDate, relTime, useEntry } from './util.tsx';

export function ConversationView({ entry, anchor, clearAnchor, focused }: { entry: PrEntry; anchor?: PrAnchor; clearAnchor: () => void; focused: boolean }) {
  useEntry(entry);
  const pr = entry.summary;

  // A round starts when the view is shown and focused: the first time, or when the data went stale
  // while it was hidden. A round that was cut short or failed is never started again from here (the
  // effect does not watch the result); Retry, Refresh and the next show do that.
  useEffect(() => {
    if (!focused) return;
    if (!entry.conv || entry.convStale) void entry.loadConversation();
  }, [entry, focused]);

  // An anchored comment scrolls into view and is highlighted for two seconds, then the anchor clears.
  const settled = Boolean(entry.conv) || Boolean(entry.convError);
  useEffect(() => {
    if (!anchor || !settled) return;
    const id = anchor.kind === 'issue' ? `issuecomment-${anchor.id}` : `discussion_r${anchor.id}`;
    const el = document.getElementById(id);
    let timer: ReturnType<typeof setTimeout> | undefined;
    if (el) {
      el.scrollIntoView({ block: 'center' });
      el.classList.add('pr-flash');
      timer = setTimeout(() => el.classList.remove('pr-flash'), 2000);
    }
    clearAnchor();
    return () => {
      if (timer) clearTimeout(timer);
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [anchor?.id, anchor?.kind, settled]);

  if (!pr) return null;
  return (
    <div className="pr-conv" data-testid="pr-conversation">
      <div className="pr-conv-main">
        <div className="pr-comment" id="pr-description" data-testid="pr-description">
          <Avatar login={pr.author} size={32} />
          <div className="pr-comment-main">
            <div className="pr-comment-head">
              <strong>{pr.author}</strong> commented{' '}
              <span title={fullDate(pr.createdAt)} className="pr-muted">
                {relTime(pr.createdAt)}
              </span>{' '}
              <AssociationBadge value={pr.authorAssociation} />
            </div>
            <CommentBody text={pr.body} />
          </div>
        </div>
        <StatusCard entry={entry} pr={pr} />
        <Timeline entry={entry} />
      </div>
      <Sidebar entry={entry} pr={pr} />
    </div>
  );
}

function Timeline({ entry }: { entry: PrEntry }) {
  if (entry.convError) return <ErrorBlock message={entry.convError} onRetry={() => void entry.loadConversation(true)} />;
  if (!entry.conv) return <div className="pr-loading">Loading the timeline…</div>;
  return (
    <div className="pr-timeline" data-testid="pr-timeline">
      {entry.conv.data.map((item) => (
        <TimelineRow key={item.key} entry={entry} item={item} />
      ))}
      {entry.conv.truncated && <div className="pr-fine">Only part of the activity is shown. See all of it on GitHub.</div>}
      {entry.convLoading && <div className="pr-fine">Refreshing…</div>}
    </div>
  );
}

const REVIEW_TEXT: Record<string, string> = {
  APPROVED: 'approved these changes',
  CHANGES_REQUESTED: 'requested changes',
  COMMENTED: 'reviewed',
  DISMISSED: 'reviewed (dismissed)',
  PENDING: 'started a review',
};

function TimelineRow({ entry, item }: { entry: PrEntry; item: TimelineItem }) {
  switch (item.type) {
    case 'comment':
      return (
        <div className="pr-comment" id={`issuecomment-${item.id}`} data-testid="pr-issue-comment">
          <Avatar login={item.author} size={32} />
          <div className="pr-comment-main">
            <div className="pr-comment-head">
              <strong>{item.author || 'Someone'}</strong> commented{' '}
              <span className="pr-muted" title={fullDate(item.at)}>
                {relTime(item.at)}
              </span>
              {item.edited && <span className="pr-muted"> · edited</span>}{' '}
              <AssociationBadge value={item.association} />
            </div>
            <CommentBody text={item.body} empty="" />
          </div>
        </div>
      );
    case 'review':
      return (
        <div className="pr-review" data-testid="pr-review">
          <div className={`pr-event pr-review-${item.state.toLowerCase()}`}>
            <Avatar login={item.author} size={20} />
            <span>
              <strong>{item.author || 'Someone'}</strong> {REVIEW_TEXT[item.state] ?? 'reviewed'}{' '}
              <span className="pr-muted" title={fullDate(item.at)}>
                {relTime(item.at)}
              </span>
            </span>
          </div>
          {item.body.trim() && (
            <div className="pr-comment-main pr-review-body">
              <CommentBody text={item.body} empty="" />
            </div>
          )}
          {item.threads.map((t) => (
            <ThreadCard key={t.id} entry={entry} thread={t} />
          ))}
        </div>
      );
    case 'commit':
      return (
        <div className="pr-event" data-testid="pr-tl-commit">
          <Avatar login={item.author} size={20} />
          <span className="pr-event-text">{item.headline || 'No commit message'}</span>
          <code className="pr-sha">{item.sha.slice(0, 7)}</code>
        </div>
      );
    case 'event':
      return (
        <div className="pr-event">
          <Avatar login={item.actor} size={20} />
          <span>
            <strong>{item.actor || 'Someone'}</strong> {item.text}{' '}
            <span className="pr-muted" title={fullDate(item.at)}>
              {relTime(item.at)}
            </span>
          </span>
        </div>
      );
    case 'thread':
      return <ThreadCard entry={entry} thread={item.thread} />;
  }
}

// ---- sidebar ----

function draftText(kind: 'ready' | 'draft', r: PrWriteResult): string {
  const note = r.recordError ? ` The task could not be updated: ${r.recordError}` : r.overridden ? ' Recorded as your override on the task.' : '';
  switch (r.outcome) {
    case 'routed':
      return 'The task marked it ready after checking the approved commit.';
    case 'alreadyReady':
      return 'It was already ready for review.';
    case 'alreadyDraft':
      return 'It was already a draft.';
    case 'tracked':
      return 'Converted to a draft. A task still tracks it, and its land step will mark it ready again if it lands it.';
    case 'converted':
      return 'Converted to a draft.';
    default:
      return (kind === 'ready' ? 'Marked ready for review.' : 'Converted to a draft.') + note;
  }
}

function Sidebar({ entry, pr }: { entry: PrEntry; pr: PrSummary }) {
  return (
    <aside className="pr-side" aria-label="Pull request details">
      <div className="pr-side-block">
        <h4>Reviewers</h4>
        {pr.reviewers.length ? pr.reviewers.map((r) => <div key={r} className="pr-person"><Avatar login={r} size={18} /> {r}</div>) : <div className="pr-muted">No reviews requested</div>}
        <DraftControl entry={entry} pr={pr} />
      </div>
      <div className="pr-side-block">
        <h4>Assignees</h4>
        {pr.assignees.length ? pr.assignees.map((r) => <div key={r} className="pr-person"><Avatar login={r} size={18} /> {r}</div>) : <div className="pr-muted">No one assigned</div>}
      </div>
      <div className="pr-side-block">
        <h4>Labels</h4>
        {pr.labels.length ? (
          <div className="pr-labels">
            {pr.labels.map((l) => (
              <span key={l.name} className="pr-label" style={l.color ? { borderColor: l.color, color: 'var(--text)', background: `color-mix(in srgb, ${l.color} 22%, transparent)` } : undefined}>
                {l.name}
              </span>
            ))}
          </div>
        ) : (
          <div className="pr-muted">None yet</div>
        )}
      </div>
    </aside>
  );
}

/** Ready for review (on a draft) or Convert to draft (on an open pull request). One write per click. */
function DraftControl({ entry, pr }: { entry: PrEntry; pr: PrSummary }) {
  const [msg, setMsg] = useState<{ text: string; bad: boolean } | null>(null);
  const [confirmAnyway, setConfirmAnyway] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  if (pr.state !== 'OPEN') return null;
  const run = (e: React.MouseEvent, kind: 'ready' | 'draft', anyway = false) => {
    const click = clickFrom(e.nativeEvent);
    if (!click || busy) return;
    setBusy(true);
    setMsg(null);
    void entry
      .enqueue(() => attempt(() => (kind === 'ready' ? entry.client.ready(click, entry.ref, anyway) : entry.client.draft(click, entry.ref))))
      .then((r) => {
        setBusy(false);
        if (r.ok) {
          setConfirmAnyway(null);
          setMsg({ text: draftText(kind, r.value), bad: false });
          void entry.poll();
        } else if (r.cls === 'Tracked' && kind === 'ready') {
          setConfirmAnyway(r.message);
        } else {
          setMsg({ text: r.message, bad: true });
          if (r.kind === 'uncertain') void entry.poll();
        }
      });
  };
  return (
    <div className="pr-draft-control">
      {pr.draft ? (
        <>
          <div className="pr-muted">This pull request is still a work in progress.</div>
          <Button variant="secondary" busy={busy} onClick={(e) => run(e, 'ready')} data-testid="pr-ready">
            Ready for review
          </Button>
        </>
      ) : (
        <div className="pr-muted">
          Still in progress?{' '}
          <button type="button" className="pr-link-btn" disabled={busy} onClick={(e) => run(e, 'draft')} data-testid="pr-draft">
            {busy ? 'Converting…' : 'Convert to draft'}
          </button>
        </div>
      )}
      {confirmAnyway && (
        <div className="pr-confirm" role="alertdialog" aria-label="Mark ready anyway">
          <div>{confirmAnyway}</div>
          <div className="pr-box-actions">
            <Button variant="secondary" autoFocus onClick={() => setConfirmAnyway(null)}>
              Cancel
            </Button>
            <Button variant="primary" onClick={(e) => run(e, 'ready', true)}>
              Mark ready anyway
            </Button>
          </div>
        </div>
      )}
      {msg && (
        <div className={msg.bad ? 'pr-fine pr-bad' : 'pr-fine'} role={msg.bad ? 'alert' : 'status'}>
          {msg.text}
        </div>
      )}
    </div>
  );
}

// ---- status card and merge box ----

const DECISION_TEXT = {
  APPROVED: 'This pull request has an approving review.',
  CHANGES_REQUESTED: 'A reviewer requested changes.',
  REVIEW_REQUIRED: 'At least one approving review is required.',
} as const;

function checksText(pr: PrSummary): string {
  const n = pr.checksTotal ? ` (${pr.checksTotal} total)` : '';
  switch (pr.checksState) {
    case 'SUCCESS':
      return `All checks have passed${n}`;
    case 'FAILURE':
    case 'ERROR':
      return `Some checks were not successful${n}`;
    case 'PENDING':
    case 'EXPECTED':
      return `Some checks are still running${n}`;
    default:
      return 'No checks';
  }
}

function StatusCard({ entry, pr }: { entry: PrEntry; pr: PrSummary }) {
  return (
    <div className="pr-status" data-testid="pr-status" aria-label="Merge status">
      <div className="pr-status-row">
        <span className={`pr-dot pr-dot-${pr.reviewDecision === 'APPROVED' ? 'ok' : pr.reviewDecision === 'CHANGES_REQUESTED' ? 'bad' : 'wait'}`} aria-hidden="true" />
        <span>{pr.reviewDecision ? DECISION_TEXT[pr.reviewDecision] : 'No review is required.'}</span>
      </div>
      <div className="pr-status-row">
        <span className={`pr-dot pr-dot-${pr.checksState === 'SUCCESS' ? 'ok' : pr.checksState === 'FAILURE' || pr.checksState === 'ERROR' ? 'bad' : pr.checksState ? 'wait' : 'none'}`} aria-hidden="true" />
        <span>{checksText(pr)}</span>
        <Button variant="text" onClick={() => entry.goto('checks')}>
          Show checks
        </Button>
      </div>
      <MergeBox entry={entry} pr={pr} />
    </div>
  );
}

function mergeText(r: PrWriteResult): { text: string; kind: 'ok' | 'warn' } {
  if (r.outcome === 'routed') return { text: 'Passed to the task, which will merge this change.', kind: 'ok' };
  if (r.outcome === 'accepted') return { text: 'GitHub accepted the merge but has not shown it as merged yet. Refresh in a moment.', kind: 'warn' };
  const note = r.recordError ? ` It could not be recorded on the task: ${r.recordError}` : r.overridden ? ' Recorded as your override on the task.' : '';
  return { text: `Merged.${note}`, kind: r.recordError ? 'warn' : 'ok' };
}

function blockedReason(pr: PrSummary): string | null {
  if (pr.mergeableState === 'dirty') return 'This branch has conflicts that must be resolved.';
  if (pr.mergeableState === 'behind') return 'This branch is behind the base branch.';
  if (pr.mergeableState === 'blocked') return 'GitHub says merging is blocked (a review or check may be required).';
  if (pr.mergeableState === 'unstable') return 'Some checks are failing.';
  return null;
}

function MergeBox({ entry, pr }: { entry: PrEntry; pr: PrSummary }) {
  const [confirm, setConfirm] = useState(false);
  const [anyway, setAnyway] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [msg, setMsg] = useState<{ text: string; kind: 'ok' | 'bad' | 'warn'; refresh?: boolean } | null>(null);
  const land = entry.land;

  if (pr.state === 'MERGED') {
    return (
      <div className="pr-merge" data-testid="pr-merged">
        Merged{pr.mergedBy ? ` by ${pr.mergedBy}` : ''} {pr.mergedAt ? relTime(pr.mergedAt) : ''}
      </div>
    );
  }
  if (pr.state === 'CLOSED') return <div className="pr-merge">This pull request is closed.</div>;

  const tracked = land?.tracked ?? null;
  const reason = blockedReason(pr);
  let disabled: string | null = null;
  if (pr.draft) disabled = 'Ready for review first';
  else if (pr.mergeableState === 'dirty') disabled = 'Merge conflicts';
  else if (land?.rule === 'draft') disabled = 'This repository is merged by hand.';
  else if (tracked?.busy) disabled = 'The task is landing this change.';
  else if (!land && !entry.landError) disabled = 'Checking how this repository merges…';
  else if (entry.landError) disabled = `Can’t read how this repository merges: ${entry.landError}`;

  const label = tracked?.routes === 'merge' ? 'Merge (via task)' : (METHOD_LABEL[land?.method ?? ''] ?? 'Merge');

  const run = (e: React.MouseEvent, withAnyway: boolean) => {
    const click = clickFrom(e.nativeEvent);
    if (!click || busy) return;
    setBusy(true);
    setMsg(null);
    void entry
      .enqueue(() => attempt<PrWriteResult>(() => entry.client.merge(click, entry.ref, pr.headSha, withAnyway)))
      .then((r) => {
        setBusy(false);
        if (r.ok) {
          setConfirm(false);
          setAnyway(null);
          setMsg(mergeText(r.value));
          void entry.afterPublish();
        } else if (r.cls === 'Tracked') {
          setConfirm(false);
          setAnyway(r.message);
        } else if (r.cls === 'HeadMoved') {
          setMsg({ text: 'New commits since you looked; refresh first.', kind: 'bad', refresh: true });
          void entry.poll();
        } else {
          setConfirm(false);
          setMsg({ text: r.kind === 'uncertain' ? r.message : `${r.message} Nothing was merged.`, kind: r.kind === 'uncertain' ? 'warn' : 'bad' });
          if (r.kind === 'uncertain') void entry.poll();
        }
      });
  };

  return (
    <div className="pr-merge" data-testid="pr-merge">
      {reason && !disabled && <div className="pr-fine pr-warn">{reason}</div>}
      {disabled && <div className="pr-fine">{disabled}</div>}
      {!confirm && !anyway && (
        <Button variant="primary" disabled={disabled !== null} onClick={() => setConfirm(true)} data-testid="pr-merge-button">
          {label}
        </Button>
      )}
      {confirm && (
        <div className="pr-confirm" role="alertdialog" aria-label="Confirm merge" onKeyDown={(e) => e.key === 'Escape' && setConfirm(false)}>
          <div>
            {label} into <code>{pr.baseRef}</code>?
          </div>
          <div className="pr-box-actions">
            <Button variant="secondary" autoFocus onClick={() => setConfirm(false)}>
              Cancel
            </Button>
            <Button variant="primary" busy={busy} onClick={(e) => run(e, false)} data-testid="pr-merge-confirm">
              {busy ? 'Merging…' : 'Confirm merge'}
            </Button>
          </div>
        </div>
      )}
      {anyway && (
        <div className="pr-confirm" role="alertdialog" aria-label="Merge anyway">
          <div>{anyway}</div>
          <div className="pr-box-actions">
            <Button variant="secondary" autoFocus onClick={() => setAnyway(null)}>
              Cancel
            </Button>
            <Button variant="primary" busy={busy} onClick={(e) => run(e, true)} data-testid="pr-merge-anyway">
              Merge anyway
            </Button>
          </div>
        </div>
      )}
      {msg && (
        <div className={msg.kind === 'bad' ? 'pr-fine pr-bad' : msg.kind === 'warn' ? 'pr-fine pr-warn' : 'pr-fine'} role={msg.kind === 'ok' ? 'status' : 'alert'} data-testid="pr-merge-msg">
          {msg.text}{' '}
          {msg.refresh && (
            <button type="button" className="pr-link-btn" onClick={() => void entry.refresh()}>
              Refresh
            </button>
          )}
        </div>
      )}
    </div>
  );
}

