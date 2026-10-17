// The Inspector's Changes slot: Conflicts, Staged and
// Unstaged sections of the chat's working tree, each file a sticky disclosure row with counts,
// status letter, "partial" tag and its stage/unstage button, unfolding into a unified diff with two
// gutters and expandable gaps. Reads only while shown, on Refresh, and when the chat's turn ends.

import { useCallback, useEffect, useMemo, useRef, useState, useSyncExternalStore } from 'react';
import type { ChangeRecord, ChangeSection, ChangesView, DiffSide, DiffView, PanelOps } from '../../api/panel.ts';
import { panelOps } from '../../api/panel.ts';
import { useShell } from '../../shell/context.ts';
import { Button, TextArea } from '../../ui/index.ts';
import { focusComment, pendingComments, report, setComments } from '../context/store.ts';
import { FileDiffIcon } from '../icons.tsx';
import { openInPanel, panelStore } from '../registry.ts';
import { joinPath, pathKey } from '../viewer/paths.ts';
import { counts, EXPAND_STEP, fold, parseDiff } from './diffModel.ts';
import type { DiffRow, DisplayRow } from './diffModel.ts';

const SECTION_ORDER: ChangeSection[] = ['conflicts', 'staged', 'unstaged'];
const SECTION_TITLE: Record<ChangeSection, string> = { conflicts: 'Conflicts', staged: 'Staged', unstaged: 'Unstaged' };
const EMPTY_TEXT: Record<ChangeSection, string> = { conflicts: '', staged: 'Nothing staged', unstaged: 'No unstaged changes' };

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
  const focusNext = useRef<string | null>(null);
  const root = useRef<HTMLDivElement>(null);

  useEffect(() => {
    setView(null);
    setError(null);
    setOpen(new Set());
  }, [chatId]);

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

  return (
    <div className="ch-root" data-testid="changes-view" ref={root}>
      <div className="ch-subtitle">
        <span className="ch-branch">⎇ {view?.branch ?? (view ? 'detached' : '')}</span>
        <span className="ch-count">{view ? `${changed} changed` : ''}</span>
        <Button variant="icon" label="Refresh changes from disk" onClick={() => setTick((t) => t + 1)} disabled={loading}>
          ⟳
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
  onToggle: () => void;
  busy: string | null;
  onStage: () => void;
}

function FileRow({ ops, chatId, repoTop, record: r, open, onToggle, busy, onStage }: FileRowProps) {
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
          ›
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
        <span className={r.status === 'U' || r.status === 'A' ? 'ch-status new' : 'ch-status'}>{r.status}</span>
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
      {open && <DiffBody ops={ops} chatId={chatId} repoTop={repoTop} record={r} />}
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

function DiffBody({ ops, chatId, repoTop, record: r }: { ops: PanelOps; chatId: string; repoTop: string; record: ChangeRecord }) {
  const [diff, setDiff] = useState<DiffView | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [revealed, setRevealed] = useState<ReadonlyMap<number, number>>(new Map());
  const [sel, setSel] = useState<Sel | null>(null);
  const [draft, setDraft] = useState('');
  const [saving, setSaving] = useState(false);
  usePanelVersionHook();
  const diffSide: DiffSide = r.section === 'conflicts' ? 'head' : r.section;
  useEffect(() => {
    let cancelled = false;
    ops
      .diff(chatId, r.path, diffSide, r.section === 'staged' ? r.from : undefined)
      .then((d) => !cancelled && setDiff(d))
      .catch((e: unknown) => !cancelled && setError(e instanceof Error ? e.message : String(e)));
    return () => {
      cancelled = true;
    };
  }, [ops, chatId, r.path, r.section, r.from, diffSide]);
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

  if (error) return <div className="ch-hint">{error}</div>;
  if (!diff) return <div className="ch-hint">Loading diff…</div>;
  if (rows.length === 0) return <div className="ch-hint">No textual changes (binary, mode-only or empty).</div>;
  const total = counts(rows);
  const comments = (pendingComments.get(chatId) ?? []).filter((c) => c.path === r.path && c.diffSide === diffSide);

  const pick = (row: DiffRow, side: Side, extend: boolean) => {
    const n = lineOn(row, side);
    if (n === undefined) return;
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
    <div className="ch-diff" data-testid="diff-view" role="table" aria-label={`Diff of ${r.path}, ${total.added} added, ${total.removed} removed`}>
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
      <div className="ch-help">Click a line number to comment; Shift-click to extend.</div>
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
