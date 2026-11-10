// What goes with a comment: your reactions, and Edit and Delete on your own. One block is used for
// conversation comments and for the comments inside a review thread.

import { useState } from 'react';
import type { ReactNode } from 'react';
import { Button } from '../ui/index.ts';
import { attempt, clickFrom } from './client.ts';
import type { PrEntry } from './store.ts';
import type { Reaction } from './types.ts';
import { WriteBox } from './util.tsx';

/** GitHub's eight reactions, in the order its picker shows them. */
export const REACTIONS: { content: string; emoji: string; label: string }[] = [
  { content: 'THUMBS_UP', emoji: '👍', label: 'Thumbs up' },
  { content: 'THUMBS_DOWN', emoji: '👎', label: 'Thumbs down' },
  { content: 'LAUGH', emoji: '😄', label: 'Laugh' },
  { content: 'HOORAY', emoji: '🎉', label: 'Hooray' },
  { content: 'CONFUSED', emoji: '😕', label: 'Confused' },
  { content: 'HEART', emoji: '❤️', label: 'Heart' },
  { content: 'ROCKET', emoji: '🚀', label: 'Rocket' },
  { content: 'EYES', emoji: '👀', label: 'Eyes' },
];

const emojiOf = (content: string) => REACTIONS.find((r) => r.content === content)?.emoji ?? content;

function Reactions({ entry, commentId, initial }: { entry: PrEntry; commentId: string; initial: Reaction[] }) {
  const [now, setNow] = useState<Reaction[] | null>(null);
  const [picking, setPicking] = useState(false);
  const [busy, setBusy] = useState(false);
  const [problem, setProblem] = useState('');
  const shown = now ?? initial;
  const react = (e: React.MouseEvent, content: string) => {
    const click = clickFrom(e.nativeEvent);
    if (!click || busy) return;
    setBusy(true);
    setProblem('');
    setPicking(false);
    void entry
      .enqueue(() => attempt(() => entry.client.react(click, entry.ref, commentId, content)))
      .then((r) => {
        setBusy(false);
        if (r.ok) setNow(r.value);
        else setProblem(r.message);
      });
  };
  return (
    <span className="pr-reactions" data-testid="pr-reactions">
      {shown.map((r) => (
        <button
          key={r.content}
          type="button"
          className="pr-reaction"
          aria-pressed={r.mine}
          disabled={busy}
          title={r.mine ? 'Take your reaction back' : 'React the same way'}
          onClick={(e) => react(e, r.content)}
        >
          {emojiOf(r.content)} {r.count}
        </button>
      ))}
      <button type="button" className="pr-reaction pr-reaction-add" aria-label="Add a reaction" aria-expanded={picking} disabled={busy} onClick={() => setPicking(!picking)}>
        ☺+
      </button>
      {picking && (
        <span className="pr-reaction-picker" role="group" aria-label="Pick a reaction">
          {REACTIONS.map((r) => (
            <button key={r.content} type="button" className="pr-reaction" title={r.label} aria-label={r.label} onClick={(e) => react(e, r.content)}>
              {r.emoji}
            </button>
          ))}
        </span>
      )}
      {problem && (
        <span className="pr-fine pr-bad" role="alert">
          {problem}
        </span>
      )}
    </span>
  );
}

/**
 * A comment's body (`children`), its reactions, and for your own comment Edit and Delete. A save or
 * a delete runs `changed`, which reloads whatever shows the comment.
 */
export function CommentBlock({
  entry,
  commentId,
  text,
  mine,
  reactions,
  changed,
  children,
}: {
  entry: PrEntry;
  commentId: string;
  text: string;
  mine: boolean;
  reactions: Reaction[];
  changed: () => void;
  children: ReactNode;
}) {
  const [editing, setEditing] = useState(false);
  const [confirm, setConfirm] = useState(false);
  const [busy, setBusy] = useState(false);
  const [problem, setProblem] = useState('');
  const boxId = `edit:${commentId}`;
  const startEdit = () => {
    if (!entry.drafts.has(boxId)) entry.drafts.set(boxId, text);
    setConfirm(false);
    setEditing(true);
  };
  const remove = (e: React.MouseEvent) => {
    const click = clickFrom(e.nativeEvent);
    if (!click || busy) return;
    setBusy(true);
    setProblem('');
    void entry
      .enqueue(() => attempt(() => entry.client.commentDelete(click, entry.ref, commentId)))
      .then((r) => {
        setBusy(false);
        if (r.ok) {
          setConfirm(false);
          changed();
        } else {
          setProblem(r.message);
          if (r.kind === 'uncertain') changed();
        }
      });
  };
  if (editing) {
    return (
      <WriteBox
        entry={entry}
        id={boxId}
        label="Edit your comment"
        rows={5}
        autoFocus
        onCancel={() => {
          entry.drafts.delete(boxId);
          setEditing(false);
        }}
        actions={[
          {
            label: 'Save',
            primary: true,
            run: (click, body) => entry.client.commentEdit(click, entry.ref, commentId, body),
            done: () => {
              setEditing(false);
              changed();
            },
            uncertain: changed,
          },
        ]}
      />
    );
  }
  return (
    <>
      {children}
      <div className="pr-comment-tools">
        <Reactions entry={entry} commentId={commentId} initial={reactions} />
        {mine && !confirm && (
          <span className="pr-own-actions">
            <button type="button" className="pr-link-btn" onClick={startEdit} data-testid="pr-comment-edit">
              Edit
            </button>
            <button type="button" className="pr-link-btn" onClick={() => setConfirm(true)} data-testid="pr-comment-delete">
              Delete
            </button>
          </span>
        )}
      </div>
      {confirm && (
        <div className="pr-confirm" role="alertdialog" aria-label="Delete this comment">
          <div>Delete this comment? It can’t be brought back.</div>
          <div className="pr-box-actions">
            <Button variant="secondary" autoFocus onClick={() => setConfirm(false)}>
              Cancel
            </Button>
            <Button variant="primary" busy={busy} onClick={remove} data-testid="pr-comment-delete-confirm">
              {busy ? 'Deleting…' : 'Delete'}
            </Button>
          </div>
        </div>
      )}
      {problem && (
        <div className="pr-fine pr-bad" role="alert">
          {problem}
        </div>
      )}
    </>
  );
}
