// Registers the `file` tab kind, the file link route, the Files slot and the modal fallback.
// Imported once for its side effects by the panel's index.

import { useCallback, useEffect, useMemo, useState } from 'react';
import type { FileView } from '../../api/panel.ts';
import { panelOps } from '../../api/panel.ts';
import { driveForm, isRemoteOrDevicePath } from '../../markdown/paths.ts';
import { ExternalIcon, FileTextIcon, FolderOpenIcon } from '../icons.tsx';
import { registerLinkRoute } from '../links.ts';
import type { PanelTab } from '../model.ts';
import { openInPanel, panelStore, registerPanelKind, registerPanelSlot } from '../registry.ts';
import type { TabContext } from '../registry.ts';
import { FileViewer } from './FileViewer.tsx';
import type { Location } from './FileViewer.tsx';
import { useLayer } from '../../ui/index.ts';
import { report } from '../context/store.ts';
import { basename, displayPath, pathKey } from './paths.ts';

interface FileTabData extends Location {
  /** What the body shows now; it can navigate away from `path`. */
  current?: string;
  currentKind?: FileView['kind'];
}

function ops() {
  const api = panelStore.api;
  if (!api) throw new Error('The side panel is not connected to the host yet.');
  return panelOps(api);
}

/** Only a complete local path survives a restore. */
function validLocalPath(p: unknown): p is string {
  return typeof p === 'string' && p.length <= 8192 && driveForm(p) === 'absolute' && !isRemoteOrDevicePath(p);
}

function FileTabBody({ tab, ctx }: { tab: PanelTab; ctx: TabContext }) {
  const data = tab.data as FileTabData;
  const start = useMemo<Location>(
    () => ({ path: data.path, line: data.line, endLine: data.endLine, column: data.column }),
    [data.path, data.line, data.endLine, data.column],
  );
  const onLocation = useCallback(
    (loc: Location, view: FileView | null) => {
      const d = tab.data as FileTabData;
      const kind = view?.kind ?? d.currentKind;
      report(tab.id, kind === 'dir' ? { kind: 'folder', path: loc.path } : { kind: 'file', path: loc.path, ...(loc.line ? { line: loc.line } : {}) });
      if (d.current === loc.path && d.currentKind === kind) return;
      ctx.setData({ ...d, current: loc.path, currentKind: kind });
      ctx.setTitle(basename(loc.path));
    },
    // ctx changes every render; the tab identity is what matters.
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [tab],
  );
  const o = useMemo(() => ops(), []);
  const onSelection = useCallback(
    (sel: { startLine: number; endLine: number; text: string } | null) => {
      const d = tab.data as FileTabData;
      const path = d.current ?? d.path;
      report(tab.id, sel ? { kind: 'file', path, selection: sel } : { kind: 'file', path });
    },
    [tab],
  );
  return <FileViewer ops={o} start={start} chatId={tab.set === '' ? null : tab.set} visible={ctx.visible} onLocation={onLocation} onSelection={onSelection} />;
}

type AppState = { state: 'idle' } | { state: 'ready'; name: string } | { state: 'none' } | { state: 'refused'; reason: string };

/** Header action "Open in <App>": follows the path the body currently shows. */
function OpenInApp({ path, loaded }: { path: string; loaded: boolean }) {
  const [app, setApp] = useState<AppState>({ state: 'idle' });
  useEffect(() => {
    if (!loaded) {
      setApp({ state: 'idle' });
      return;
    }
    let cancelled = false;
    ops()
      .defaultHandler(path)
      .then((r) => {
        if (cancelled) return;
        if (r.handler) setApp({ state: 'ready', name: r.handler.name });
        else if (r.refusal?.step === 'noHandler') setApp({ state: 'none' });
        else setApp({ state: 'refused', reason: r.refusal?.reason ?? 'This file cannot be opened.' });
      })
      .catch((e: unknown) => !cancelled && setApp({ state: 'refused', reason: e instanceof Error ? e.message : String(e) }));
    return () => {
      cancelled = true;
    };
  }, [path, loaded]);
  const label =
    app.state === 'ready' ? `Open in ${app.name}` : app.state === 'none' ? 'No default app' : 'Open in default app';
  return (
    <button
      type="button"
      className="fv-button fv-open-app"
      data-testid="open-in-app"
      disabled={app.state !== 'ready'}
      title={app.state === 'refused' ? app.reason : label}
      onClick={() => void ops().openDefault(path).catch((e: unknown) => panelStore.showNotice(e instanceof Error ? e.message : String(e)))}
    >
      <ExternalIcon />
      <span>{label}</span>
    </button>
  );
}

registerPanelKind({
  kind: 'file',
  title: (tab) => basename((tab.data as FileTabData).current ?? (tab.data as FileTabData).path),
  icon: (tab) => ((tab.data as FileTabData).currentKind === 'dir' ? <FolderOpenIcon /> : <FileTextIcon />),
  render: (tab, ctx) => <FileTabBody tab={tab as PanelTab} ctx={ctx} />,
  headerActions: (tab) => {
    const d = tab.data as FileTabData;
    return <OpenInApp path={d.current ?? d.path} loaded={d.currentKind !== undefined} />;
  },
  persist: {
    version: 1,
    save: (tab) => {
      const d = tab.data as FileTabData;
      const out: Location = { path: d.path };
      if (d.line) out.line = d.line;
      return out;
    },
    restore: (data) => {
      const d = data as Partial<Location> | null;
      if (!d || !validLocalPath(d.path)) return null;
      const out: Location = { path: d.path };
      if (typeof d.line === 'number' && d.line >= 1) out.line = d.line;
      return out;
    },
  },
});

/** File links from replies: a `file` tab at the line, or the modal when the panel refuses. */
registerLinkRoute('file', (req, ctx) => {
  const path = displayPath(req.path);
  const data: FileTabData = { path };
  if (req.line) data.line = req.line;
  if (req.endLine) data.endLine = req.endLine;
  if (req.column) data.column = req.column;
  if (!openInPanel('file', pathKey(path), data, { intent: 'user', chatId: ctx.chatId })) {
    panelStore.openModal(path, { line: req.line, endLine: req.endLine, column: req.column, chatId: ctx.chatId });
  }
  return true;
});

/** Paths written in replies: the host resolves `~`, chat-relative and `/c/` forms, then they open
 *  like any file link. A missing path is explained by the host's message. */
registerLinkRoute('path', async (req, ctx) => {
  try {
    const r = await ops().pathResolve(req.path, ctx.chatId);
    const path = displayPath(r.path);
    const data: FileTabData = { path };
    if (r.kind === 'file' && req.line) data.line = req.line;
    if (r.kind === 'file' && req.endLine) data.endLine = req.endLine;
    if (r.kind === 'file' && req.column) data.column = req.column;
    if (!openInPanel('file', pathKey(path), data, { intent: 'user', chatId: ctx.chatId })) {
      panelStore.openModal(path, { line: data.line, endLine: data.endLine, column: data.column, chatId: ctx.chatId });
    }
    return true;
  } catch (e) {
    return e instanceof Error ? e.message : String(e);
  }
});

/** The Inspector's Files slot: the chat's project folder, one level at a time. */
function FilesSlot({ chatId, visible }: { chatId: string | null; visible: boolean }) {
  const [root, setRoot] = useState<string | null | undefined>(undefined);
  useEffect(() => {
    setRoot(undefined);
    if (!chatId || !visible) return;
    let cancelled = false;
    ops()
      .chatFolder(chatId)
      .then((r) => !cancelled && setRoot(r.path))
      .catch(() => !cancelled && setRoot(null));
    return () => {
      cancelled = true;
    };
  }, [chatId, visible]);
  const start = useMemo<Location | null>(() => (root ? { path: root } : null), [root]);
  const o = useMemo(() => ops(), []);
  if (!chatId) return <div className="sp-empty">Pick a chat to browse its project files.</div>;
  if (root === undefined) return <div className="sp-empty">Loading…</div>;
  if (!start) return <div className="sp-empty">This chat has no project folder to browse.</div>;
  return (
    <div className="sp-tab-body" data-testid="files-slot">
      <FileViewer ops={o} start={start} chatId={chatId} visible={visible} root={root ?? undefined} compact />
    </div>
  );
}

registerPanelSlot('files', { title: 'Files', render: ({ chatId, visible }) => <FilesSlot chatId={chatId} visible={visible} /> });

/** The modal viewer: the fallback when a file cannot open as a tab. */
export function ViewerModal() {
  const modal = panelStore.modal;
  const start = useMemo<Location | null>(
    () => (modal ? { path: modal.path, line: modal.line, endLine: modal.endLine, column: modal.column } : null),
    // A new nonce is a new open, even of the same path.
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [modal?.nonce],
  );
  useLayer('dialog', Boolean(modal), () => panelStore.closeModal());
  if (!modal || !start || !panelStore.api) return null;
  return (
    <div className="fv-modal-backdrop" onClick={(e) => e.target === e.currentTarget && panelStore.closeModal()}>
      <div className="fv-modal" role="dialog" aria-modal="true" aria-label={basename(modal.path)} data-testid="file-modal">
        <div className="fv-modal-head">
          <span className="fv-modal-title">{basename(modal.path)}</span>
          <button type="button" className="sp-icon-button" aria-label="Close" onClick={() => panelStore.closeModal()}>
            ×
          </button>
        </div>
        <FileViewer ops={panelOps(panelStore.api)} start={start} chatId={modal.chatId} visible />
      </div>
    </div>
  );
}
