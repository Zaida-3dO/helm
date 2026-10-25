// Always-mounted parts of Settings (rendered beside the settings dialog): the shortcuts sheet on
// Ctrl+/, the zoom shortcuts, and the sidebar entries the user has hidden.

import { useEffect } from 'react';
import { useDialog, useShell } from '../shell/context.ts';
import { parseZoom, ZOOM_DEFAULT } from '../shell/theme.ts';
import { Dialog, toast, useBinding } from '../ui/index.ts';
import { parseNameList } from './applied.ts';
import { stepZoom } from './generalOptions.ts';
import { KeyboardPane } from './keyboard/index.tsx';
import './watchers.css';

export const SHORTCUTS_DIALOG = 'shortcuts';

export function SettingsWatchers() {
  const shell = useShell();

  // Hidden sidebar entries become one attribute on the page; the stylesheet does the hiding.
  useEffect(() => {
    const root = document.documentElement;
    const apply = (v: unknown) => {
      const ids = parseNameList(v);
      if (ids.length > 0) root.dataset.hideSidebar = ids.join(' ');
      else delete root.dataset.hideSidebar;
    };
    shell
      .api<unknown>('preference_get', { key: 'hiddenSidebarItems' })
      .then(apply)
      .catch(() => undefined);
    return shell.bus.on('preferences', (e) => {
      const changed = (e as { changed?: Record<string, unknown> }).changed ?? {};
      if ('hiddenSidebarItems' in changed) apply(changed.hiddenSidebarItems);
    });
  }, [shell]);

  const zoom = async (dir: 1 | -1 | 0) => {
    let before = ZOOM_DEFAULT;
    try {
      before = parseZoom(await shell.api<unknown>('preference_get', { key: 'zoomLevel' }));
    } catch {
      /* the default is the starting point */
    }
    const next = dir === 0 ? ZOOM_DEFAULT : stepZoom(before, dir);
    toast.show({ kind: 'info', text: `Zoom ${next}%` });
    if (next === before) return;
    shell.bus.dispatch({ kind: 'preferences', changed: { zoomLevel: next } });
    shell.api('preference', { key: 'zoomLevel', value: next }).catch((e: unknown) => {
      shell.bus.dispatch({ kind: 'preferences', changed: { zoomLevel: before } });
      toast.show({ kind: 'error', text: `Could not save: ${String((e as Error).message ?? e)}` });
    });
  };
  useBinding({ id: 'window.zoomIn', title: 'Zoom in', defaultKeys: 'Ctrl+=', run: () => void zoom(1) });
  useBinding({ id: 'window.zoomOut', title: 'Zoom out', defaultKeys: 'Ctrl+-', run: () => void zoom(-1) });
  useBinding({ id: 'window.zoomReset', title: 'Reset zoom', defaultKeys: 'Ctrl+0', run: () => void zoom(0) });
  useBinding({
    id: 'shortcuts.sheet',
    title: 'Keyboard shortcuts',
    defaultKeys: 'Ctrl+/',
    run: () => (shell.openDialogId === SHORTCUTS_DIALOG ? shell.closeDialog(SHORTCUTS_DIALOG) : shell.openDialog(SHORTCUTS_DIALOG)),
  });

  const { open, close } = useDialog(SHORTCUTS_DIALOG);
  if (!open) return null;
  return (
    <Dialog title="Keyboard shortcuts" onClose={close} testId="shortcuts-sheet">
      <div className="shortcuts-sheet">
        <KeyboardPane />
      </div>
    </Dialog>
  );
}
