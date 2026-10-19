// The app frame: sidebar | main pane (top bar, chat, terminal
// dock) | side panel, plus the toast overlay and the dialogs.

import { useEffect, useMemo, useState } from 'react';
import type { Api } from './api/client.ts';
import type { EventBus } from './api/events.ts';
import { ActionRow, LeafSlots } from './actions/index.ts';
import { SearchDialog } from './search/index.ts';
import { chatOps } from './api/ops.ts';
import { ChatMenu, ProjectTags, Sidebar } from './sidebar/index.ts';
import { ChatView } from './ChatView.tsx';
import { SidePanel } from './panel/index.ts';
import type { SendShortcut } from './composer/keymap.ts';
import { SETTINGS_DIALOG, SettingsDialog } from './settings/index.ts';
import './settings/general.tsx';
import './settings/shell/ShellPane.tsx';
import './settings/mcp/index.tsx';
import './settings/usage/index.tsx';
import './settings/schedules/index.tsx';
import './settings/orchestrator/index.tsx';
import './orchestrator/index.ts'; // the composer's Orchestrator switch
import { AfkPresence } from './afk/index.tsx'; // Settings → AFK and presence
import { GLOBAL_HOTKEY_BINDING, shellOps } from './api/shell.ts';
import { ShellController, ShellProvider, useSelection, useShell } from './shell/context.ts';
import type { Flushers } from './shell/quit.ts';
import { connectAppearance } from './shell/theme.ts';
import { KEYBINDINGS_PREF, keys, layers, ToastSlotView } from './ui/index.ts';
import { ChatStore } from './state/store.ts';
import { TopBar } from './topbar/TopBar.tsx';
import { TerminalDock } from './terminal/TerminalDock.tsx';
import { Notifications } from './notifications/Notifications.tsx';
import { SettingsToast } from './hosttools/SettingsToast.tsx';
import { followSendShortcut } from './hosttools/sendShortcut.ts';
import './hosttools/index.ts'; // the Allow cards

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
  // The tray, a toast or the hotkey asked to open a chat.
  useEffect(
    () =>
      bus.on('shell_open_chat', (e) => {
        const id = (e as { chatId?: unknown }).chatId;
        if (typeof id === 'string') shell.select(id);
      }),
    [bus, shell],
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
    <ShellProvider shell={shell}>
      <Frame flushers={flushers} />
    </ShellProvider>
  );
}

function Frame({ flushers }: { flushers: Flushers }) {
  const shell = useShell();
  const { chatId } = useSelection();
  const [sendShortcut, setSendShortcut] = useState<SendShortcut>('mod-enter');
  // The send shortcut setting, read once and followed live (a chat can change it).
  useEffect(() => followSendShortcut(shell.api, shell.bus, setSendShortcut), [shell]);

  return (
    <div className="shell" data-testid="shell">
      <aside className="shell-sidebar" aria-label="Sidebar">
        <Sidebar /> {/* chat and project tree */}
      </aside>
      <main className="shell-main">
        <TopBar actionRow={<ActionRow chatId={chatId} />} chatMenu={<ChatMenu chatId={chatId} />} tags={<ProjectTags chatId={chatId} />} />
        <div className="shell-chat">
          {chatId ? (
            <ChatView key={chatId} chatId={chatId} store={shell.store} ops={shell.ops} flushers={flushers} sendShortcut={sendShortcut} />
          ) : (
            <div className="empty-main">Pick a chat, or enter a folder and press New chat.</div>
          )}
        </div>
        <TerminalDock />
      </main>
      <SidePanel api={shell.api} chatId={chatId} /> {/* side panel */}
      <ToastSlotView />
      <SettingsToast api={shell.api} bus={shell.bus} onOpenChat={(id) => shell.select(id)} /> {/* settings-change toast */}
      <Notifications />
      <SettingsDialog />
      <SearchDialog /> {/* Mod+K search */}
      <LeafSlots /> {/* registers the action slots */}
      <AfkPresence /> {/* AFK presence */}
      {null /* <SavedDialog /> */}
    </div>
  );
}
