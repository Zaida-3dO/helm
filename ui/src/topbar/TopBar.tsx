// The top bar: one row. Left: the chat title (double-click to rename), then the project pill
// (the board the chat is shown under, with its folder moves). Right, in order: the usage meter,
// [update-pill slot] [Swarm slot], the action row, Toggle inspector, [Chat view slot], and at the
// far right the Windows caption buttons (Helm draws its own caption).
//
// What left the bar: the folder breadcrumb and the Project/Folder tags strip (the project pill
// carries both), Fork/Pin/Archive (the composer's / menu and the sidebar's chat row menu), the
// graphics-card control (Settings → Local models, plus the app_gpu tool) and the working-mode chip
// (Settings → Chat & agents). The Fork and rename keyboard shortcuts stay.
//
// The bar is a window drag region: a left press that is not on a control starts a native drag, and
// a double-click toggles maximise.

import { useLayoutEffect, useRef, useState, useSyncExternalStore, type ReactNode } from 'react';
import { panelStore, togglePanel } from '../panel/index.ts';
import { useSelection, useShell } from '../shell/context.ts';
import { Button, layers, TextInput, toast, useBinding, useHint } from '../ui/index.ts';
import { IconMute, IconPanelLeft, IconPanelRight, IconServer } from '../ui/icons.tsx';
import { TitleSuggestions } from '../chat/TitleSuggestions.tsx';
import { sidebarLayout } from '../shell/layout.ts';
import { UpdatePill } from '../updates/UpdatePill.tsx';
import { UsageMeter } from '../usage/UsageMeter.tsx';
import { isBusy } from '../sidebar/status.ts';
import { LOCAL_WORKING_LABEL, isLocalModelId, localWaitLabel } from '../state/localModel.ts';
import { CaptionButtons, dragHandlers } from './caption.tsx';
import { ProjectPill } from './ProjectPill.tsx';
import { crumbsFixedWidth, MIN_TITLE_ROOM, optionalWidth, shedTier, shouldFold, titleRoom, topBarTier, type TopBarTier } from './fold.ts';
import { folderLabel, RenameEdit } from './label.ts';

export interface TopBarProps {
  /** The <ActionRow chatId/> (ui/src/actions). */
  actionRow?: ReactNode;
  /** Replaces the default "Restart to update" pill (reserved for a richer update control). */
  updatePill?: ReactNode;
  swarm?: ReactNode;
  /** Reserved: the chat-view picker (Settings → Chat). */
  chatView?: ReactNode;
}

export { folderLabel };

const localStatus = (ahead: number | null | undefined) => (ahead != null ? localWaitLabel(ahead) : LOCAL_WORKING_LABEL);

const err = (e: unknown) => toast.show({ kind: 'error', text: String((e as Error).message ?? e) });

export function TopBar({ actionRow, updatePill, swarm, chatView }: TopBarProps) {
  const shell = useShell();
  const { chatId } = useSelection();
  const panel = useSyncExternalStore(panelStore.subscribe, () => panelStore.model.visible(chatId));
  const chat = useSyncExternalStore(shell.store.subscribe, () =>
    chatId ? (shell.store.view(chatId).chat ?? shell.store.chatList().find((c) => c.id === chatId) ?? null) : null,
  );
  const [renaming, setRenaming] = useState<string | null>(null);
  const rename = (title: string) => {
    setRenaming(null);
    const t = title.trim();
    if (!chat || !t || t === chat.title) return;
    shell.api('chat_rename', { chatId: chat.id, title: t }).catch(err);
  };
  const edit = useRef<RenameEdit | null>(null);
  // What the box held when it opened: the name suggestions under it show only while it is unchanged.
  const renameFrom = useRef('');
  const startRename = (title: string) => {
    edit.current = new RenameEdit(rename);
    renameFrom.current = title;
    setRenaming(title);
  };
  const fork = () => {
    if (chat) shell.ops.fork(chat.id).then((c) => shell.select(c.id)).catch(err);
  };
  // The chat shortcuts the bar advertises. Hints read the live chord. Forking or
  // renaming behind an open dialog would surprise, so the shortcuts wait for it to close.
  const chatShortcutsOn = () => !!chat && !layers.has('dialog');
  useBinding({ id: 'chat.fork', title: 'Fork chat', defaultKeys: 'Ctrl+Shift+F', enabled: chatShortcutsOn, run: fork });
  useBinding({ id: 'chat.rename', title: 'Rename chat', defaultKeys: 'Ctrl+Alt+R', enabled: chatShortcutsOn, run: () => chat && startRename(chat.title) });
  const renameHint = useHint('Double-click to rename', 'chat.rename');
  const panelHint = useHint('Toggle inspector', 'panel.toggle');
  const showSidebarHint = useHint('Show sidebar', 'sidebar.toggle');
  // The bar sheds optional parts as the main pane narrows, so the title and the window buttons
  // always fit (they used to be pushed under the side panel). The width alone does not know what
  // else is in the bar (the show-sidebar button, a local-model chip), so the bar also sheds them
  // early when the title would be left too little room, and takes them back once they fit again
  // with room to spare.
  const barRef = useRef<HTMLElement | null>(null);
  const [widthTier, setWidthTier] = useState<TopBarTier>('wide');
  const [shed, setShed] = useState(false);
  // What the optional parts (the usage meter) took before they were shed: what taking them back
  // would cost the title.
  const fullWidth = useRef(0);
  const tier = shed ? shedTier(widthTier) : widthTier;
  useLayoutEffect(() => {
    const el = barRef.current;
    const crumbs = el?.querySelector<HTMLElement>('.crumbs');
    const actions = el?.querySelector<HTMLElement>('.topbar-actions');
    if (!el || !crumbs || !actions) return;
    const measure = () => {
      // A new width tier is laid out before the title's room is judged (this runs again then).
      const wt = topBarTier(el.getBoundingClientRect().width);
      if (wt !== widthTier) return setWidthTier(wt);
      const now = actions.getBoundingClientRect().width;
      if (!shed) fullWidth.current = now;
      const room = titleRoom(crumbs.getBoundingClientRect().width, crumbsFixedWidth(crumbs), el.scrollWidth, el.clientWidth);
      // The action row folds first; the bar sheds only after that, and only when there is something
      // to shed. While shed, the row stays folded (see ActionRow), so taking the parts back is
      // measured against the same folded row.
      const rowFolded = !actions.querySelector('.action-row:not([data-folded])');
      const next = shed ? shouldFold(true, room, fullWidth.current, now) : room < MIN_TITLE_ROOM && rowFolded && optionalWidth(el) > 0;
      if (next !== shed) setShed(next);
    };
    measure();
    const ro = new ResizeObserver(measure);
    ro.observe(el);
    ro.observe(actions);
    const mo = new MutationObserver(measure);
    mo.observe(crumbs, { childList: true });
    return () => {
      ro.disconnect();
      mo.disconnect();
    };
  }, [shed, widthTier]);
  const sidebarVisible = useSyncExternalStore(sidebarLayout.subscribe, () => sidebarLayout.get().visible);
  return (
    <header ref={barRef} className="topbar" data-testid="topbar" data-tier={tier} data-shed={shed ? 'true' : undefined} {...dragHandlers()}>
      {!sidebarVisible && (
        <Button variant="icon" label={showSidebarHint} data-testid="show-sidebar" onClick={() => sidebarLayout.setVisible(true)}>
          <IconPanelLeft size={16} />
        </Button>
      )}
      <nav className="crumbs" aria-label="Chat">
        {renaming !== null ? (
          <span className="crumb-rename-wrap">
            <TextInput
              className="crumb-rename"
              autoFocus
              aria-label="Chat title"
              value={renaming}
              onChange={(e) => setRenaming(e.target.value)}
              onBlur={() => edit.current?.commit(renaming)}
              onKeyDown={(e) => {
                if (e.key === 'Enter') edit.current?.commit(renaming);
                if (e.key === 'Escape') {
                  e.stopPropagation();
                  edit.current?.cancel();
                  setRenaming(null);
                }
              }}
            />
            {chat && <TitleSuggestions api={shell.api} chatId={chat.id} value={renaming} opened={renameFrom.current} onPick={setRenaming} />}
          </span>
        ) : (
          <button
            type="button"
            className="crumb-title"
            data-testid="topbar-title"
            title={chat ? `${chat.title}
${renameHint}` : undefined}
            onDoubleClick={() => chat && startRename(chat.title)}
            onKeyDown={(e) => {
              if (chat && (e.key === 'Enter' || e.key === ' ')) {
                e.preventDefault();
                startRename(chat.title);
              }
            }}
          >
            {chat?.title ?? (chatId ? '' : 'New chat')}
          </button>
        )}
        {/* A muted chat says so beside its title: it raises no notices and no flags. The chat menu's Mute / Unmute chat changes it. */}
        {chat?.muted && (
          <span className="crumb-muted" data-testid="topbar-muted" role="img" aria-label="Muted: no notices or flags" title="Muted: no notices or flags">
            <IconMute size={13} />
          </span>
        )}
        {/* A busy chat on a local model server says which half of its wait it is in: queued
            behind other work, or being answered. The model server answers one request at a time.
            The chip is one line at every width: the glyph and one word, with the full wording in
            its tooltip; a short bar drops the word and keeps the glyph. */}
        {chat && isBusy(chat.status) && isLocalModelId(chat.model) && (
          <span
            className="crumb-status muted"
            data-testid="topbar-local-status"
            data-ahead={chat.localAhead != null ? String(chat.localAhead) : undefined}
            role="img"
            aria-label={localStatus(chat.localAhead)}
            title={localStatus(chat.localAhead)}
          >
            <IconServer size={13} />
            <span className="crumb-status-word">Local</span>
          </span>
        )}
        {/* The project pill comes after the title: what the chat is shown under, and from there
            where it runs. It gives way (ellipsises) before the title does. */}
        <ProjectPill chatId={chatId} />
      </nav>
      <div className="topbar-actions">
        <UsageMeter />
        {updatePill ?? <UpdatePill />}
        {swarm}
        {actionRow}
        {chat && (
          <Button variant="icon" label={panelHint} pressed={panel} data-testid="panel-toggle" onClick={togglePanel}>
            <IconPanelRight size={16} />
          </Button>
        )}
        {chatView}
      </div>
      <CaptionButtons />
    </header>
  );
}
