// The app frame: sidebar | main pane (top bar, chat, terminal
// dock) | side panel, plus the toast overlay and the dialogs.

import { useEffect, useMemo, useState } from 'react';
import type { Api } from './api/client.ts';
import type { EventBus } from './api/events.ts';
import { ActionRow, LeafSlots } from './actions/index.ts';
import { SearchDialog } from './search/index.ts';
import { chatOps } from './api/ops.ts';
import { Sidebar } from './sidebar/index.ts';
import { ChatView } from './ChatView.tsx';
import { SidePanel } from './panel/index.ts';
import type { SendShortcut } from './composer/keymap.ts';
import { SETTINGS_DIALOG, SettingsDialog } from './settings/index.ts';
import './settings/general.tsx';
import { ShellController, ShellProvider, useSelection, useShell } from './shell/context.ts';
import type { Flushers } from './shell/quit.ts';
import { connectAppearance } from './shell/theme.ts';
import { KEYBINDINGS_PREF, keys, layers, ToastSlotView } from './ui/index.ts';
import { ChatStore } from './state/store.ts';
import { TopBar } from './topbar/TopBar.tsx';

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
  useEffect(() => {
    // The send shortcut setting; missing or unreadable means the default.
    shell
      .api<unknown>('preference_get', { key: 'sendShortcut' })
      .then((v) => {
        if (v === 'enter' || v === 'mod-enter') setSendShortcut(v);
      })
      .catch(() => undefined);
  }, [shell]);

  return (
    <div className="shell" data-testid="shell">
      <aside className="shell-sidebar" aria-label="Sidebar">
        <Sidebar /> {/* chat and project tree */}
      </aside>
      <main className="shell-main">
        <TopBar actionRow={<ActionRow chatId={chatId} />} chatMenu={null /* <ChatMenu chatId={chatId} /> */} />
        <div className="shell-chat">
          {chatId ? (
            <ChatView key={chatId} chatId={chatId} store={shell.store} ops={shell.ops} flushers={flushers} sendShortcut={sendShortcut} />
          ) : (
            <div className="empty-main">Pick a chat, or enter a folder and press New chat.</div>
          )}
        </div>
        {null /* <TerminalDock /> */}
      </main>
      <SidePanel api={shell.api} chatId={chatId} /> {/* side panel */}
      <ToastSlotView />
      {null /* <Notifications /> (the completion stack, bottom-right) */}
      <SettingsDialog />
      <SearchDialog /> {/* Mod+K search */}
      <LeafSlots /> {/* registers the action slots */}
      {null /* <SavedDialog /> */}
    </div>
  );
}
