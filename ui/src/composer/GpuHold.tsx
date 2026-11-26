// The graphics-card hold in a chat's composer. While a chat has said the card is in use, a composer
// of a chat on a local model shows why above the box and asks once before the first send, because
// that send would reload the model server. Chats on Claude models never read it.

import { useCallback, useEffect, useRef, useState, type ReactNode } from 'react';
import { useShell } from '../shell/context.ts';
import { ConfirmDialog, type ConfirmSpec } from '../ui/index.ts';
import { IconWarning } from '../ui/icons.tsx';
import { gpuOps, type LockView } from '../gpu/api.ts';
import { activeHold, holdSentence, type GpuHold } from './gpuHold.ts';
import './gpuHold.css';

export interface GpuHoldGate {
  /** The hold in force for this composer, or null (also for a chat the hold does not concern). */
  hold: GpuHold | null;
  /** Resolves true when the send may go: no hold, the hold was already agreed to, or the user agrees now. */
  confirmSend: () => Promise<boolean>;
  /** The banner and the question, drawn once in the composer. */
  notice: ReactNode;
}

/** `applies` is true for a chat that runs on a local model on this PC. */
export function useGpuHold(applies: boolean): GpuHoldGate {
  const shell = useShell();
  const [view, setView] = useState<LockView | null>(null);
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    if (!applies) {
      setView(null);
      return;
    }
    const ops = gpuOps(shell.api);
    let live = true;
    const read = () =>
      void ops
        .state()
        .then((v) => {
          if (!live) return;
          setView(v);
          setNow(Date.now());
        })
        .catch(() => undefined);
    read();
    const off = shell.bus.on('local_lock', read);
    return () => {
      live = false;
      off();
    };
  }, [shell, applies]);
  const hold = applies ? activeHold(view, now) : null;
  // The banner goes away by itself when the hold runs out.
  const until = hold?.until;
  useEffect(() => {
    if (until === undefined) return;
    const t = setTimeout(() => setNow(Date.now()), Math.max(0, until - Date.now()) + 50);
    return () => clearTimeout(t);
  }, [until]);

  // The send callback is built once per chat, so it reads the hold through refs.
  const holdRef = useRef<GpuHold | null>(null);
  holdRef.current = hold;
  const agreed = useRef<number | null>(null);
  const waiting = useRef<((ok: boolean) => void) | null>(null);
  const [spec, setSpec] = useState<ConfirmSpec | null>(null);
  const finish = useCallback((ok: boolean) => {
    const done = waiting.current;
    waiting.current = null;
    setSpec(null);
    done?.(ok);
  }, []);
  const confirmSend = useCallback((): Promise<boolean> => {
    const h = holdRef.current;
    if (!h || agreed.current === h.until) return Promise.resolve(true);
    // A second press while the question is open does nothing.
    if (waiting.current) return Promise.resolve(false);
    return new Promise<boolean>((resolve) => {
      waiting.current = resolve;
      setSpec({
        title: 'Send and reload the local model?',
        body: holdSentence(h),
        confirm: 'Send anyway',
        onConfirm: () => {
          agreed.current = h.until;
          finish(true);
        },
      });
    });
  }, [finish]);

  const notice = (
    <>
      {hold && (
        <div className="composer-gpu-hold" role="status" data-testid="composer-gpu-hold">
          <IconWarning size={16} />
          <span>{holdSentence(hold)}</span>
        </div>
      )}
      {spec && <ConfirmDialog spec={spec} onClose={() => finish(false)} />}
    </>
  );
  return { hold, confirmSend, notice };
}
