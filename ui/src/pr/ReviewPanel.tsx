// "Review changes": one review at the commit the view shows. The review is sent as a whole
// on the Submit click (Comment, Approve or Request changes); there is no pending review.

import { useState } from 'react';
import { toast } from '../ui/toasts.ts';
import type { ReviewEvent } from './client.ts';
import type { PrEntry } from './store.ts';
import { WriteBox, useEntry } from './util.tsx';

const CHOICES: { value: ReviewEvent; title: string; hint: string }[] = [
  { value: 'COMMENT', title: 'Comment', hint: 'General feedback without explicit approval.' },
  { value: 'APPROVE', title: 'Approve', hint: 'Approve these changes.' },
  { value: 'REQUEST_CHANGES', title: 'Request changes', hint: 'Feedback that must be addressed before merging.' },
];

interface ReviewUi {
  open: boolean;
  choice: ReviewEvent;
}

export function reviewUi(entry: PrEntry): ReviewUi {
  let u = entry.ui.review as ReviewUi | undefined;
  if (!u) {
    u = { open: false, choice: 'COMMENT' };
    entry.ui.review = u;
  }
  return u;
}

/** The commit a review is for: the one Files shows, else the polled head. Null while neither is known. */
export function reviewHead(entry: PrEntry): { shown: string; polled: string } {
  const polled = entry.summary?.headSha ?? '';
  return { shown: entry.files?.data.headSha || polled, polled };
}

export function ReviewButton({ entry }: { entry: PrEntry }) {
  useEntry(entry);
  const pr = entry.summary;
  if (!pr || pr.state !== 'OPEN') return null;
  const ui = reviewUi(entry);
  return (
    <button
      type="button"
      className="pr-review-btn"
      aria-haspopup="dialog"
      aria-expanded={ui.open}
      data-testid="pr-review-button"
      onClick={() => {
        ui.open = !ui.open;
        entry.changed();
      }}
    >
      Review changes ▾
    </button>
  );
}

export function ReviewPanel({ entry }: { entry: PrEntry }) {
  useEntry(entry);
  const ui = reviewUi(entry);
  const [, bump] = useState(0);
  const pr = entry.summary;
  if (!ui.open || !pr || pr.state !== 'OPEN') return null;
  const own = pr.viewer !== null && pr.viewer === pr.author;
  const { shown, polled } = reviewHead(entry);
  const stale = shown !== polled && polled !== '';
  const choice = own ? 'COMMENT' : ui.choice;
  const close = () => {
    ui.open = false;
    entry.changed();
  };
  return (
    <div
      className="pr-review-panel"
      role="dialog"
      aria-label="Finish your review"
      data-testid="pr-review-panel"
      onKeyDown={(e) => {
        if (e.key === 'Escape') {
          e.stopPropagation();
          close();
        }
      }}
    >
      <div className="pr-review-title">
        <strong>Finish your review</strong>
        <button type="button" className="pr-link-btn" aria-label="Close" onClick={close}>
          ✕
        </button>
      </div>
      <div className="pr-choices" role="radiogroup" aria-label="Kind of review">
        {CHOICES.map((c) => {
          const off = own && c.value !== 'COMMENT';
          return (
            <label key={c.value} className={`pr-choice${off ? ' off' : ''}`}>
              <input
                type="radio"
                name="pr-review-choice"
                checked={choice === c.value}
                disabled={off}
                onChange={() => {
                  ui.choice = c.value;
                  bump((n) => n + 1);
                }}
              />
              <span>
                <strong>{c.title}</strong>
                <span className="pr-fine">{off ? `Authors can’t ${c.value === 'APPROVE' ? 'approve' : 'request changes on'} their own pull request.` : c.hint}</span>
              </span>
            </label>
          );
        })}
      </div>
      {stale && (
        <div className="pr-fine pr-warn" role="status">
          Files shows {shown.slice(0, 7)} but the pull request is now at {polled.slice(0, 7)}. Refresh Files first, so the review is of the code you saw.
        </div>
      )}
      <WriteBox
        entry={entry}
        id="review"
        label="Leave a comment"
        placeholder="Leave a comment"
        rows={5}
        autoFocus
        note={choice === 'REQUEST_CHANGES' ? 'Say what needs to change.' : 'The review is of the commit the view shows.'}
        actions={[
          {
            label: 'Submit review',
            busyLabel: 'Submitting…',
            primary: true,
            allowBlank: () => choice === 'APPROVE',
            run: (click, text) => {
              if (stale) throw Object.assign(new Error('[Refused] Refresh first, so the review is of the latest code.'));
              return entry.client.review(click, entry.ref, shown, choice, text);
            },
            done: () => {
              ui.choice = 'COMMENT';
              ui.open = false;
              toast.show({ kind: 'info', text: 'Review submitted.' });
              void entry.loadThreads();
              void entry.afterPublish();
            },
            uncertain: () => {
              void entry.loadThreads();
              void entry.poll();
            },
          },
        ]}
        onCancel={close}
      />
    </div>
  );
}
