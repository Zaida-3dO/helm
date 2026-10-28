// Comments and review threads: one comment body, and a thread card used inline in Files and in the
// Conversation timeline. Reply and Resolve are the only writes here, each from its own click.

import { useState } from 'react';
import { Markdown } from '../markdown/index.ts';
import type { LinkTarget } from '../markdown/index.ts';
import { handleLink } from '../panel/links.ts';
import { Button } from '../ui/index.ts';
import { attempt, clickFrom } from './client.ts';
import type { PrEntry } from './store.ts';
import type { Thread, ThreadComment } from './types.ts';
import { AssociationBadge, Avatar, WriteBox, fullDate, relTime, useEntry } from './util.tsx';

export function openLink(t: LinkTarget): void {
  void handleLink(t, { chatId: null });
}

export function CommentBody({ text, empty = 'No description provided.' }: { text: string; empty?: string }) {
  if (!text.trim()) return <div className="pr-empty">{empty}</div>;
  return (
    <div className="pr-md">
      <Markdown text={text} onLink={openLink} />
    </div>
  );
}

function ThreadComments({ comments }: { comments: ThreadComment[] }) {
  return (
    <>
      {comments.map((c) => (
        <div key={c.id} className="pr-tc" id={`discussion_r${c.databaseId}`} data-comment-id={c.databaseId}>
          <Avatar login={c.author} size={20} />
          <div className="pr-tc-main">
            <div className="pr-tc-head">
              <strong>{c.author || 'Someone'}</strong> <AssociationBadge value={c.authorAssociation} />
              <span className="pr-muted" title={fullDate(c.createdAt)}>
                {relTime(c.createdAt)}
              </span>
            </div>
            <CommentBody text={c.body} empty="" />
          </div>
        </div>
      ))}
    </>
  );
}

/** The last four lines of the hunk, as text: a line's colour comes only from its first character. */
function HunkTail({ hunk }: { hunk: string }) {
  const lines = hunk.split('\n').filter((l) => !l.startsWith('@@')).slice(-4);
  if (lines.length === 0) return null;
  return (
    <pre className="pr-hunk" aria-label="The code this conversation is about">
      {lines.map((l, i) => (
        <span key={i} className={l.startsWith('+') ? 'pr-hunk-add' : l.startsWith('-') ? 'pr-hunk-del' : undefined}>
          {l}
          {'\n'}
        </span>
      ))}
    </pre>
  );
}

export function ThreadCard({
  entry,
  thread,
  inline,
  open: forceOpen,
}: {
  entry: PrEntry;
  thread: Thread;
  /** Shown under a diff line (no path header or code excerpt). */
  inline?: boolean;
  open?: boolean;
}) {
  useEntry(entry);
  const [folded, setFolded] = useState<boolean | null>(null);
  const isFolded = forceOpen ? false : (folded ?? thread.resolved);
  const rid = `resolve:${thread.id}`;
  const outcome = entry.outcomes.get(rid);
  const busy = entry.sending.has(rid);
  const place = thread.line ? `${thread.path}:${thread.line}` : thread.path;
  const resolve = (e: React.MouseEvent) => {
    const click = clickFrom(e.nativeEvent);
    if (!click || entry.sending.has(rid)) return;
    entry.sending.add(rid);
    entry.outcomes.delete(rid);
    entry.changed();
    void entry
      .enqueue(() => attempt(() => entry.client.resolve(click, thread.id, !thread.resolved)))
      .then((r) => {
        entry.sending.delete(rid);
        if (r.ok) entry.patchThread(thread.id, { resolved: !thread.resolved });
        else {
          entry.outcomes.set(rid, { kind: r.kind, text: r.kind === 'uncertain' ? 'Couldn’t confirm the conversation changed. Click again only if it didn’t.' : r.message });
          if (r.kind === 'uncertain') void entry.loadThreads();
        }
        entry.changed();
      });
  };
  return (
    <div className={`pr-thread${inline ? ' inline' : ''}${thread.resolved ? ' resolved' : ''}`} data-testid="pr-thread" data-thread-id={thread.id}>
      <div className="pr-thread-head">
        <button type="button" className="pr-link-btn" aria-expanded={!isFolded} onClick={() => setFolded(!isFolded)}>
          {isFolded ? 'Show thread' : 'Hide thread'}
        </button>
        <span className="pr-thread-path">{place}</span>
        {thread.outdated && <span className="pr-mark">Outdated</span>}
        {thread.resolved && <span className="pr-mark pr-mark-ok">Resolved</span>}
      </div>
      {!isFolded && (
        <>
          {!inline && <HunkTail hunk={thread.diffHunk} />}
          <ThreadComments comments={thread.comments} />
          <div className="pr-thread-actions">
            {thread.canReply && !thread.resolved && (
              <ReplyBox entry={entry} thread={thread} />
            )}
            {thread.canResolve && (
              <Button variant="secondary" busy={busy} onClick={resolve} data-testid="pr-resolve">
                {thread.resolved ? 'Unresolve conversation' : 'Resolve conversation'}
              </Button>
            )}
          </div>
          {outcome && outcome.kind !== 'ok' && (
            <div className={outcome.kind === 'uncertain' ? 'pr-fine pr-warn' : 'pr-fine pr-bad'} role="alert">
              {outcome.text}
            </div>
          )}
        </>
      )}
    </div>
  );
}

function ReplyBox({ entry, thread }: { entry: PrEntry; thread: Thread }) {
  const id = `reply:${thread.id}`;
  const [open, setOpen] = useState(() => entry.drafts.has(id));
  if (!open) {
    return (
      <Button variant="secondary" onClick={() => setOpen(true)} data-testid="pr-reply">
        Reply…
      </Button>
    );
  }
  return (
    <WriteBox
      entry={entry}
      id={id}
      label="Reply to this conversation"
      placeholder="Reply…"
      rows={3}
      autoFocus
      onCancel={() => setOpen(false)}
      actions={[
        {
          label: 'Reply',
          primary: true,
          run: (click, text) => entry.client.reply(click, entry.ref, thread.id, text),
          done: () => {
            setOpen(false);
            void entry.loadThreads();
          },
          uncertain: () => void entry.loadThreads(),
        },
      ]}
    />
  );
}
