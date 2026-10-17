// Helm's own window caption: the Windows
// minimise / maximise / close buttons drawn at the top bar's right edge, and the two drag regions
// (the top bar and the sidebar title strip). Close hides the window, as before.
// Outside Tauri (the vite fake host) the buttons render but do nothing.

import { useEffect, useState } from 'react';
import { startsDrag } from './label.ts';

type Win = {
  minimize(): Promise<void>;
  toggleMaximize(): Promise<void>;
  close(): Promise<void>;
  startDragging(): Promise<void>;
  isMaximized(): Promise<boolean>;
  onResized(cb: () => void): Promise<() => void>;
};

let winPromise: Promise<Win | null> | null = null;
function win(): Promise<Win | null> {
  if (!('__TAURI_INTERNALS__' in window)) return Promise.resolve(null);
  winPromise ??= import('@tauri-apps/api/window').then((m) => m.getCurrentWindow() as unknown as Win).catch(() => null);
  return winPromise;
}

export { startsDrag };

/** Spread onto a drag region. */
export function dragHandlers() {
  return {
    onMouseDown: (e: React.MouseEvent) => {
      if (e.detail > 1) return;
      if (startsDrag(e.target as Element, e.button, e.ctrlKey)) void win().then((w) => w?.startDragging());
    },
    onDoubleClick: (e: React.MouseEvent) => {
      if (startsDrag(e.target as Element, e.button, e.ctrlKey)) void win().then((w) => w?.toggleMaximize());
    },
  };
}

export function CaptionButtons() {
  const [max, setMax] = useState(false);
  useEffect(() => {
    let off: (() => void) | undefined;
    void win().then(async (w) => {
      if (!w) return;
      setMax(await w.isMaximized());
      off = await w.onResized(() => void w.isMaximized().then(setMax));
    });
    return () => off?.();
  }, []);
  return (
    <div className="caption" data-testid="caption">
      <button type="button" className="caption-btn" aria-label="Minimise" title="Minimise" onClick={() => void win().then((w) => w?.minimize())}>
        <svg width="10" height="10" viewBox="0 0 10 10" aria-hidden="true">
          <path d="M0 5h10" stroke="currentColor" strokeWidth="1" />
        </svg>
      </button>
      <button type="button" className="caption-btn" aria-label={max ? 'Restore' : 'Maximise'} title={max ? 'Restore' : 'Maximise'} onClick={() => void win().then((w) => w?.toggleMaximize())}>
        <svg width="10" height="10" viewBox="0 0 10 10" fill="none" stroke="currentColor" strokeWidth="1" aria-hidden="true">
          {max ? (
            <>
              <rect x="0.5" y="2.5" width="7" height="7" />
              <path d="M2.5 2.5V0.5h7v7h-2" />
            </>
          ) : (
            <rect x="0.5" y="0.5" width="9" height="9" />
          )}
        </svg>
      </button>
      <button type="button" className="caption-btn close" aria-label="Close" title="Close (Helm keeps running in the tray)" onClick={() => void win().then((w) => w?.close())}>
        <svg width="10" height="10" viewBox="0 0 10 10" aria-hidden="true">
          <path d="M0 0l10 10M10 0L0 10" stroke="currentColor" strokeWidth="1" />
        </svg>
      </button>
    </div>
  );
}
