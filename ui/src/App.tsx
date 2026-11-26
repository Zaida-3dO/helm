// The app frame: sidebar | main pane (top bar, chat, terminal
// dock) | side panel, plus the toast overlay and the dialogs.

import { useEffect, useLayoutEffect, useMemo, useRef, useState, useSyncExternalStore, type CSSProperties } from 'react';
import type { Api } from './api/client.ts';
import type { EventBus } from './api/events.ts';
import { ActionRow, LeafSlots } from './actions/index.ts';
import { CheckpointSlot } from './checkpoints/index.ts';
import { ClaudeChatViewer } from './claudechats/index.ts';
import { SearchDialog } from './search/index.ts';
import { chatOps, type ChatOps } from './api/ops.ts';
import { Sidebar } from './sidebar/index.ts';
import { ChatView } from './ChatView.tsx';
import { applyPanelOpen, openInPanel, panelStore, SidePanel, registerLinkRoute } from './panel/index.ts';
import { showSlot } from './panel/registry.ts';
import { pageOps } from './pages/api.ts';
import { openPageTab, openRemotePageTab } from './pages/open.ts';
import { openTasks } from './tasks/index.ts';
import { devicesStoreFor } from './devices/store.ts';
import { remoteHostOf } from './devices/state.ts';
import type { SendShortcut } from './composer/keymap.ts';
import { SETTINGS_DIALOG, SettingsDialog } from './settings/index.ts';
import './settings/general.tsx';
import './settings/shell/ShellPane.tsx';
import './settings/keyboard/index.tsx';
import './settings/mcp/index.tsx';
import './settings/hub/index.tsx';
import './settings/profiles/index.tsx';
import './settings/devices/index.tsx';
import './settings/usage/index.tsx';
import './settings/schedules/index.tsx';
import './settings/queue/index.tsx';
import './settings/orchestrator/index.tsx';
import './settings/runtime/index.tsx';
import './settings/github/index.tsx';
import './settings/websearch/index.tsx';
import './settings/skills/index.tsx';
import './settings/data/index.tsx';
import './settings/localmodels/index.tsx';
import './settings/memory/index.tsx';
import './settings/gatekeeper/index.tsx';
import './saved/SavedPane.tsx';
import './orchestrator/index.ts'; // the composer's Orchestrator switch
import { AfkPresence } from './afk/index.tsx'; // Settings → AFK and presence
import { GLOBAL_HOTKEY_BINDING, shellOps } from './api/shell.ts';
import { ShellController, ShellProvider, useSelection, useShell } from './shell/context.ts';
import type { Flushers } from './shell/quit.ts';
import { connectAppearance } from './shell/theme.ts';
import { connectSounds } from './shell/sounds.ts';
import { KEYBINDINGS_PREF, keys, layers, toast, ToastSlotView } from './ui/index.ts';
import { ChatStore } from './state/store.ts';
import { TopBar } from './topbar/TopBar.tsx';
import { EmptyMain } from './shell/EmptyMain.tsx';
import { ShellShortcuts } from './shell/Shortcuts.tsx';
import { SidebarResizer } from './shell/SidebarResizer.tsx';
import { carryOnRollOver, decideOpen, openTarget, type OpenEvent } from './shell/follow.ts';
import { SIDEBAR_VISIBLE_PREF, SIDEBAR_WIDTH_PREF, sidebarLayout } from './shell/layout.ts';
import { TerminalDock } from './terminal/TerminalDock.tsx';
import { SavedDueNotice } from './saved/SavedDueNotice.tsx';
import { SettingsToast } from './hosttools/SettingsToast.tsx';
import { followSendShortcut } from './hosttools/sendShortcut.ts';
import './hosttools/index.ts'; // the Allow cards
import { NewChatHost } from './newchat/index.ts';
import { IconPickerHost } from './projects/index.ts';
import { SchedulesViewHost } from './schedules/SchedulesView.tsx';
import { FrameStats } from './perf/index.ts'; // the frame-stats overlay (Mod+Shift+P)
import { ErrorBoundary } from './safety/ErrorBoundary.tsx';
import { Delight } from './delight/index.tsx'; // celebrations, the first-run intro, the pet's settings

/**
 * Open a chat window by id. A session that rolled over points at where the chat moved: the
 * stack's newest session opens instead — one window spans the whole stack — and a message to
 * land on is taken along, since a jump resolves to any session of the stack.
 */
async function openChatWindow(ops: ChatOps, shell: ShellController, chatId: string, messageId?: string): Promise<void> {
  let target = chatId;
  try {
    if ((await ops.get(chatId)).chat.stackId !== null) target = openTarget(chatId, (await ops.stack(chatId)).latestId);
  } catch {
    // The id we were given still opens; ChatView reports what it cannot load.
  }
  shell.select(target, messageId ? { messageId } : {});
}

export function App({ api, bus, flushers }: { api: Api; bus: EventBus; flushers: Flushers }) {
  const ops = useMemo(() => chatOps(api), [api]);
  const store = useMemo(() => new ChatStore(), []);
  const shell = useMemo(() => new ShellController(api, bus, store, ops), [api, bus, store, ops]);

  useEffect(() => store.connect(bus), [store, bus]);
  useEffect(() => {
    void shell.restore();
    return flushers.add(() => shell.flush());
  }, [shell, flushers]);
  // Colour mode, and the user's key map; both re-read when a preference changes.
  useEffect(() => connectAppearance(api, bus, document.documentElement), [api, bus]);
  // Quiet cues for a finished chat and a chat needing attention, while the window is unfocused.
  useEffect(() => connectSounds(api, bus), [api, bus]);
  useEffect(() => {
    const load = () =>
      api<unknown>('preference_get', { key: KEYBINDINGS_PREF })
        .then((v) => keys.loadUserMap(v))
        .catch(() => undefined);
    void load();
    return bus.on('preferences', (e) => {
      const changed = (e as { changed?: Record<string, unknown> }).changed ?? {};
      if (KEYBINDINGS_PREF in changed) keys.loadUserMap(changed[KEYBINDINGS_PREF]);
    });
  }, [api, bus]);
  // The tray, a toast or the hotkey asked to open a chat; a chat another chat started follows
  // the window only while the user is still looking at the one it came from, else it notices.
  useEffect(
    () =>
      bus.on('shell_open_chat', (e) => {
        const d = decideOpen(e as OpenEvent, shell.selection.chatId);
        if (!d) return;
        // A roll over's successor takes over the old session's side panel before the window
        // moves, so the reader finds the panel as they left it, whether or not they follow now.
        const carried = carryOnRollOver(e as OpenEvent, async (id) => (await ops.get(id)).chat, (from, to) => panelStore.carry(from, to));
        if (d.kind === 'select') {
          void carried.then(() => openChatWindow(ops, shell, d.chatId, d.messageId));
          return;
        }
        toast.show({
          kind: 'info',
          text: d.title ? `New chat started: ${d.title}` : 'A new chat is ready',
          action: { label: 'Open', run: () => void openChatWindow(ops, shell, d.chatId) },
          ttlMs: 8000,
        });
      }),
    [bus, ops, shell],
  );
  // A roll over that could not finish says why (the old chat is left as it was).
  useEffect(
    () =>
      bus.on('chat_roll_over_failed', (e) => {
        const message = (e as { message?: unknown }).message;
        toast.show({ kind: 'error', text: `Roll over stopped: ${typeof message === 'string' ? message : 'something went wrong.'}` });
      }),
    [bus],
  );
  // Helm's own links, each checked by the link gate first (panel/links.ts). File links need no route
  // here: they go to the host's resolve, as a path written in a reply does.
  // Chat links in replies (`helm://chats/<id>`) open that chat, if it still exists.
  useEffect(
    () =>
      registerLinkRoute('chat', async (req) => {
        try {
          await ops.get(req.chatId);
        } catch {
          return 'That chat no longer exists.';
        }
        // A rolled-over session's link opens the window the chat lives in now, at the message it names.
        await openChatWindow(ops, shell, req.chatId, req.messageId !== undefined ? String(req.messageId) : undefined);
        return true;
      }),
    [ops, shell],
  );
  // Page links (`helm://pages/<pin id>`) open that pinned page: on this PC for a chat that runs
  // here, else on the PC the chat runs on (its pin ids are that PC's).
  useEffect(() => {
    const hostOf = async (chatId: string | null): Promise<string | null> => {
      if (!chatId) return null;
      const { chat } = await ops.get(chatId);
      const devices = devicesStoreFor(api, bus);
      devices.start();
      return remoteHostOf(chat, devices.snapshot.list, devices.snapshot.selfUid);
    };
    const noRoom = 'The side panel has no room for another tab.';
    const offPage = registerLinkRoute('page', async (req, ctx) => {
      const pages = pageOps(api);
      const host = await hostOf(ctx.chatId);
      if (host) {
        // The list names only the person's own pins; a pin of someone else's still opens by id.
        const listed = await pages.devicePages().catch(() => null);
        const found = listed?.devices.find((d) => d.deviceUid === host)?.pages.find((p) => p.id === req.pageId);
        return openRemotePageTab(host, found ?? { id: req.pageId, path: '', title: 'Page' }, ctx.chatId) || noRoom;
      }
      const found = (await pages.list()).pages.find((p) => p.id === req.pageId);
      if (!found) return 'That page is not pinned on this PC any more.';
      return openPageTab(found.path, ctx.chatId) || noRoom;
    });
    // Panel links (`helm://panel/changes|files|tasks`) show that view for the chat.
    const offPanel = registerLinkRoute('panel', async (req, ctx) => {
      if (req.view === 'tasks') {
        const host = await hostOf(ctx.chatId);
        return openTasks(api, undefined, host ? ctx.chatId : null) || noRoom;
      }
      if (!ctx.chatId) return 'Open a chat first: that view belongs to a chat.';
      showSlot(req.view);
      return true;
    });
    return () => {
      offPage();
      offPanel();
    };
  }, [api, bus, ops]);
  // A chat that shows the user a file or a page (app_open_panel) pushes it through a `panel_open`
  // event naming that chat: it lands in the chat's own panel, even while another chat is on screen.
  useEffect(
    () =>
      bus.on('panel_open', (e) =>
        applyPanelOpen(e, {
          openTab: (kind, key, data, opts) => openInPanel(kind, key, data, opts),
          openPage: (path, chatId) => openPageTab(path, chatId),
          openModal: (path, opts) => panelStore.openModal(path, opts),
          notice: (text) => panelStore.showNotice(text),
          current: () => panelStore.current,
        }),
      ),
    [bus],
  );
  // The global hotkey lives in the same key map as every other shortcut (scope "global": the host
  // registers it with Windows; the window listener never fires it).
  useEffect(() => {
    let off = () => {};
    shellOps(api)
      .get()
      .then((v) => {
        off = keys.register({ id: GLOBAL_HOTKEY_BINDING, title: 'Show or hide Helm (anywhere in Windows)', defaultKeys: v.settings.globalHotkey, scope: 'global' });
      })
      .catch(() => undefined);
    return () => off();
  }, [api]);
  // The window is created hidden and shown once the first frame is ready.
  useEffect(() => {
    requestAnimationFrame(() => void api('window_ready').catch(() => undefined));
  }, [api]);
  // The one Escape router and the one shortcut dispatcher (ui/layers.ts, ui/keys.ts).
  useEffect(() => layers.install(document), []);
  useEffect(() => keys.install(window), []);
  useEffect(
    () =>
      shell.registerCommand('settings.toggle', {
        title: 'Settings',
        keys: 'Ctrl+,',
        run: () => (shell.openDialogId === SETTINGS_DIALOG ? shell.closeDialog(SETTINGS_DIALOG) : shell.openDialog(SETTINGS_DIALOG)),
      }),
    [shell],
  );

  return (
    <ErrorBoundary name="app" fatal>
      <ShellProvider shell={shell}>
        <Frame flushers={flushers} />
      </ShellProvider>
    </ErrorBoundary>
  );
}

function Frame({ flushers }: { flushers: Flushers }) {
  const shell = useShell();
  const { chatId } = useSelection();
  const [sendShortcut, setSendShortcut] = useState<SendShortcut>('mod-enter');
  // The send shortcut setting, read once and followed live (a chat can change it).
  useEffect(() => followSendShortcut(shell.api, shell.bus, setSendShortcut), [shell]);
  // The sidebar's width and visibility, read once and saved on change.
  const layout = useSyncExternalStore(sidebarLayout.subscribe, sidebarLayout.get);
  useEffect(() => {
    sidebarLayout.connect((key, value) => {
      shell.api('preference', { key, value }).catch((e: unknown) => toast.show({ kind: 'error', text: `Could not save the sidebar layout: ${String((e as Error).message ?? e)}` }));
    });
    const read = (key: string) => shell.api<unknown>('preference_get', { key }).catch(() => undefined);
    void Promise.all([read(SIDEBAR_WIDTH_PREF), read(SIDEBAR_VISIBLE_PREF)]).then(([w, v]) => sidebarLayout.load(w, v));
    return () => sidebarLayout.connect(() => undefined);
  }, [shell]);
  // The frame's width and the side panel beside the chat fit the sidebar (a maximised panel covers
  // the chat, so it takes no room from the sidebar); the chat area's top places the toasts
  // under the top bar and whatever the tag strip takes (nothing with no chat, two lines when it wraps).
  const frame = useRef<HTMLDivElement>(null);
  useLayoutEffect(() => {
    const el = frame.current;
    const main = el?.querySelector<HTMLElement>(':scope > .shell-main');
    const chat = main?.querySelector<HTMLElement>(':scope > .shell-chat');
    if (!el || !main || !chat) return;
    const measure = () => {
      const panel = el.querySelector<HTMLElement>(':scope > .side-panel:not(.maximized)');
      sidebarLayout.setRoom(el.getBoundingClientRect().width, panel?.getBoundingClientRect().width ?? 0);
      main.style.setProperty('--toast-top', `${chat.offsetTop}px`);
    };
    measure();
    const ro = new ResizeObserver(measure);
    for (const x of [el, main, chat]) ro.observe(x);
    return () => ro.disconnect();
  }, []);

  return (
    <div ref={frame} className="shell" data-testid="shell">
      <aside className="shell-sidebar" aria-label="Sidebar" hidden={!layout.visible} style={{ '--sidebar-width': `${layout.width}px` } as CSSProperties}>
        <ErrorBoundary name="sidebar">
          <Sidebar /> {/* chat and project tree */}
        </ErrorBoundary>
        <SidebarResizer />
      </aside>
      <main className="shell-main">
        <TopBar actionRow={<ActionRow chatId={chatId} />} />
        <div className="shell-chat">
          <ErrorBoundary name="chat" resetKey={chatId}>
            {chatId ? (
              <ChatView key={chatId} chatId={chatId} store={shell.store} ops={shell.ops} flushers={flushers} sendShortcut={sendShortcut} />
            ) : (
              <EmptyMain />
            )}
          </ErrorBoundary>
          <NewChatHost flushers={flushers} sendShortcut={sendShortcut} /> {/* the new-chat page */}
          <IconPickerHost /> {/* the project icon picker */}
          <SchedulesViewHost /> {/* the schedules view */}
        </div>
        <TerminalDock />
        <SavedDueNotice /> {/* the one notice for saved items that came due */}
        {/* Toasts sit in the main area, below the tag strip, so they never cover the title or the project chips. */}
        <ToastSlotView />
        <SettingsToast api={shell.api} bus={shell.bus} onOpenChat={(id) => shell.select(id)} /> {/* settings-change toast */}
      </main>
      <ErrorBoundary name="side panel" resetKey={chatId}>
        <SidePanel api={shell.api} chatId={chatId} /> {/* side panel */}
      </ErrorBoundary>
      <ShellShortcuts />
      <SettingsDialog />
      <SearchDialog /> {/* Mod+K search */}
      <ClaudeChatViewer /> {/* one of Claude Code's own chats, read */}
      <LeafSlots /> {/* registers the action slots */}
      <CheckpointSlot /> {/* go back to before a message */}
      <AfkPresence /> {/* AFK presence */}
      <FrameStats /> {/* frame-stats overlay */}
      <Delight /> {/* celebrations and the first-run intro */}
      {null /* the saved-items dialog mounts here */}
    </div>
  );
}
