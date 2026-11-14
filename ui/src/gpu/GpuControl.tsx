// The graphics-card control in the top bar. With a local model server switched on it is an icon
// button, "Unload local models"; the click asks first, then stops local-model chats, unloads the
// servers and refuses every load until the user turns them back on. While models are unloaded the
// button gives way to a "GPU reserved" pill (a game that is running can set it too); a click on the
// pill lists what the unload did and offers "Turn local models back on".
// The same state and actions drive the Graphics card section of Settings → Local models.

import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { useShell } from '../shell/context.ts';
import { AnchoredDialog, Button, ConfirmDialog, toast, type ConfirmSpec } from '../ui/index.ts';
import { IconGpu } from '../ui/icons.tsx';
import { WheelMark } from '../ui/status/WheelMark.tsx';
import type { Rect } from '../ui/place.ts';
import type { TopBarTier } from '../topbar/fold.ts';
import { gpuOps, type LockView } from './api.ts';
import { comeBackSentence, gpuControl, lockSentence, unloadConfirmLines } from './model.ts';
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

/** Unload (after the confirm) and turn back on, shared by the top bar and Settings. */
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

export function GpuControl({ tier }: { tier: TopBarTier }) {
  const { view, set } = useLockView();
  const { askUnload, turnOn, keepOff, turningOn, dialog } = useGpuActions(view, set);
  const [anchor, setAnchor] = useState<Rect | null>(null);
  const pill = useRef<HTMLButtonElement | null>(null);
  const face = gpuControl(view, tier);

  // The popover belongs to the pill: when the lock lifts underneath it, it closes.
  useEffect(() => {
    if (face.face !== 'locked') setAnchor(null);
  }, [face.face]);

  if (!view || face.face === 'none') return <>{dialog}</>;
  if (face.face === 'unload') {
    return (
      <>
        <button type="button" className="ui-btn ui-btn-icon gpu-unload" aria-label={face.label} title={face.tooltip} data-testid="gpu-unload" onClick={askUnload}>
          <IconGpu size={16} />
        </button>
        {dialog}
      </>
    );
  }
  const openPopover = () => {
    // Nothing to offer while models are being turned back on.
    if (!face.lock) return;
    const r = pill.current?.getBoundingClientRect();
    if (r) setAnchor({ left: r.left, top: r.top, width: r.width, height: r.height });
  };
  return (
    <>
      <button
        ref={pill}
        type="button"
        className="gpu-pill"
        data-testid="gpu-locked"
        data-state={face.lock ?? 'on'}
        data-working={face.working || undefined}
        title={face.tooltip}
        onClick={openPopover}
      >
        {face.working && <WheelMark size={13} state="working" />}
        {face.label}
      </button>
      {anchor && (
        <AnchoredDialog
          title="GPU reserved"
          testId="gpu-popover"
          anchor={anchor}
          onClose={() => setAnchor(null)}
          footer={
            <>
              {face.keepOff && (
                <Button variant="secondary" data-testid="gpu-keep-off" disabled={turningOn} onClick={() => void keepOff()}>
                  Keep them off after the game
                </Button>
              )}
              <Button variant="primary" data-testid="gpu-reload" busy={turningOn} disabled={turningOn} onClick={() => void turnOn().then(() => setAnchor(null))}>
                Turn local models back on
              </Button>
            </>
          }
        >
          <p className="gpu-popover-lead">{lockSentence(view)}.</p>
          <p className="gpu-popover-lead">{comeBackSentence(view)}</p>
          {view.report.length > 0 && (
            <ul className="gpu-report" data-testid="gpu-report">
              {view.report.map((line, i) => (
                <li key={i}>{line}</li>
              ))}
            </ul>
          )}
        </AnchoredDialog>
      )}
      {dialog}
    </>
  );
}
