// The AFK pill in the composer toolbar: shown in every chat while AFK is on, with Exit. One shared
// read of the host's AFK state serves every mounted composer; the host's `afk` event refreshes it.

import { useEffect, useState } from 'react';
import { useShell } from '../shell/context.ts';
import { IconMoon } from '../ui/icons.tsx';
import { afkOps, type AfkView } from './api.ts';
import { pillModel } from './model.ts';
import './afk.css';

export function AfkPill() {
  const shell = useShell();
  const [view, setView] = useState<AfkView | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  useEffect(() => {
    let live = true;
    afkOps(shell.api)
      .state()
      .then((v) => live && setView(v))
      .catch(() => undefined);
    const off = shell.bus.on('afk', (e) => setView(e as unknown as AfkView));
    return () => {
      live = false;
      off();
    };
  }, [shell]);
  const m = view ? pillModel(view) : null;
  if (!m) return null;
  const exit = () => {
    setBusy(true);
    setError(null);
    afkOps(shell.api)
      .set(false)
      .then(setView)
      .catch((e: unknown) => setError(String((e as Error)?.message ?? e)))
      .finally(() => setBusy(false));
  };
  return (
    <span className="afk-pill" data-testid="afk-pill" title={m.tooltip}>
      <IconMoon size={11} />
      <span data-testid="afk-pill-label">{m.label}</span>
      <button type="button" className="afk-pill-exit" data-testid="afk-pill-exit" disabled={busy} onClick={exit}>
        Exit
      </button>
      {error && (
        <span className="afk-pill-error" data-testid="afk-pill-error" title={error}>
          {error}
        </span>
      )}
    </span>
  );
}
