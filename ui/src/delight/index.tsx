// The small joys, mounted once from the app frame: the sweep around a finished chat's row, the
// first-run intro, and the ways the desktop pet asks Helm to open something. The pet's own
// settings section is registered here too, by importing it. Everything honours the quiet level and
// reduced motion (see celebrate.ts and the pet's page).

import { useEffect } from 'react';
import { requestPane } from '../settings/registry.ts';
import { SETTINGS_DIALOG } from '../settings/index.ts';
import { useShell } from '../shell/context.ts';
import '../pet/settings.tsx';
import { celebrate, type CompletionLike } from './celebrate.ts';
import { Intro } from './Intro.tsx';
import './celebrate.css';

export { celebrate } from './celebrate.ts';

export function Delight() {
  const shell = useShell();
  useEffect(
    () =>
      shell.bus.on('completion', (e) => {
        const c = (e as { completion?: CompletionLike }).completion;
        if (c && typeof c.chatId === 'string') celebrate.completion(c);
      }),
    [shell],
  );
  // The desktop pet asked for the Needs attention list or the Appearance settings.
  useEffect(() => {
    const offA = shell.bus.on('shell_open_attention', () => document.querySelector<HTMLElement>('[data-testid="nav-attention"]')?.click());
    const offS = shell.bus.on('shell_open_settings', (e) => {
      const pane = (e as { pane?: unknown }).pane;
      if (typeof pane === 'string') requestPane(pane);
      shell.openDialog(SETTINGS_DIALOG);
    });
    return () => {
      offA();
      offS();
    };
  }, [shell]);
  return <Intro api={shell.api} />;
}
