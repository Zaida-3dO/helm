// The graphics-card state and actions, shared by the Graphics card section of
// Settings → Local models and by nothing else: the top bar used to carry an unload button and a
// "GPU reserved" pill, and both moved to Settings (a chat can do the same through the app_gpu
// tool). The hooks below read the host's lock view and drive unload (with its confirm dialog),
// turning back on, and keeping models off after a game.

import { useCallback, useEffect, useMemo, useState } from 'react';
import { useShell } from '../shell/context.ts';
import { ConfirmDialog, toast, type ConfirmSpec } from '../ui/index.ts';
import { gpuOps, type LockView } from './api.ts';
import { unloadConfirmLines } from './model.ts';
import './gpu.css';

const errorText = (e: unknown) => String((e as Error).message ?? e);

/** The host's lock view, kept fresh by its `local_lock` event (and `local_models`, which carries the servers' on/off switch). */
export function useLockView(): { view: LockView | null; set: (v: LockView) => void } {
  const shell = useShell();
  const [view, setView] = useState<LockView | null>(null);
  useEffect(() => {
    const ops = gpuOps(shell.api);
    let live = true;
    const read = () =>
      void ops
        .state()
        .then((v) => live && setView(v))
        .catch(() => undefined);
    read();
    const offs = [shell.bus.on('local_lock', read), shell.bus.on('local_models', read)];
    return () => {
      live = false;
      offs.forEach((off) => off());
    };
  }, [shell]);
  return { view, set: setView };
}

/** Unload (after the confirm) and turn back on, as Settings draws them. */
export function useGpuActions(view: LockView | null, set: (v: LockView) => void) {
  const shell = useShell();
  const ops = useMemo(() => gpuOps(shell.api), [shell.api]);
  const [confirm, setConfirm] = useState<ConfirmSpec | null>(null);
  const [turningOn, setTurningOn] = useState(false);

  const askUnload = useCallback(() => {
    const [ask, what] = unloadConfirmLines(view?.restartTask);
    setConfirm({
      title: 'Unload local models?',
      body: (
        <>
          <span>{ask}</span>
          <span className="gpu-confirm-line">{what}</span>
        </>
      ),
      confirm: 'Unload',
      danger: true,
      onConfirm: () =>
        ops
          .unload()
          .then(set)
          .catch((e: unknown) => {
            toast.show({ kind: 'error', text: errorText(e) });
            // Another window or a game may have locked first: show what is true now.
            void ops.state().then(set).catch(() => undefined);
          }),
    });
  }, [ops, set, view?.restartTask]);

  const turnOn = useCallback(async () => {
    setTurningOn(true);
    try {
      set(await ops.reload());
    } catch (e) {
      toast.show({ kind: 'error', text: errorText(e) });
    } finally {
      setTurningOn(false);
    }
  }, [ops, set]);

  // While a game holds the lock, this keeps models off after the game closes.
  const keepOff = useCallback(async () => {
    try {
      set(await ops.unload());
    } catch (e) {
      toast.show({ kind: 'error', text: errorText(e) });
      void ops.state().then(set).catch(() => undefined);
    }
  }, [ops, set]);

  const dialog = confirm && <ConfirmDialog spec={confirm} onClose={() => setConfirm(null)} />;
  return { askUnload, turnOn, keepOff, turningOn, dialog };
}
