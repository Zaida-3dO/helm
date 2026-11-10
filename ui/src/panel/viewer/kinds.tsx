// Registers the `file` tab kind, the file link route, the Files slot and the modal fallback.
// Imported once for its side effects by the panel's index.

import { useCallback, useEffect, useMemo, useState } from 'react';
import type { FileView } from '../../api/panel.ts';
import { panelOps } from '../../api/panel.ts';
import { ExternalIcon, FileTextIcon, FolderOpenIcon } from '../icons.tsx';
import { IconPlay } from '../../ui/icons.tsx';
import { forgetBadge } from '../badges.ts';
import { registerLinkRoute } from '../links.ts';
import type { PanelTab } from '../model.ts';
import { openInPanel, panelStore, registerPanelKind, registerPanelSlot } from '../registry.ts';
import type { TabContext } from '../registry.ts';
import { FileViewer } from './FileViewer.tsx';
import type { Location } from './FileViewer.tsx';
import { FilesTree } from './FilesTree.tsx';
import { useLayer } from '../../ui/index.ts';
import { report } from '../context/store.ts';
import { basename, displayPath, joinPath, pathKey } from './paths.ts';
import { openPageTab } from '../../pages/open.ts';
import '../../pages/register.tsx';
import { chatRoot, requestReveal, revealRefusal, useChatRoot } from './reveal.ts';
import { restoreFileTab, saveFileTab } from './tabdata.ts';
import type { ViewMode } from './tabdata.ts';

interface FileTabData extends Location {
  /** What the body shows now; it can navigate away from `path`. */
  current?: string;
  currentKind?: FileView['kind'];
  /** Preview or Source as the person last chose it, and the first visible line of the source. */
  mode?: ViewMode;
  top?: number;
  /** Opened from the Files tree, so the viewer offers a way back to it. */
  fromFiles?: boolean;
}

function ops() {
  const api = panelStore.api;
  if (!api) throw new Error('The side panel is not connected to the host yet.');
  return panelOps(api);
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
  const chatId = tab.set === '' ? null : tab.set;
  const root = useChatRoot(o, chatId);
  const shownPath = data.current ?? data.path;
  const filesLink = useMemo(
    () =>
      chatId
        ? {
            refusal: revealRefusal(root, shownPath),
            fromFiles: data.fromFiles === true,
            show: (path: string) => requestReveal(chatId, path),
          }
        : undefined,
    [chatId, root, shownPath, data.fromFiles],
  );
  const onMode = useCallback(
    (mode: ViewMode) => ctx.setData({ ...(tab.data as FileTabData), mode }),
    // ctx changes every render; the tab identity is what matters.
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [tab],
  );
  // The reading position changes as the person scrolls: kept on the tab without redrawing the panel.
  const onTop = useCallback(
    (line: number) => {
      (tab.data as FileTabData).top = line;
    },
    [tab],
  );
  const onSelection = useCallback(
    (sel: { startLine: number; endLine: number; text: string } | null) => {
      const d = tab.data as FileTabData;
      const path = d.current ?? d.path;
      report(tab.id, sel ? { kind: 'file', path, selection: sel } : { kind: 'file', path });
    },
    [tab],
  );
  return (
    <FileViewer
      ops={o}
      start={start}
      chatId={chatId}
      visible={ctx.visible}
      live={ctx.focused}
      initialMode={data.mode}
      initialTop={data.top}
      onMode={onMode}
      onTop={onTop}
      filesLink={filesLink}
      onLocation={onLocation}
      onSelection={onSelection}
    />
  );
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

/** Header action "Run page": an HTML file previews inertly; this opens it as a sealed page that runs its scripts. */
function RunPage({ path }: { path: string }) {
  return (
    <button
      type="button"
      className="fv-button"
      data-testid="run-page"
      title="Run this page with its scripts in a sealed frame"
      onClick={() => {
        if (!openPageTab(path)) panelStore.showNotice('The side panel has no room for another tab.');
      }}
    >
      <IconPlay size={13} />
      <span>Run page</span>
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
    return (
      <>
        {d.currentKind === 'html' && <RunPage path={d.current ?? d.path} />}
        <OpenInApp path={d.current ?? d.path} loaded={d.currentKind !== undefined} />
      </>
    );
  },
  onClose: (tab) => forgetBadge(tab.id),
  persist: {
    version: 1,
    save: (tab) => saveFileTab(tab.data as FileTabData),
    restore: (data) => restoreFileTab(data),
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

/** Shows `rel`, a path inside the current chat's project folder, in the Files tab. False when the path is not a plain relative one. */
export function revealInFiles(rel: string): boolean {
  const chatId = panelStore.current;
  const parts = rel.split(/[\\/]/);
  if (!chatId || parts.some((s) => s === '' || s === '.' || s === '..')) return false;
  void chatRoot(ops(), chatId).then((root) => {
    if (root) requestReveal(chatId, joinPath(root, parts.join('/')));
    else panelStore.showNotice('This chat has no project folder to browse.');
  });
  return true;
}

/** The Inspector's Files slot: the chat's project folder as a tree. */
function FilesSlot({ chatId, visible }: { chatId: string | null; visible: boolean }) {
  const o = useMemo(() => ops(), []);
  const root = useChatRoot(o, chatId);
  if (!chatId) return <div className="sp-empty">Pick a chat to browse its project files.</div>;
  if (root === undefined) return <div className="sp-empty">Loading…</div>;
  if (!root) return <div className="sp-empty">This chat has no project folder to browse.</div>;
  return (
    <div className="sp-tab-body" data-testid="files-slot">
      <FilesTree ops={o} chatId={chatId} root={root} visible={visible} />
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
        <FileViewer ops={panelOps(panelStore.api)} start={start} chatId={modal.chatId} visible live />
      </div>
    </div>
  );
}
