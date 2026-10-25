// The "Edited N files" card: under the last reply of a completed turn, the
// files the agent changed that turn with net counts ("Net diff unavailable" when the history could
// not be reconstructed). Five files, then "Show N more files"; grouped by repository when more than
// one. Clicking a file opens it in the side panel; Review opens that turn's recorded edits in the Changes view; the minimise
// button collapses every change card in the chat.

import { useEffect, useMemo, useState, useSyncExternalStore } from 'react';
import type { TurnChanges, TurnFile } from '../../api/panel.ts';
import { panelOps } from '../../api/panel.ts';
import { splitPath } from '../../transcript/toolKinds.ts';
import { registerRowFooter } from '../../transcript/slots.ts';
import { IconChevronDown, IconFileDiff, IconMaximize, IconMinimize } from '../../ui/icons.tsx';
import { openInPanel, panelStore } from '../registry.ts';
import { displayPath, pathKey } from '../viewer/paths.ts';
import { TurnLoader } from './loader.ts';
import { openTurnReview } from './review.ts';
import './turns.css';

const SHOWN = 5;

let loader: TurnLoader | null = null;
function getLoader(): TurnLoader | null {
  if (!panelStore.api) return null;
  const ops = panelOps(panelStore.api);
  loader ??= new TurnLoader((chatId, ids) => ops.reviewTurns(chatId, ids));
  return loader;
}

// Chats whose change cards the reader minimised, for this session.
const minimised = new Set<string>();
const listeners = new Set<() => void>();
function toggleMinimised(chatId: string) {
  if (!minimised.delete(chatId)) minimised.add(chatId);
  listeners.forEach((l) => l());
}
function useMinimised(chatId: string): boolean {
  return useSyncExternalStore(
    (l) => {
      listeners.add(l);
      return () => listeners.delete(l);
    },
    () => minimised.has(chatId),
  );
}

function Counts({ added, removed, partial }: { added: number | null; removed: number | null; partial?: boolean }) {
  if (added === null || removed === null) return <span className="ec-unavailable">Net diff unavailable</span>;
  return (
    <span className="ec-counts">
      <span className="ec-add">+{added}</span> <span className="ec-del">−{removed}</span>
      {partial && <span className="ec-unavailable"> partial</span>}
    </span>
  );
}

function FileRow({ f, chatId }: { f: TurnFile; chatId: string }) {
  const { name, dir } = splitPath(f.repositoryPath);
  return (
    <button
      type="button"
      className="ec-file"
      data-testid="turn-change-file"
      title={displayPath(f.path)}
      onClick={() => {
        const p = displayPath(f.path);
        if (!openInPanel('file', pathKey(p), { path: p }, { intent: 'user', chatId })) panelStore.openModal(p, { chatId });
      }}
    >
      <span className="ec-path">
        <span className="ec-name">{name}</span>
        {dir && <span className="ec-dir">{dir}</span>}
      </span>
      <Counts added={f.added} removed={f.removed} partial={f.added !== null && !f.completeCounts} />
    </button>
  );
}

function TurnCard({ chatId, messageId, revision }: { chatId: string; messageId: number; revision: number }) {
  const l = getLoader();
  const [data, setData] = useState<TurnChanges | null | undefined>(() => l?.peek(chatId, messageId, revision));
  const [all, setAll] = useState(false);
  const collapsed = useMinimised(chatId);
  useEffect(() => {
    if (!l) return;
    let live = true;
    void l.load(chatId, messageId, revision).then(() => live && setData(l.peek(chatId, messageId, revision)));
    return () => {
      live = false;
    };
  }, [l, chatId, messageId, revision]);
  const groups = useMemo(() => {
    const m = new Map<string, TurnFile[]>();
    for (const f of data?.files ?? []) {
      const k = f.repositoryRoot ?? '';
      m.set(k, [...(m.get(k) ?? []), f]);
    }
    return [...m];
  }, [data]);
  if (!data || data.files.length === 0) return null;
  const n = data.files.length;
  const totals = data.files.reduce((a, f) => ({ added: a.added + (f.added ?? 0), removed: a.removed + (f.removed ?? 0) }), { added: 0, removed: 0 });
  const unknown = data.files.every((f) => f.added === null);
  const partial = data.truncated || data.files.some((f) => !f.completeCounts || f.added === null);
  let budget = all ? Infinity : SHOWN;
  return (
    <div className={`ec-card${collapsed ? ' is-collapsed' : ''}`} data-testid="turn-changes" data-files={n}>
      <div className="ec-head">
        <span className="ec-tile" aria-hidden="true">
          <IconFileDiff size={14} />
        </span>
        <span className="ec-title">
          Edited {n} file{n === 1 ? '' : 's'}
        </span>
        {unknown ? <span className="ec-unavailable">—</span> : <Counts added={totals.added} removed={totals.removed} partial={partial} />}
        <span className="ec-spacer" />
        <button type="button" className="ec-review" data-testid="turn-changes-review" onClick={() => openTurnReview(chatId, messageId)}>
          Review
        </button>
        <button
          type="button"
          className="ec-min"
          aria-pressed={collapsed}
          title={collapsed ? 'Show change cards' : 'Minimise change cards in this chat'}
          aria-label={collapsed ? 'Show change cards' : 'Minimise change cards in this chat'}
          onClick={() => toggleMinimised(chatId)}
        >
          {collapsed ? <IconMaximize size={13} /> : <IconMinimize size={13} />}
        </button>
      </div>
      {!collapsed && (
        <>
          <div className="ec-files">
            {groups.map(([root, files]) => {
              const visible = files.slice(0, Math.max(0, budget));
              budget -= visible.length;
              if (visible.length === 0) return null;
              return (
                <div key={root}>
                  {groups.length > 1 && <div className="ec-repo">{root ? root.split(/[\\/]/).pop() : 'Outside any repository'}</div>}
                  {visible.map((f) => (
                    <FileRow key={f.path} f={f} chatId={chatId} />
                  ))}
                </div>
              );
            })}
          </div>
          {(n > SHOWN || data.truncated) && (
            <div className="ec-foot">
              {n > SHOWN && (
                <button type="button" className={`ec-more${all ? ' is-open' : ''}`} aria-expanded={all} onClick={() => setAll((a) => !a)}>
                  {all ? 'Show fewer files' : `Show ${n - SHOWN} more file${n - SHOWN === 1 ? '' : 's'}`}
                  <IconChevronDown size={11} className="ec-more-chevron" />
                </button>
              )}
              {data.truncated && <span className="ec-unavailable">Partial list</span>}
            </div>
          )}
        </>
      )}
    </div>
  );
}

registerRowFooter({
  id: 'turn-changes',
  // Every settled assistant reply asks; the host answers only for a completed turn's last reply.
  appliesTo: (row, ctx) => row.role === 'assistant' && row.status === 'complete' && !(ctx.running && ctx.isLatestAssistant),
  render: (row, ctx) => <TurnCard chatId={ctx.chatId} messageId={row.id} revision={row.revision} />,
});
