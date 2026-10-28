// The PR tab body: header, view bar, and the shown view. Loaded as its own chunk on first show.

import { useEffect, useState } from 'react';
import type { KeyboardEvent } from 'react';
import { panelStore } from '../panel/registry.ts';
import type { PanelTab } from '../panel/model.ts';
import type { TabContext } from '../panel/registry.ts';
import { Button } from '../ui/index.ts';
import { addToChat, newChatWith } from './actions.ts';
import { prBlock } from './addToChat.ts';
import { ChecksView, CommitsView } from './Commits.tsx';
import { ConversationView } from './Conversation.tsx';
import { FilesView } from './Files.tsx';
import { statBlocks } from './files.ts';
import { prKey } from './links.ts';
import { openOnGitHub } from './open.ts';
import { ReviewButton, ReviewPanel } from './ReviewPanel.tsx';
import { POLL_MS, getEntry } from './store.ts';
import type { PrEntry } from './store.ts';
import type { PrTabData, PrView } from './types.ts';
import { StatePill, WriteBox, ageNote, useEntry } from './util.tsx';

const VIEWS: { id: PrView; label: string }[] = [
  { id: 'conversation', label: 'Conversation' },
  { id: 'commits', label: 'Commits' },
  { id: 'checks', label: 'Checks' },
  { id: 'files', label: 'Files changed' },
];

export function entryFor(data: PrTabData): PrEntry {
  const api = panelStore.api;
  if (!api) throw new Error('The side panel is not connected to the host yet.');
  return getEntry(data, api);
}

export default function PrTab({ tab, ctx }: { tab: PanelTab; ctx: TabContext }) {
  const data = tab.data as PrTabData;
  const entry = entryFor(data);
  useEntry(entry);
  const pr = entry.summary;

  // What the loaders may rely on: shown, focused, and which view.
  entry.setShown(ctx.visible, ctx.focused, data.view);
  entry.goto = (view) => ctx.setData({ owner: data.owner, repo: data.repo, number: data.number, view });

  // The first read when the tab is shown, then the header poll while shown and focused.
  useEffect(() => {
    if (ctx.visible && !entry.summary && entry.lastAttempt === 0) void entry.poll();
  }, [entry, ctx.visible]);
  useEffect(() => {
    if (!ctx.focused) return;
    void entry.pollIfStale();
    const t = setInterval(() => void entry.poll(), POLL_MS);
    return () => clearInterval(t);
  }, [entry, ctx.focused]);

  useEffect(() => {
    if (pr) ctx.setTitle(`#${data.number} ${pr.title}`);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [pr?.title, data.number]);

  if (!pr) {
    if (entry.summaryError) {
      return (
        <div className="pr-root">
          <div className="pr-error" role="alert" data-testid="pr-error">
            <div>{entry.summaryError}</div>
            <Button variant="secondary" onClick={() => void entry.poll()}>
              Retry
            </Button>
          </div>
        </div>
      );
    }
    return <div className="pr-root pr-loading">{ctx.visible ? 'Loading the pull request…' : ''}</div>;
  }

  const select = (view: PrView) => ctx.setData({ owner: data.owner, repo: data.repo, number: data.number, view });
  const onKeys = (e: KeyboardEvent) => {
    if (e.key !== 'ArrowRight' && e.key !== 'ArrowLeft') return;
    const i = VIEWS.findIndex((v) => v.id === data.view);
    const next = VIEWS[(i + (e.key === 'ArrowRight' ? 1 : VIEWS.length - 1)) % VIEWS.length];
    if (next) {
      e.preventDefault();
      select(next.id);
      requestAnimationFrame(() => document.querySelector<HTMLElement>(`[data-view-tab="${next.id}"]`)?.focus());
    }
  };
  const count: Record<PrView, string> = {
    conversation: String(pr.comments),
    commits: String(pr.commits),
    checks: pr.checksTotal >= 100 ? '100+' : String(pr.checksTotal),
    files: String(pr.changedFiles),
  };
  const noun = pr.commits === 1 ? 'commit' : 'commits';
  const merge =
    pr.state === 'MERGED' ? (
      <>
        <strong>{pr.mergedBy ?? pr.author}</strong> merged {pr.commits} {noun} into <code>{pr.baseRef}</code> from <code>{pr.headRef}</code>
      </>
    ) : (
      <>
        <strong>{pr.author}</strong> {pr.state === 'CLOSED' ? 'wanted to merge' : 'wants to merge'} {pr.commits} {noun} into <code>{pr.baseRef}</code> from <code>{pr.headRef}</code>
      </>
    );

  return (
    <div className="pr-root" data-testid="pr-tab" data-pr={prKey(data)} data-view={data.view}>
      <header className="pr-header">
        <h2 className="pr-title">
          <span dir="auto">{pr.title}</span> <span className="pr-num">#{data.number}</span>
        </h2>
        <div className="pr-meta">
          <StatePill state={pr.state} draft={pr.draft} />
          <span className="pr-merge-line" data-testid="pr-merge-line">
            {merge}
          </span>
        </div>
        <div className="pr-tabbar" role="tablist" aria-label="Pull request views" onKeyDown={onKeys}>
          {VIEWS.map((v) => (
            <button
              key={v.id}
              type="button"
              role="tab"
              data-view-tab={v.id}
              data-testid={`pr-view-${v.id}`}
              aria-selected={data.view === v.id}
              tabIndex={data.view === v.id ? 0 : -1}
              className={data.view === v.id ? 'pr-viewtab active' : 'pr-viewtab'}
              onClick={() => select(v.id)}
            >
              {v.label} <span className="pr-count">{count[v.id]}</span>
            </button>
          ))}
          <span className="pr-spacer" />
          <span className="pr-diffstat" role="img" aria-label={`${pr.additions} lines added, ${pr.deletions} removed`}>
            <span className="pr-add">+{pr.additions}</span> <span className="pr-del">−{pr.deletions}</span>
            <span className="pr-blocks" aria-hidden="true">
              {statBlocks(pr.additions, pr.deletions).map((k, i) => (
                <i key={i} className={`pr-block-${k}`} />
              ))}
            </span>
          </span>
          <ReviewButton entry={entry} />
        </div>
      </header>
      <ReviewPanel entry={entry} />
      <CommentBox entry={entry} />
      <Note entry={entry} />
      {entry.summaryError && (
        <div className="pr-fine pr-warn" role="status">
          {entry.summaryError}
        </div>
      )}
      <div className="pr-body">
        {data.view === 'conversation' && (
          <ConversationView entry={entry} anchor={data.anchor} clearAnchor={() => ctx.setData({ owner: data.owner, repo: data.repo, number: data.number, view: data.view })} />
        )}
        {data.view === 'commits' && <CommitsView entry={entry} />}
        {data.view === 'checks' && <ChecksView entry={entry} focused={ctx.focused} />}
        {data.view === 'files' && <FilesView entry={entry} />}
      </div>
    </div>
  );
}

/** The header's Comment box: one top-level comment on the conversation. */
function CommentBox({ entry }: { entry: PrEntry }) {
  useEntry(entry);
  if (!entry.ui.commentOpen) return null;
  const close = () => {
    entry.ui.commentOpen = false;
    entry.changed();
  };
  return (
    <div className="pr-comment-box">
      <WriteBox
        entry={entry}
        id="comment"
        label="Leave a comment"
        rows={4}
        autoFocus
        onCancel={close}
        note="This posts to the pull request’s conversation, where everyone who can see it can read it."
        actions={[
          {
            label: 'Comment',
            primary: true,
            run: (click, text) => entry.client.comment(click, entry.ref, text),
            done: () => {
              entry.ui.commentOpen = false;
              entry.ui.note = 'Comment posted.';
              void entry.afterPublish();
            },
            uncertain: () => {
              void entry.afterPublish();
            },
          },
        ]}
      />
    </div>
  );
}

/** A short status line that clears itself after four seconds. */
function Note({ entry }: { entry: PrEntry }) {
  useEntry(entry);
  const text = entry.ui.note as string | undefined;
  useEffect(() => {
    if (!text) return;
    const t = setTimeout(() => {
      entry.ui.note = undefined;
      entry.changed();
    }, 4000);
    return () => clearTimeout(t);
  }, [entry, text]);
  return text ? (
    <div className="pr-fine pr-note" role="status" data-testid="pr-note">
      {text}
    </div>
  ) : null;
}

/** The tab strip's actions: Refresh with its note, Add to chat, New chat, Comment, Open on GitHub. */
export function HeaderActions({ tab, ctx }: { tab: PanelTab; ctx: TabContext }) {
  const data = tab.data as PrTabData;
  const entry = entryFor(data);
  useEntry(entry);
  const [, tick] = useState(0);
  const [refreshing, setRefreshing] = useState(false);
  useEffect(() => {
    const t = setInterval(() => tick((n) => n + 1), 5000);
    return () => clearInterval(t);
  }, []);
  const pr = entry.summary;
  const oldest = entry.oldestAt();
  const note = (text: string) => {
    entry.ui.note = text;
    entry.changed();
  };
  return (
    <>
      <span className="pr-updated" aria-live="off" data-testid="pr-updated">
        {Number.isFinite(oldest) ? `updated ${ageNote(oldest)}` : ''}
      </span>
      <Button
        variant="text"
        busy={refreshing}
        data-testid="pr-refresh"
        onClick={() => {
          setRefreshing(true);
          void entry.refresh().then((r) => {
            setRefreshing(false);
            if (r === 'failed') note('Couldn’t refresh. Try again.');
          });
        }}
      >
        Refresh
      </Button>
      <Button variant="text" disabled={!pr} data-testid="pr-add-to-chat" onClick={() => pr && note(addToChat(prBlock(entry.ref, pr)).text)}>
        Add to chat
      </Button>
      <Button variant="text" disabled={!pr} data-testid="pr-new-chat" title="Start a new chat with this pull request in its draft" onClick={() => pr && note(newChatWith(prBlock(entry.ref, pr)).text)}>
        New chat
      </Button>
      <Button
        variant="text"
        disabled={!pr}
        title={pr ? 'Add a comment to the conversation on GitHub' : 'Loading the pull request…'}
        data-testid="pr-comment-button"
        onClick={() => {
          entry.ui.commentOpen = !entry.ui.commentOpen;
          entry.changed();
        }}
      >
        Comment
      </Button>
      <Button variant="text" data-testid="pr-open-github" onClick={() => openOnGitHub(entry.ref, data.view)}>
        Open on GitHub
      </Button>
      {ctx.visible ? null : null}
    </>
  );
}
