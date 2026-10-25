// The Inspector's Changes slot: Conflicts, Staged and
// Unstaged sections of the chat's working tree, each file a sticky disclosure row with counts,
// status letter, "partial" tag and its stage/unstage button, unfolding into a unified diff with two
// gutters and expandable gaps. Reads only while shown, on Refresh, and when the chat's turn ends.

import { useCallback, useEffect, useMemo, useRef, useState, useSyncExternalStore } from 'react';
import type { ChangeRecord, ChangeSection, ChangesView, DiffSide, DiffView, PanelOps, TurnFile } from '../../api/panel.ts';
import { panelOps } from '../../api/panel.ts';
import { useShell } from '../../shell/context.ts';
import { Markdown } from '../../markdown/index.ts';
import { Button, TextArea } from '../../ui/index.ts';
import { focusComment, pendingComments, report, setComments } from '../context/store.ts';
import { BranchIcon, ChevronIcon, FileDiffIcon, ReloadIcon, WrapIcon } from '../icons.tsx';
import { openInPanel, panelStore } from '../registry.ts';
import { reviewTarget } from '../turns/review.ts';
import { joinPath, pathKey } from '../viewer/paths.ts';
import { counts, EXPAND_STEP, fold, parseDiff } from './diffModel.ts';
import type { DiffRow, DisplayRow } from './diffModel.ts';

const SECTION_ORDER: ChangeSection[] = ['conflicts', 'staged', 'unstaged'];
const SECTION_TITLE: Record<ChangeSection, string> = { conflicts: 'Conflicts', staged: 'Staged', unstaged: 'Unstaged' };
const EMPTY_TEXT: Record<ChangeSection, string> = { conflicts: '', staged: 'Nothing staged', unstaged: 'No unstaged changes' };

type StatusKind = 'modified' | 'added' | 'deleted' | 'renamed' | 'untracked' | 'conflict';
const STATUS_TITLE: Record<StatusKind, string> = {
  modified: 'Modified',
  added: 'Added',
  deleted: 'Deleted',
  renamed: 'Renamed',
  untracked: 'Untracked',
  conflict: 'Conflict',
};

/** The kind a status code stands for, so the letter can be tinted: D beats R beats A beats M. */
export function statusKind(status: string, section?: ChangeSection): StatusKind {
  if (section === 'conflicts') return 'conflict';
  if (status === 'U') return 'untracked';
  if (status.includes('D')) return 'deleted';
  if (status.includes('R')) return 'renamed';
  if (status.includes('A')) return 'added';
  if (status.includes('C')) return 'added';
  return 'modified';
}

const rowKey = (r: Pick<ChangeRecord, 'path' | 'section'>) => `${r.section}:${r.path}`;

function usePanelVersionHook() {
  return useSyncExternalStore(panelStore.subscribe, panelStore.getVersion);
}

function splitPath(p: string): { dir: string; base: string } {
  const at = p.lastIndexOf('/');
  return at < 0 ? { dir: '', base: p } : { dir: p.slice(0, at + 1), base: p.slice(at + 1) };
}

export function ChangesSlot({ chatId, visible }: { chatId: string | null; visible: boolean }) {
  const ops = useMemo(() => (panelStore.api ? panelOps(panelStore.api) : null), []);
  const shell = useShell();
  const [view, setView] = useState<ChangesView | null>(null);
  const [error, setError] = useState<{ code: string; message: string } | null>(null);
  const [loading, setLoading] = useState(false);
  const [tick, setTick] = useState(0);
  const [busy, setBusy] = useState<string | null>(null);
  const [announce, setAnnounce] = useState('');
  const [open, setOpen] = useState<ReadonlySet<string>>(new Set());
  const [wrap, setWrap] = useState(false);
  const [mode, setMode] = useState<'tree' | 'review'>('tree');
  const asked = useSyncExternalStore(reviewTarget.subscribe, reviewTarget.get);
  const review = asked && asked.chatId === chatId ? asked : null;
  const focusNext = useRef<string | null>(null);
  const root = useRef<HTMLDivElement>(null);

  useEffect(() => {
    setView(null);
    setError(null);
    setOpen(new Set());
  }, [chatId]);

  // Asking to review a turn (its card's Review button) switches to that turn's recorded edits.
  useEffect(() => {
    setMode(review ? 'review' : 'tree');
  }, [review?.nonce, review]);

  useEffect(() => {
    if (!visible || !chatId || !ops) return;
    let cancelled = false;
    setLoading(true);
    ops
      .changes(chatId)
      .then((v) => {
        if (cancelled) return;
        setView(v);
        setError(null);
      })
      .catch((e: { code?: string; message?: string }) => {
        if (cancelled) return;
        setView(null);
        setError({ code: e.code ?? 'error', message: e.message ?? String(e) });
      })
      .finally(() => !cancelled && setLoading(false));
    return () => {
      cancelled = true;
    };
  }, [visible, chatId, ops, tick]);

  // A turn ending is a moment the tree likely changed: re-read when the chat goes idle.
  useEffect(() => {
    if (!chatId) return;
    let last: string | undefined;
    return shell.bus.on('chat', (e) => {
      const chat = (e as { chat?: { id?: string; status?: string } }).chat;
      if (chat?.id !== chatId) return;
      if (last && last !== chat.status && chat.status === 'idle') setTick((t) => t + 1);
      last = chat.status;
    });
  }, [shell, chatId]);

  useEffect(() => {
    const want = focusNext.current;
    if (!want || !root.current) return;
    focusNext.current = null;
    const el = root.current.querySelector<HTMLElement>(`[data-stage-key="${CSS.escape(want)}"]`) ?? root.current.querySelector<HTMLElement>(`[data-section-head="${CSS.escape(want.split(':')[0] ?? '')}"]`);
    el?.focus();
  }, [view]);

  const runStage = useCallback(
    async (r: ChangeRecord) => {
      if (!ops || !chatId || busy) return;
      setBusy(rowKey(r));
      try {
        const res = r.section === 'staged' ? await ops.unstage(chatId, r.path, r.from) : await ops.stage(chatId, r.path);
        const dest: ChangeSection = r.section === 'staged' ? 'unstaged' : 'staged';
        focusNext.current = rowKey({ path: r.path, section: dest });
        setView(res.changes);
        setAnnounce(res.noop ? `Nothing changed for ${r.path}.` : `${r.section === 'staged' ? 'Unstaged' : 'Staged'} ${r.path}.`);
      } catch (e) {
        setAnnounce(e instanceof Error ? e.message : String(e));
        panelStore.showNotice(e instanceof Error ? e.message : String(e));
      } finally {
        setBusy(null);
      }
    },
    [ops, chatId, busy],
  );

  if (!chatId) return <Empty heading="No chat selected" text="Pick a chat to see its changes." />;
  if (error?.code === 'no_repository') return <Empty heading="No Git repository" text="You can still browse project files." />;

  const records = view?.records ?? [];
  const changed = new Set(records.map((r) => r.path)).size;

  if (review && mode === 'review' && ops) {
    return (
      <div className="ch-root" data-testid="changes-view" data-mode="review" ref={root}>
        <div className="ch-modes">
          <ModeSwitch mode={mode} onChange={setMode} />
          <WrapToggle wrap={wrap} onToggle={() => setWrap((w) => !w)} />
        </div>
        <TurnReview ops={ops} chatId={chatId} messageId={review.messageId} wrap={wrap} />
      </div>
    );
  }

  return (
    <div className="ch-root" data-testid="changes-view" ref={root}>
      {review && (
        <div className="ch-modes">
          <ModeSwitch mode={mode} onChange={setMode} />
        </div>
      )}
      <div className="ch-subtitle">
        <span className="ch-branch">
          <BranchIcon />
          <span>{view?.branch ?? (view ? 'detached' : '')}</span>
        </span>
        <span className="ch-count">{view ? `${changed} changed` : ''}</span>
        <WrapToggle wrap={wrap} onToggle={() => setWrap((w) => !w)} />
        <Button variant="icon" label="Refresh changes from disk" onClick={() => setTick((t) => t + 1)} disabled={loading}>
          <ReloadIcon />
        </Button>
      </div>
      <div className="ch-notice">Working tree · Staged: compared with HEAD · Unstaged: compared with the index</div>
      {error && (
        <div className="ch-error" role="alert">
          {error.message}
        </div>
      )}
      {view && records.length === 0 && <Empty heading="No changes yet" text="Changes will appear here as you work." />}
      {view && records.length > 0 && (
        <div className="ch-scroll">
          {SECTION_ORDER.map((section) => {
            const rows = records.filter((r) => r.section === section);
            if (section === 'conflicts' && rows.length === 0) return null;
            return (
              <section key={section} className="ch-section" data-testid="changes-section" data-section={section}>
                <h3 className="ch-heading" tabIndex={-1} data-section-head={section}>
                  {SECTION_TITLE[section].toUpperCase()} ({rows.length})
                </h3>
                {section === 'conflicts' && <div className="ch-hint">Resolve conflicts in your editor or terminal.</div>}
                {rows.length === 0 && <div className="ch-hint">{EMPTY_TEXT[section]}</div>}
                {rows.map((r) => (
                  <FileRow
                    key={rowKey(r)}
                    ops={ops!}
                    chatId={chatId}
                    repoTop={view.repoTop}
                    record={r}
                    open={open.has(rowKey(r))}
                    wrap={wrap}
                    onToggle={() =>
                      setOpen((s) => {
                        const n = new Set(s);
                        if (n.has(rowKey(r))) n.delete(rowKey(r));
                        else n.add(rowKey(r));
                        return n;
                      })
                    }
                    busy={busy}
                    onStage={() => void runStage(r)}
                  />
                ))}
              </section>
            );
          })}
          {view.truncated && <div className="ch-hint">Showing the first 2,000 changes.</div>}
        </div>
      )}
      <div className="ch-live" role="status" aria-live="polite">
        {announce}
      </div>
    </div>
  );
}

function Empty({ heading, text }: { heading: string; text: string }) {
  return (
    <div className="sp-empty" data-testid="changes-empty">
      <FileDiffIcon size={25} />
      <div className="sp-empty-heading">{heading}</div>
      <div>{text}</div>
    </div>
  );
}

interface FileRowProps {
  ops: PanelOps;
  chatId: string;
  repoTop: string;
  record: ChangeRecord;
  open: boolean;
  wrap: boolean;
  onToggle: () => void;
  busy: string | null;
  onStage: () => void;
}

function FileRow({ ops, chatId, repoTop, record: r, open, wrap, onToggle, busy, onStage }: FileRowProps) {
  const { dir, base } = splitPath(r.path);
  const deleted = r.status.includes('D');
  const key = rowKey(r);
  const label = r.section === 'staged' ? `Unstage ${r.path}` : `Stage ${r.path}`;
  return (
    <div className="ch-file" data-testid="change-row" data-path={r.path} data-section={r.section}>
      <div
        className={open ? 'ch-file-row open' : 'ch-file-row'}
        role="button"
        tabIndex={0}
        aria-expanded={open}
        aria-label={`${r.path}${r.partial ? `, also has ${r.section === 'staged' ? 'unstaged' : 'staged'} changes` : ''}`}
        onClick={onToggle}
        onKeyDown={(e) => {
          if (e.key === 'Enter' || e.key === ' ') {
            e.preventDefault();
            onToggle();
          }
        }}
      >
        <span className="ch-chevron" aria-hidden>
          <ChevronIcon size={13} />
        </span>
        <FileDiffIcon size={15} />
        <button
          type="button"
          className="ch-path"
          disabled={deleted}
          title={deleted ? 'Deleted files cannot be opened' : `Open ${r.path}`}
          onClick={(e) => {
            e.stopPropagation();
            const abs = joinPath(repoTop, r.path);
            if (!openInPanel('file', pathKey(abs), { path: abs }, { intent: 'user', chatId })) panelStore.openModal(abs, { chatId });
          }}
        >
          <span className="ch-dir">{dir}</span>
          <span className="ch-base">{base}</span>
        </button>
        {r.added !== undefined && (
          <span className="ch-counts">
            <span className="ch-add">+{r.added}</span> <span className="ch-del">−{r.removed}</span>
          </span>
        )}
        <span className={`ch-status ${statusKind(r.status, r.section)}`} title={STATUS_TITLE[statusKind(r.status, r.section)]}>
          {r.status}
        </span>
        {r.partial && <span className="ch-partial">partial</span>}
        {r.section !== 'conflicts' && (
          <Button
            variant="icon"
            className={busy === key ? 'ch-stage busy' : 'ch-stage'}
            label={label}
            data-stage-key={key}
            data-testid="stage-button"
            aria-disabled={busy !== null || undefined}
            aria-busy={busy === key || undefined}
            onClick={(e) => {
              e.stopPropagation();
              if (busy === null) onStage();
            }}
          >
            {r.section === 'staged' ? '−' : '+'}
          </Button>
        )}
      </div>
      {open && <DiffBody ops={ops} chatId={chatId} repoTop={repoTop} record={r} wrap={wrap} />}
    </div>
  );
}

function ModeSwitch({ mode, onChange }: { mode: 'tree' | 'review'; onChange: (m: 'tree' | 'review') => void }) {
  return (
    <div className="ch-view-switch" role="group" aria-label="Changes view">
      <button type="button" aria-pressed={mode === 'tree'} onClick={() => onChange('tree')} data-testid="mode-working-tree">
        Working tree
      </button>
      <button type="button" aria-pressed={mode === 'review'} onClick={() => onChange('review')} data-testid="mode-review">
        Review
      </button>
    </div>
  );
}

function WrapToggle({ wrap, onToggle }: { wrap: boolean; onToggle: () => void }) {
  return (
    <Button variant="icon" label="Word wrap" pressed={wrap} onClick={onToggle}>
      <WrapIcon />
    </Button>
  );
}

/** One finished turn's recorded edits: each file the agent changed, with the diff of what it did. */
function TurnReview({ ops, chatId, messageId, wrap }: { ops: PanelOps; chatId: string; messageId: number; wrap: boolean }) {
  const [files, setFiles] = useState<TurnFile[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [open, setOpen] = useState<ReadonlySet<string>>(new Set());
  useEffect(() => {
    let cancelled = false;
    setFiles(null);
    setError(null);
    setOpen(new Set());
    ops
      .reviewTurns(chatId, [messageId])
      .then((r) => {
        if (cancelled) return;
        const list = r.find((t) => t.messageId === messageId)?.files ?? [];
        setFiles(list);
        // A turn that changed one file opens it straight away.
        if (list.length === 1) setOpen(new Set([(list[0] as TurnFile).path]));
      })
      .catch((e: unknown) => !cancelled && setError(e instanceof Error ? e.message : String(e)));
    return () => {
      cancelled = true;
    };
  }, [ops, chatId, messageId]);
  if (error) return <div className="ch-error" role="alert">{error}</div>;
  if (!files) return <div className="ch-hint">Loading…</div>;
  if (files.length === 0) return <Empty heading="Nothing recorded" text="That turn's edits are no longer available." />;
  return (
    <div className="ch-scroll" data-testid="turn-review">
      <div className="ch-notice">Net change this turn: what the agent edited, not the current state of the files</div>
      {files.map((f) => {
        const isOpen = open.has(f.path);
        const { dir, base } = splitPath(f.repositoryPath);
        const toggle = () =>
          setOpen((s) => {
            const n = new Set(s);
            if (n.has(f.path)) n.delete(f.path);
            else n.add(f.path);
            return n;
          });
        return (
          <div key={f.path} className="ch-file" data-testid="turn-review-row" data-path={f.repositoryPath}>
            <div
              className={isOpen ? 'ch-file-row open' : 'ch-file-row'}
              role="button"
              tabIndex={0}
              aria-expanded={isOpen}
              aria-label={f.repositoryPath}
              onClick={toggle}
              onKeyDown={(e) => {
                if (e.key === 'Enter' || e.key === ' ') {
                  e.preventDefault();
                  toggle();
                }
              }}
            >
              <span className="ch-chevron" aria-hidden>
                <ChevronIcon size={13} />
              </span>
              <FileDiffIcon size={15} />
              <span className="ch-path">
                <span className="ch-dir">{dir}</span>
                <span className="ch-base">{base}</span>
              </span>
              {f.added !== null && f.removed !== null ? (
                <span className="ch-counts">
                  <span className="ch-add">+{f.added}</span> <span className="ch-del">−{f.removed}</span>
                </span>
              ) : (
                <span className="ch-partial">counts unavailable</span>
              )}
            </div>
            {isOpen && (
              <DiffBody
                ops={ops}
                chatId={chatId}
                repoTop={f.repositoryRoot ?? ''}
                record={{ path: f.repositoryPath, status: 'M', section: 'unstaged', partial: false }}
                wrap={wrap}
                turn={{ messageId, path: f.path }}
              />
            )}
          </div>
        );
      })}
    </div>
  );
}

type Side = 'left' | 'right';
interface Sel {
  side: Side;
  start: number;
  end: number;
}

/** SHA-256 of the patch, first 16 bytes as hex: the host's own fingerprint of the same text. */
async function fingerprintOf(patch: string): Promise<string> {
  const digest = await crypto.subtle.digest('SHA-256', new TextEncoder().encode(patch));
  return [...new Uint8Array(digest).slice(0, 16)].map((b) => b.toString(16).padStart(2, '0')).join('');
}

const lineOn = (row: DiffRow, side: Side) => (side === 'left' ? row.oldNo : row.newNo);

interface DiffBodyProps {
  ops: PanelOps;
  chatId: string;
  repoTop: string;
  record: ChangeRecord;
  wrap: boolean;
  /** A finished turn's recorded edits instead of the working tree: read-only, no line comments. */
  turn?: { messageId: number; path: string };
}

const isMarkdownPath = (p: string) => /\.(md|markdown)$/i.test(p);

function formatTime(t: number): string {
  const ms = t < 1e12 ? t * 1000 : t;
  return new Date(ms).toLocaleTimeString([], { hour: 'numeric', minute: '2-digit' });
}

/** The file as it is now, rendered: the other half of a Markdown file's Rendered / Diff switch. */
function RenderedDoc({ ops, chatId, path }: { ops: PanelOps; chatId: string; path: string }) {
  const [text, setText] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  useEffect(() => {
    let cancelled = false;
    ops
      .fileRead(path, chatId)
      .then((v) => !cancelled && setText(v.text ?? ''))
      .catch((e: unknown) => !cancelled && setError(e instanceof Error ? e.message : String(e)));
    return () => {
      cancelled = true;
    };
  }, [ops, chatId, path]);
  if (error) return <div className="ch-hint">{error}</div>;
  if (text === null) return <div className="ch-hint">Loading…</div>;
  return (
    <div className="ch-rendered fv-markdown" data-testid="diff-rendered">
      <Markdown text={text} />
    </div>
  );
}

function DiffBody({ ops, chatId, repoTop, record: r, wrap, turn }: DiffBodyProps) {
  const [diff, setDiff] = useState<DiffView | null>(null);
  const [recorded, setRecorded] = useState<{ complete: boolean; time: number | null } | null>(null);
  const [shownAs, setShownAs] = useState<'diff' | 'rendered'>('diff');
  const [error, setError] = useState<string | null>(null);
  const [revealed, setRevealed] = useState<ReadonlyMap<number, number>>(new Map());
  const [sel, setSel] = useState<Sel | null>(null);
  const [draft, setDraft] = useState('');
  const [saving, setSaving] = useState(false);
  usePanelVersionHook();
  const diffSide: DiffSide = r.section === 'conflicts' ? 'head' : r.section;
  useEffect(() => {
    let cancelled = false;
    const load: Promise<DiffView> = turn
      ? ops.turnDiff(chatId, turn.messageId, turn.path).then((d) => {
          if (!cancelled) setRecorded({ complete: d.complete, time: d.time });
          return { patch: d.patch, fullContext: false, truncated: false };
        })
      : ops.diff(chatId, r.path, diffSide, r.section === 'staged' ? r.from : undefined);
    load.then((d) => !cancelled && setDiff(d)).catch((e: unknown) => !cancelled && setError(e instanceof Error ? e.message : String(e)));
    return () => {
      cancelled = true;
    };
  }, [ops, chatId, r.path, r.section, r.from, diffSide, turn?.messageId, turn?.path]);
  const rows = useMemo(() => (diff ? parseDiff(diff.patch) : []), [diff]);
  const shown = useMemo<DisplayRow[]>(() => fold(rows, revealed), [rows, revealed]);

  // The selected lines are the side panel context item while they are selected.
  useEffect(() => {
    if (!sel) {
      report(`changes:${chatId}`, null);
      return;
    }
    const picked = rows.filter((x) => {
      const n = lineOn(x, sel.side);
      return n !== undefined && n >= sel.start && n <= sel.end && (x.kind === 'ctx' || x.kind === (sel.side === 'left' ? 'del' : 'add'));
    });
    report(`changes:${chatId}`, {
      kind: 'diff',
      repoTop,
      path: r.path,
      side: diffSide,
      startLine: sel.start,
      endLine: sel.end,
      rows: picked.slice(0, 121).map((x) => ({ sign: x.kind === 'add' ? '+' : x.kind === 'del' ? '-' : ' ', text: x.text })),
    });
  }, [sel, rows, chatId, repoTop, r.path, diffSide]);

  const absPath = turn ? turn.path : joinPath(repoTop, r.path);
  const canRender = isMarkdownPath(r.path) && !r.status.includes('D');
  const switcher = canRender ? (
    <div className="ch-view-switch" role="group" aria-label="Show as">
      <button type="button" aria-pressed={shownAs === 'rendered'} onClick={() => setShownAs('rendered')}>
        Rendered
      </button>
      <button type="button" aria-pressed={shownAs === 'diff'} onClick={() => setShownAs('diff')}>
        Diff
      </button>
    </div>
  ) : null;
  const stamp =
    turn && recorded ? (
      <div className="ch-turn-stamp">
        <span>Net change this turn</span>
        {recorded.time !== null && <span>{formatTime(recorded.time)}</span>}
      </div>
    ) : null;
  if (error) return <div className="ch-hint">{error}</div>;
  if (!diff) return <div className="ch-hint">Loading diff…</div>;
  if (canRender && shownAs === 'rendered') {
    return (
      <>
        {stamp}
        {switcher}
        <RenderedDoc ops={ops} chatId={chatId} path={absPath} />
      </>
    );
  }
  if (rows.length === 0) {
    return (
      <>
        {stamp}
        {switcher}
        <div className="ch-hint">
          {turn && recorded && !recorded.complete
            ? 'The record of this file’s earlier contents is incomplete, so no diff can be shown.'
            : 'No textual changes (binary, mode-only or empty).'}
        </div>
      </>
    );
  }
  const total = counts(rows);
  const comments = (pendingComments.get(chatId) ?? []).filter((c) => c.path === r.path && c.diffSide === diffSide);

  const pick = (row: DiffRow, side: Side, extend: boolean) => {
    const n = lineOn(row, side);
    if (n === undefined || turn) return;
    if (extend && sel && sel.side === side) {
      const start = Math.min(sel.start, n);
      const end = Math.max(sel.end, n);
      if (end - start + 1 > 120) return;
      setSel({ side, start, end });
    } else {
      setSel({ side, start: n, end: n });
      setDraft('');
    }
  };
  const inSel = (row: DiffRow) => {
    if (!sel) return false;
    const n = lineOn(row, sel.side);
    return n !== undefined && n >= sel.start && n <= sel.end && row.kind !== (sel.side === 'left' ? 'add' : 'del');
  };
  const isEnd = (row: DiffRow) => Boolean(sel) && lineOn(row, sel!.side) === sel!.end && inSel(row);
  const save = async () => {
    if (!sel || !draft.trim() || !diff) return;
    setSaving(true);
    try {
      const id = globalThis.crypto?.randomUUID?.() ?? `c-${Date.now()}`;
      await ops.commentAdd({
        chatId,
        id,
        path: r.path,
        diffSide,
        side: sel.side,
        startLine: sel.start,
        endLine: sel.end,
        body: draft.trim(),
        fingerprint: await fingerprintOf(diff.patch),
      });
      setComments(chatId, await ops.commentList(chatId));
      setSel(null);
      setDraft('');
    } catch (e) {
      panelStore.showNotice(e instanceof Error ? e.message : String(e));
    } finally {
      setSaving(false);
    }
  };

  return (
    <div
      className={wrap ? 'ch-diff wrap' : 'ch-diff'}
      data-testid="diff-view"
      role="table"
      aria-label={`Diff of ${r.path}, ${total.added} added, ${total.removed} removed`}
    >
      {stamp}
      {switcher}
      {recorded && !recorded.complete && <div className="ch-hint">Some edits could not be verified, so this diff may be missing changes.</div>}
      {diff.truncated && <div className="ch-hint">Excerpt of this diff</div>}
      {shown.map((row, i) =>
        row.kind === 'gap' ? (
          <div key={`g${row.id}`} className="ch-gap" data-testid="diff-gap">
            <span>{row.rows.length} unchanged lines</span>
            <button type="button" onClick={() => setRevealed((m) => new Map(m).set(row.id, (m.get(row.id) ?? 0) + EXPAND_STEP))}>
              Expand {Math.min(EXPAND_STEP, row.rows.length)} lines
            </button>
            {row.rows.length > EXPAND_STEP && (
              <button type="button" onClick={() => setRevealed((m) => new Map(m).set(row.id, Number.MAX_SAFE_INTEGER))}>
                Expand all
              </button>
            )}
          </div>
        ) : row.kind === 'hunk' && diff.fullContext ? null : (
          <div key={i}>
            <Line row={row} selected={inSel(row)} onPick={pick} />
            {isEnd(row) && (
              <div className="ch-editor" data-testid="comment-editor">
                <div className="ch-editor-head">
                  <span>Comment</span>
                  <span className="ch-editor-range">
                    {sel!.side === 'left' ? 'Left' : 'Right'} lines {sel!.start}–{sel!.end}
                  </span>
                </div>
                <TextArea
                  autoFocus
                  value={draft}
                  maxLength={4000}
                  placeholder="What should change?"
                  aria-label="Comment"
                  onChange={(e) => setDraft(e.target.value)}
                  onKeyDown={(e) => {
                    if ((e.ctrlKey || e.metaKey) && e.key === 'Enter') {
                      e.preventDefault();
                      void save();
                    }
                  }}
                />
                <div className="ch-editor-foot">
                  <Button variant="text" onClick={() => setSel(null)}>
                    Cancel
                  </Button>
                  <Button variant="primary" disabled={saving || !draft.trim()} onClick={() => void save()} data-testid="comment-save">
                    {saving ? 'Saving…' : 'Comment'}
                  </Button>
                </div>
              </div>
            )}
            {row.kind !== 'hunk' &&
              row.kind !== 'meta' &&
              comments
                .filter((c) => lineOn(row, c.side) === c.endLine && row.kind !== (c.side === 'left' ? 'add' : 'del'))
                .map((c) => (
                  <div
                    key={c.id}
                    className="ch-comment"
                    data-testid="review-comment"
                    role="button"
                    tabIndex={0}
                    onClick={() => focusComment(chatId, c.id)}
                    onKeyDown={(e) => e.key === 'Enter' && focusComment(chatId, c.id)}
                  >
                    <div className="ch-editor-head">
                      <span>
                        You · {c.side === 'left' ? 'L' : 'R'} {c.startLine}–{c.endLine}
                      </span>
                      <Button
                        variant="icon"
                        label="Remove comment"
                        onClick={(e) => {
                          e.stopPropagation();
                          void ops
                            .commentRemove(chatId, c.id)
                            .then((l) => setComments(chatId, l))
                            .catch((err: unknown) => panelStore.showNotice(err instanceof Error ? err.message : String(err)));
                        }}
                      >
                        ×
                      </Button>
                    </div>
                    <div className="ch-comment-body">{c.body}</div>
                  </div>
                ))}
          </div>
        ),
      )}
      {revealed.size > 0 && (
        <div className="ch-gap">
          <button type="button" onClick={() => setRevealed(new Map())}>
            Collapse
          </button>
        </div>
      )}
      {!turn && <div className="ch-help">Click a line number to comment; Shift-click to extend.</div>}
    </div>
  );
}

function Line({ row, selected, onPick }: { row: DiffRow; selected: boolean; onPick: (row: DiffRow, side: Side, extend: boolean) => void }) {
  const sign = row.kind === 'add' ? '+' : row.kind === 'del' ? '−' : ' ';
  const gutter = (side: Side, n: number | undefined) => (
    <span
      className={n !== undefined ? 'ch-gutter pickable' : 'ch-gutter'}
      role="cell"
      tabIndex={n !== undefined ? 0 : undefined}
      aria-label={n !== undefined ? `${side === 'left' ? 'Old' : 'New'} line ${n}` : undefined}
      onClick={(e) => n !== undefined && onPick(row, side, e.shiftKey)}
      onKeyDown={(e) => n !== undefined && (e.key === 'Enter' || e.key === ' ') && (e.preventDefault(), onPick(row, side, e.shiftKey))}
    >
      {n ?? ''}
    </span>
  );
  return (
    <div className={`ch-line ${row.kind}${selected ? ' selected' : ''}`} role="row" data-kind={row.kind}>
      {gutter('left', row.oldNo)}
      {gutter('right', row.newNo)}
      <span className="ch-sign" role="cell">
        {row.kind === 'hunk' || row.kind === 'meta' ? '' : sign}
      </span>
      <span className="ch-code" role="cell">
        {row.text}
      </span>
    </div>
  );
}
