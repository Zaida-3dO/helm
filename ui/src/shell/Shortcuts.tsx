// The shell's own navigation shortcuts: next and previous chat in the order the
// sidebar draws them. The sidebar publishes that order (sidebar/rowOrder.ts); the shell selects.

import { stepVisibleChat } from '../sidebar/rowOrder.ts'; // the sidebar's published order
import { useShell } from './context.ts';
import { useBinding } from '../ui/index.ts';

export function ShellShortcuts() {
  const shell = useShell();
  const step = (dir: 1 | -1) => {
    const id = stepVisibleChat(shell.selection.chatId, dir);
    if (id && id !== shell.selection.chatId) shell.select(id);
  };
  useBinding({ id: 'chat.next', title: 'Next chat', defaultKeys: 'Ctrl+Tab', run: () => step(1) });
  useBinding({ id: 'chat.previous', title: 'Previous chat', defaultKeys: 'Ctrl+Shift+Tab', run: () => step(-1) });
  return null;
}
