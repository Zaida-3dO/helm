// Handing a session someone else opened to the terminal dock: the dock adds it as a tab, shows
// itself and focuses it. The chat transcript uses this for "Sign in again".

import type { TerminalSession } from '../api/terminal.ts';

type Listener = (s: TerminalSession) => void;

let listener: Listener | null = null;
let waiting: TerminalSession[] = [];

/** Ask the dock to take this session. Held until the dock is listening, so an early call is not lost. */
export function adoptSession(s: TerminalSession): void {
  if (listener) listener(s);
  else waiting.push(s);
}

/** The dock listens here; sessions handed over before it did arrive at once. Returns the stop function. */
export function onAdoptSession(fn: Listener): () => void {
  listener = fn;
  const early = waiting;
  waiting = [];
  for (const s of early) fn(s);
  return () => {
    if (listener === fn) listener = null;
  };
}
