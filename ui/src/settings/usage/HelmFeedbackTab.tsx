// Settings → Usage → Helm feedback: things to tell the maintainers, kept as threads. The user
// files them here; agents file and append with the app_*_feedback tools. Every item and entry shows
// who wrote it. Agents' words render as plain text only, never as Markdown or HTML.

import { useCallback, useEffect, useMemo, useState } from 'react';
import {
  feedbackOps,
  type FeedbackAuthor,
  type FeedbackAuthorKind,
  type FeedbackFilter,
  type FeedbackItemRow,
  type FeedbackStatus,
  type FeedbackThread,
} from '../../api/feedback.ts';
import { useShell } from '../../shell/context.ts';
import { Button, Segmented, TextArea, TextInput, toast } from '../../ui/index.ts';
import './feedback.css';

const STATUS_LABEL: Record<FeedbackStatus, string> = { open: 'Open', filed: 'Filed', done: 'Done', wont_do: "Won't do" };
const AUTHOR_LABEL: Record<FeedbackAuthorKind, string> = { human: 'You', agent: 'Agent', agent_for_user: 'Agent, for you' };
const MAX_TITLE = 120;
const MAX_TEXT = 4000;

type Ops = ReturnType<typeof feedbackOps>;

function errorText(e: unknown): string {
  return String((e as Error)?.message ?? e);
}

function AuthorBadge({ author }: { author: FeedbackAuthor }) {
  const tip =
    author.kind === 'human'
      ? 'Typed by you in Helm'
      : author.kind === 'agent'
        ? `An agent's own observation (chat ${author.chatId ?? 'unknown'})`
        : `An agent writing down what you said (chat ${author.chatId ?? 'unknown'})`;
  return (
    <span className="hf-badge" data-author={author.kind} title={tip} data-testid="helm-feedback-author">
      {AUTHOR_LABEL[author.kind]}
    </span>
  );
}

function when(ms: number): string {
  return new Date(ms).toLocaleString(undefined, { dateStyle: 'medium', timeStyle: 'short' });
}

export function HelmFeedbackTab() {
  const shell = useShell();
  const ops = useMemo(() => feedbackOps(shell.api), [shell.api]);
  const [status, setStatus] = useState<FeedbackStatus | 'any'>('any');
  const [author, setAuthor] = useState<FeedbackAuthorKind | 'any'>('any');
  const [query, setQuery] = useState('');
  const [items, setItems] = useState<FeedbackItemRow[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [open, setOpen] = useState<number | null>(null);
  const [filing, setFiling] = useState(false);
  const [rev, setRev] = useState(0);

  const reload = useCallback(() => setRev((n) => n + 1), []);

  useEffect(() => {
    let live = true;
    const f: FeedbackFilter = {
      ...(status !== 'any' ? { status } : {}),
      ...(author !== 'any' ? { author } : {}),
      ...(query.trim() ? { text: query.trim() } : {}),
    };
    // Typing in the search box waits a moment so each key does not hit the host.
    const t = setTimeout(
      () =>
        void ops.items(f).then(
          (r) => {
            if (!live) return;
            setItems(r.items);
            setError(null);
          },
          (e) => {
            if (live) setError(errorText(e));
          },
        ),
      query ? 200 : 0,
    );
    return () => {
      live = false;
      clearTimeout(t);
    };
  }, [ops, status, author, query, rev]);

  const unfiltered = status === 'any' && author === 'any' && !query.trim();
  return (
    <section data-testid="helm-feedback">
      <div className="usage-pane-head">
        <h3>Helm feedback</h3>
        <span className="hf-head-actions">
          <Button variant="text" onClick={reload} data-testid="helm-feedback-refresh">
            Refresh
          </Button>
          <Button variant="text" onClick={() => setFiling((v) => !v)} pressed={filing} data-testid="helm-feedback-new">
            New feedback
          </Button>
        </span>
      </div>
      <p className="usage-fine hf-intro">
        Things to tell the people who build Helm: bugs, rough edges, ideas. Agents add to this list too, and each entry shows who wrote it. It stays on this computer.
      </p>
      {filing && (
        <NewFeedback
          ops={ops}
          onFiled={(id) => {
            setFiling(false);
            setOpen(id);
            reload();
          }}
          onCancel={() => setFiling(false)}
        />
      )}
      <div className="hf-filters">
        <Segmented<FeedbackStatus | 'any'>
          label="Status"
          testId="helm-feedback-status"
          value={status}
          options={[{ value: 'any', label: 'Any' }, ...(Object.keys(STATUS_LABEL) as FeedbackStatus[]).map((s) => ({ value: s, label: STATUS_LABEL[s] }))]}
          onChange={setStatus}
        />
        <Segmented<FeedbackAuthorKind | 'any'>
          label="Written by"
          testId="helm-feedback-author-filter"
          value={author}
          options={[
            { value: 'any', label: 'Anyone' },
            { value: 'human', label: 'You' },
            { value: 'agent', label: 'Agent' },
            { value: 'agent_for_user', label: 'Agent, for you' },
          ]}
          onChange={setAuthor}
        />
        <TextInput className="hf-search" value={query} onChange={(e) => setQuery(e.target.value)} placeholder="Search feedback" aria-label="Search feedback" data-testid="helm-feedback-search" />
      </div>
      {error && (
        <p className="usage-notice" data-state="inactive" data-testid="helm-feedback-error">
          {error}
        </p>
      )}
      {items && items.length === 0 && (
        <p className="usage-notice" data-state="inactive" data-testid="helm-feedback-empty">
          {unfiltered ? 'No feedback yet. Use New feedback to write the first note.' : 'Nothing matches those filters.'}
        </p>
      )}
      <div className="fb-list">
        {(items ?? []).map((it) => (
          <div className="fb-item hf-item" key={it.id} data-testid="helm-feedback-item" data-open={open === it.id ? 'true' : undefined}>
            <button type="button" className="hf-row" aria-expanded={open === it.id} onClick={() => setOpen(open === it.id ? null : it.id)}>
              <span className="hf-title">{it.title}</span>
              <span className="hf-meta">
                <AuthorBadge author={it.author} />
                <span className="hf-status" data-status={it.status} data-testid="helm-feedback-state">
                  {STATUS_LABEL[it.status]}
                </span>
                <span>
                  {it.entryCount} entr{it.entryCount === 1 ? 'y' : 'ies'} · {when(it.updatedAt)}
                </span>
              </span>
              {open !== it.id && <span className="hf-snippet">{it.snippet}</span>}
            </button>
            {open === it.id && <Thread id={it.id} ops={ops} onChanged={reload} />}
          </div>
        ))}
      </div>
    </section>
  );
}

function NewFeedback({ ops, onFiled, onCancel }: { ops: Ops; onFiled: (id: number) => void; onCancel: () => void }) {
  const [title, setTitle] = useState('');
  const [text, setText] = useState('');
  const [busy, setBusy] = useState(false);
  const ready = title.trim().length > 0 && text.trim().length > 0;
  const send = () => {
    setBusy(true);
    void ops.create(title.trim(), text.trim()).then(
      (r) => {
        toast.show({ kind: 'info', text: 'Feedback saved.' });
        onFiled(r.id);
      },
      (e) => {
        setBusy(false);
        toast.show({ kind: 'error', text: errorText(e) });
      },
    );
  };
  return (
    <div className="hf-form" data-testid="helm-feedback-form">
      <TextInput value={title} maxLength={MAX_TITLE} onChange={(e) => setTitle(e.target.value)} placeholder="A short title" aria-label="Feedback title" autoFocus data-testid="helm-feedback-title" />
      <TextArea value={text} maxLength={MAX_TEXT} onChange={(e) => setText(e.target.value)} placeholder="What happened, or what would help?" aria-label="Feedback details" data-testid="helm-feedback-text" />
      <div className="hf-actions">
        <Button variant="text" onClick={onCancel}>
          Cancel
        </Button>
        <Button variant="primary" disabled={!ready} busy={busy} onClick={send} data-testid="helm-feedback-save">
          Save feedback
        </Button>
      </div>
    </div>
  );
}

function Thread({ id, ops, onChanged }: { id: number; ops: Ops; onChanged: () => void }) {
  const [thread, setThread] = useState<FeedbackThread | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [reply, setReply] = useState('');
  const [board, setBoard] = useState('');
  const [busy, setBusy] = useState(false);

  const load = useCallback(() => {
    void ops.item(id).then(
      (t) => {
        setThread(t);
        setError(null);
      },
      (e) => setError(errorText(e)),
    );
  }, [ops, id]);
  useEffect(() => load(), [load]);

  const run = (p: Promise<unknown>, done?: () => void) => {
    setBusy(true);
    void p
      .then(
        () => {
          done?.();
          load();
          onChanged();
        },
        (e) => toast.show({ kind: 'error', text: errorText(e) }),
      )
      .finally(() => setBusy(false));
  };

  if (error) {
    return (
      <p className="usage-notice" data-state="inactive">
        {error}
      </p>
    );
  }
  if (!thread) return <p className="usage-fine">Loading…</p>;
  return (
    <div className="hf-thread" data-testid="helm-feedback-thread">
      <ol className="hf-entries">
        {thread.entries.map((e) => (
          <li key={e.id} data-testid="helm-feedback-entry">
            <div className="hf-entry-head">
              <AuthorBadge author={e.author} />
              <span className="usage-fine">{when(e.at)}</span>
            </div>
            <div className="hf-entry-text">{e.text}</div>
          </li>
        ))}
      </ol>
      <div className="hf-form">
        <TextArea value={reply} maxLength={MAX_TEXT} onChange={(e) => setReply(e.target.value)} placeholder="Add to this feedback" aria-label="Add an entry" data-testid="helm-feedback-reply" />
        <div className="hf-actions">
          <Button variant="primary" disabled={!reply.trim() || busy} onClick={() => run(ops.append(id, reply.trim()), () => setReply(''))} data-testid="helm-feedback-append">
            Add
          </Button>
        </div>
      </div>
      <div className="hf-admin">
        <Segmented<FeedbackStatus>
          label="Status"
          testId="helm-feedback-set-status"
          value={thread.status}
          options={(Object.keys(STATUS_LABEL) as FeedbackStatus[]).map((s) => ({ value: s, label: STATUS_LABEL[s] }))}
          onChange={(s) => run(ops.update(id, { status: s }))}
        />
        <span className="hf-board">
          <TextInput value={board} onChange={(e) => setBoard(e.target.value)} placeholder={thread.boardItem ? `Board item ${thread.boardItem}` : 'Board item id'} aria-label="Board item id" data-testid="helm-feedback-board" />
          <Button variant="text" disabled={!board.trim() || busy} onClick={() => run(ops.update(id, { boardItem: board.trim() }), () => setBoard(''))}>
            {thread.boardItem ? 'Change' : 'Attach'}
          </Button>
        </span>
      </div>
    </div>
  );
}
