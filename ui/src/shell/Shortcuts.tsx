// The shell's own navigation shortcuts: next and previous chat in the order the
// sidebar draws them (the sidebar publishes that order in sidebar/rowOrder.ts; the shell selects),
// and Ctrl+B to hide or show the sidebar. In the composer Ctrl+B is bold: the editor takes the key first, so the sidebar toggle never sees it there.
// None of them act behind an open dialog.

import { stepVisibleChat } from '../sidebar/rowOrder.ts'; // the sidebar's published order
import { useShell } from './context.ts';
import { sidebarLayout } from './layout.ts';
import { layers, useBinding } from '../ui/index.ts';

const noDialog = () => !layers.has('dialog');

export function ShellShortcuts() {
  const shell = useShell();
  const step = (dir: 1 | -1) => {
    const id = stepVisibleChat(shell.selection.chatId, dir);
    if (id && id !== shell.selection.chatId) shell.select(id);
  };
  useBinding({ id: 'chat.next', title: 'Next chat', defaultKeys: 'Ctrl+Tab', enabled: noDialog, run: () => step(1) });
  useBinding({ id: 'chat.previous', title: 'Previous chat', defaultKeys: 'Ctrl+Shift+Tab', enabled: noDialog, run: () => step(-1) });
  useBinding({ id: 'sidebar.toggle', title: 'Hide or show the sidebar', defaultKeys: 'Ctrl+B', enabled: noDialog, run: sidebarLayout.toggle });
  return null;
}
