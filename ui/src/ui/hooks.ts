// React glue for the layer stack and the key registry.

import { useEffect, useRef, useSyncExternalStore } from 'react';
import { keys, type Binding } from './keys.ts';
import { layers, type LayerKind } from './layers.ts';

/**
 * Hold an Escape layer while `open` is true. The latest `onEscape` is always called (no stale
 * closures); return false from it to pass Escape on.
 */
export function useLayer(kind: LayerKind, open: boolean, onEscape: () => boolean | void, active?: () => boolean): void {
  const ref = useRef({ onEscape, active });
  ref.current = { onEscape, active };
  useEffect(() => {
    if (!open) return;
    return layers.push({
      kind,
      onEscape: () => ref.current.onEscape(),
      active: () => ref.current.active?.() ?? true,
    });
  }, [kind, open]);
}

/** Register a command for as long as the component is mounted. */
export function useBinding(b: Binding): void {
  const ref = useRef(b);
  ref.current = b;
  useEffect(
    () =>
      keys.register({
        ...b,
        run: () => ref.current.run?.(),
        enabled: () => ref.current.enabled?.() ?? true,
      }),
    // Re-register only when the identity or default changes.
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [b.id, b.defaultKeys, b.scope, b.title],
  );
}

/** The chord currently bound to a command (re-renders on rebinding), for hints and tooltips. */
export function useChord(id: string): string | null {
  useSyncExternalStore(keys.subscribe, keys.getVersion);
  return keys.chordFor(id);
}
