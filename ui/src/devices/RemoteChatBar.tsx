// The strip at the top of a chat that runs on another device: where it runs and, when it cannot be
// changed from here, why. Mounted by ChatView for a chat whose host is not this PC.
//
// It also lists the ports of that PC forwarded to this one for the chat, each with Open (this
// PC's browser, at a loopback address built here) and Stop, and offers "Forward a port…" while the
// device is live.

import { useEffect, useMemo, useState } from 'react';
import { handleLink } from '../panel/links.ts';
import { forwardRows } from '../settings/devices/model.ts';
import { useShell } from '../shell/context.ts';
import { toast } from '../ui/index.ts';
import { IconMonitor } from '../ui/icons.tsx';
import { deviceOps, forwardAddress, FORWARDS_CHANGED, type Forward } from './api.ts';
import type { RemoteChatState } from './state.ts';
import './remote.css';

function errorText(e: unknown): string {
  return e instanceof Error ? e.message : String((e as { message?: unknown })?.message ?? e);
}

/** A port number someone typed, or null when it is not one. */
export function typedPort(text: string): number | null {
  const t = text.trim();
  if (!/^\d{1,5}$/.test(t)) return null;
  const n = Number(t);
  return n >= 1 && n <= 65535 ? n : null;
}

/** The ports forwarded to this PC, kept fresh from the host's event. */
export function useForwards(): Forward[] {
  const shell = useShell();
  const ops = useMemo(() => deviceOps(shell.api), [shell.api]);
  const [forwards, setForwards] = useState<Forward[]>([]);
  useEffect(() => {
    let live = true;
    const load = () =>
      ops.forwards().then(
        (f) => live && setForwards(f),
        () => live && setForwards([]),
      );
    void load();
    const off = shell.bus.on(FORWARDS_CHANGED, () => void load());
    return () => {
      live = false;
      off();
    };
  }, [ops, shell.bus]);
  return forwards;
}

/** Opens a forward in this PC's browser, through the link gate like any web address. */
export function openForward(f: Forward, chatId: string | null): void {
  const href = forwardAddress(f);
  if (href) void handleLink({ kind: 'url', href }, { chatId }, { skipWebRoutes: true });
}

export function RemoteChatBar({ state, chatId }: { state: RemoteChatState; chatId: string }) {
  const shell = useShell();
  const ops = useMemo(() => deviceOps(shell.api), [shell.api]);
  const forwards = useForwards();
  const rows = forwardRows(forwards, Date.now(), chatId);
  const [asking, setAsking] = useState(false);
  const [port, setPort] = useState('');
  const [busy, setBusy] = useState(false);
  const wanted = typedPort(port);

  const forward = () => {
    if (wanted === null || busy) return;
    setBusy(true);
    ops
      .forwardOpen({ chatId, port: wanted })
      .then(() => {
        setAsking(false);
        setPort('');
      })
      .catch((e: unknown) => toast.show({ kind: 'error', text: `Port ${wanted} was not forwarded: ${errorText(e)}` }))
      .finally(() => setBusy(false));
  };
  const stop = (f: Forward) => void ops.forwardClose(f.forwardId).catch((e: unknown) => toast.show({ kind: 'error', text: errorText(e) }));

  return (
    <div className="remote-bar" data-testid="remote-chat-bar" data-state={state.state} data-tone={state.tone}>
      <span className="remote-bar-glyph" aria-hidden="true">
        <IconMonitor size={12} />
      </span>
      <span className="remote-bar-device">Runs on {state.deviceName}</span>
      {state.text && (
        <span className="remote-bar-state" data-testid="remote-chat-state" title={state.text}>
          {state.text}
        </span>
      )}
      <span className="remote-bar-forwards">
        {rows.map(({ forward: f, line, use }) => (
          <span key={f.forwardId} className="remote-fwd" data-testid="remote-forward" data-port={f.port} title={`${line} · ${use}`}>
            <span className="remote-fwd-port">
              :{f.port}
              {f.localPort !== f.port && ` → :${f.localPort}`}
            </span>
            <button type="button" className="remote-fwd-btn" data-testid="remote-forward-open" title={`Open 127.0.0.1:${f.localPort} in the browser`} onClick={() => openForward(f, chatId)}>
              Open
            </button>
            <button type="button" className="remote-fwd-btn" data-testid="remote-forward-stop" title="Stop forwarding this port" onClick={() => stop(f)}>
              Stop
            </button>
          </span>
        ))}
        {state.live &&
          (asking ? (
            <span className="remote-fwd-ask">
              <input
                className="remote-fwd-input"
                data-testid="remote-forward-port"
                aria-label={`Port on ${state.deviceName}`}
                inputMode="numeric"
                placeholder="Port"
                value={port}
                autoFocus
                onChange={(e) => setPort(e.target.value)}
                onKeyDown={(e) => {
                  if (e.key === 'Enter') forward();
                  if (e.key === 'Escape') {
                    e.stopPropagation();
                    setAsking(false);
                  }
                }}
              />
              <button type="button" className="remote-fwd-btn" data-testid="remote-forward-go" disabled={wanted === null || busy} onClick={forward}>
                Forward
              </button>
              <button type="button" className="remote-fwd-btn" onClick={() => setAsking(false)}>
                Cancel
              </button>
            </span>
          ) : (
            <button
              type="button"
              className="remote-fwd-btn"
              data-testid="remote-forward-add"
              title={`Open a port of ${state.deviceName} (a dev server, say) on this PC`}
              onClick={() => setAsking(true)}
            >
              Forward a port…
            </button>
          ))}
      </span>
    </div>
  );
}
