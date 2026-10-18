// Presence for auto-AFK (reduced to all-chats mode): tells the host when Helm's
// window gains or loses focus, and that the user gave input in it (at most every 20 s). Only
// trusted events count, and nothing about the input itself is sent.

import { useEffect } from 'react';
import { useShell } from '../shell/context.ts';
import { afkOps } from './api.ts';
import { PresenceThrottle } from './model.ts';

export function AfkPresence() {
  const shell = useShell();
  useEffect(() => {
    const ops = afkOps(shell.api);
    const throttle = new PresenceThrottle();
    const send = (p: { focused?: boolean; input?: boolean }) => void ops.presence(p).catch(() => undefined);
    const onFocus = () => send({ focused: true });
    const onBlur = () => send({ focused: false });
    const onInput = (e: Event) => {
      if (e.isTrusted && throttle.input(Date.now())) send({ focused: document.hasFocus(), input: true });
    };
    send({ focused: document.hasFocus() });
    window.addEventListener('focus', onFocus);
    window.addEventListener('blur', onBlur);
    window.addEventListener('keydown', onInput, true);
    window.addEventListener('pointerdown', onInput, true);
    return () => {
      window.removeEventListener('focus', onFocus);
      window.removeEventListener('blur', onBlur);
      window.removeEventListener('keydown', onInput, true);
      window.removeEventListener('pointerdown', onInput, true);
    };
  }, [shell]);
  return null;
}
