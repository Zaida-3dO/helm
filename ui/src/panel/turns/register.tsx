// The turn change card: under the last reply of a completed turn, the
// files the agent changed that turn with net counts ("Net diff unavailable" when the history could
// not be reconstructed). Up to 5 files, then "Show all"; grouped by repository when more than one.
// Clicking a file opens it in the side panel.

import { useEffect, useMemo, useState } from 'react';
import type { TurnChanges, TurnFile } from '../../api/panel.ts';
import { panelOps } from '../../api/panel.ts';
import { registerRowFooter } from '../../transcript/slots.ts';
import { FileDiffIcon } from '../icons.tsx';
import { openInPanel, panelStore } from '../registry.ts';
import { displayPath, pathKey } from '../viewer/paths.ts';
import { TurnLoader } from './loader.ts';

const SHOWN = 5;

let loader: TurnLoader | null = null;
function getLoader(): TurnLoader | null {
  if (!panelStore.api) return null;
  const ops = panelOps(panelStore.api);
  loader ??= new TurnLoader((chatId, ids) => ops.reviewTurns(chatId, ids));
  return loader;
}

function counts(f: TurnFile) {
  if (f.added === null || f.removed === null) return <span className="tc-unavailable">Net diff unavailable</span>;
  return (
    <span className="ch-counts">
      <span className="ch-add">+{f.added}</span> <span className="ch-del">−{f.removed}</span>
      {!f.completeCounts && <span className="tc-unavailable"> (partial)</span>}
    </span>
  );
}

function TurnCard({ chatId, messageId, revision }: { chatId: string; messageId: number; revision: number }) {
  const l = getLoader();
  const [data, setData] = useState<TurnChanges | null | undefined>(() => l?.peek(chatId, messageId, revision));
  const [all, setAll] = useState(false);
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
  const totals = data.files.reduce((a, f) => ({ added: a.added + (f.added ?? 0), removed: a.removed + (f.removed ?? 0) }), { added: 0, removed: 0 });
  const partial = data.truncated || data.files.some((f) => !f.completeCounts || f.added === null);
  let budget = all ? Infinity : SHOWN;
  return (
    <div className="tc-card" data-testid="turn-changes" data-files={data.files.length}>
      <div className="tc-head">
        <FileDiffIcon size={14} />
        <span>
          {data.files.length} file{data.files.length === 1 ? '' : 's'} changed
        </span>
        <span className="ch-counts">
          <span className="ch-add">+{totals.added}</span> <span className="ch-del">−{totals.removed}</span>
          {partial && <span className="tc-unavailable"> (partial)</span>}
        </span>
        {data.truncated && <span className="tc-unavailable">Some changes were not counted</span>}
      </div>
      {groups.map(([root, files]) => {
        const visible = files.slice(0, Math.max(0, budget));
        budget -= visible.length;
        if (visible.length === 0) return null;
        return (
          <div key={root}>
            {groups.length > 1 && <div className="tc-repo">{root ? root.split(/[\\/]/).pop() : 'Outside any repository'}</div>}
            {visible.map((f) => (
              <button
                key={f.path}
                type="button"
                className="tc-file"
                data-testid="turn-change-file"
                title={displayPath(f.path)}
                onClick={() => {
                  const p = displayPath(f.path);
                  if (!openInPanel('file', pathKey(p), { path: p }, { intent: 'user', chatId })) panelStore.openModal(p, { chatId });
                }}
              >
                <span className="tc-name">{f.repositoryPath}</span>
                {counts(f)}
              </button>
            ))}
          </div>
        );
      })}
      {data.files.length > SHOWN && (
        <button type="button" className="tc-more" onClick={() => setAll((a) => !a)}>
          {all ? 'Show fewer' : `Show all ${data.files.length} files`}
        </button>
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
