// Settings → Hub: "Pair with a code". The person who runs the hub makes a one-time code for this
// computer; typing it here with the hub's address connects this Helm as that person on this
// computer: the host stores the token (sealed, never shown again), sets up the hub line and the
// shared-memory server in one step, and links the home profile to the person. Entering a token by
// hand stays available in the fields above. Whether other devices may run chats on this PC is under
// Settings → Devices.

import { useEffect, useMemo, useState } from 'react';
import { hubOps, type HubConnection } from '../../api/hub.ts';
import { useShell } from '../../shell/context.ts';
import { Button, SettingRow, TextInput } from '../../ui/index.ts';
import '../../profiles/profiles.css';

const POLL_MS = 4000;

function errorText(e: unknown): string {
  return String((e as { message?: unknown })?.message ?? e);
}

export function HubPairing() {
  const shell = useShell();
  const ops = useMemo(() => hubOps(shell.api), [shell.api]);
  const [conn, setConn] = useState<HubConnection | null>(null);
  const [address, setAddress] = useState('');
  const [fallback, setFallback] = useState('');
  const [code, setCode] = useState('');
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [done, setDone] = useState<string | null>(null);

  useEffect(() => {
    let live = true;
    const read = () =>
      void ops
        .connection()
        .then((c) => live && setConn(c))
        .catch(() => undefined);
    read();
    const t = setInterval(read, POLL_MS);
    return () => {
      live = false;
      clearInterval(t);
    };
  }, [ops]);

  const paired = conn?.personName ? `Connected as ${conn.personName}${conn.machineName ? ` on ${conn.machineName}` : ''}` : null;
  const ready = address.trim() !== '' && code.trim().length >= 4;

  const pair = async () => {
    setBusy(true);
    try {
      const r = await ops.pair({
        address: address.trim(),
        ...(fallback.trim() ? { fallback: fallback.trim() } : {}),
        code: code.trim(),
      });
      setConn(r.connection);
      setDone(`Connected as ${r.personName} on ${r.machineName}`);
      setCode('');
      setAddress('');
      setFallback('');
      setError(null);
    } catch (e) {
      setDone(null);
      setError(errorText(e));
    } finally {
      setBusy(false);
    }
  };

  return (
    <div data-testid="hub-pairing">
      <h3 className="hub-heading">Pair with a code</h3>
      {(done ?? paired) && (
        <SettingRow testId="hub-paired" title={done ?? paired} description="Shared memory and the hub line are set up; no token to paste." />
      )}
      <SettingRow
        title="Hub address"
        description="The hub's address on the home network, for pairing. The same kind of address as the connection above."
      >
        <TextInput
          autoComplete="off"
          spellCheck={false}
          aria-label="Hub address to pair with"
          placeholder="ws://192.168.1.20:8000"
          value={address}
          onChange={(e) => setAddress(e.target.value)}
          data-testid="hub-pair-address"
        />
      </SettingRow>
      <SettingRow title="Fallback address" description="Optional. Tried when the first address does not answer, for example its Tailscale address.">
        <TextInput
          autoComplete="off"
          spellCheck={false}
          aria-label="Fallback address to pair with"
          placeholder="ws://100.64.0.20:8000"
          value={fallback}
          onChange={(e) => setFallback(e.target.value)}
          data-testid="hub-pair-fallback"
        />
      </SettingRow>
      <SettingRow
        title="Pairing code"
        description="The one-time code from whoever runs the hub. It works once and runs out after 15 minutes."
      >
        <span className="pf-pair-code">
          <TextInput
            autoComplete="off"
            spellCheck={false}
            aria-label="Pairing code"
            placeholder="ABCD2345"
            maxLength={16}
            value={code}
            onChange={(e) => setCode(e.target.value)}
            onKeyDown={(e) => e.key === 'Enter' && ready && !busy && void pair()}
            data-testid="hub-pair-code"
          />
        </span>
      </SettingRow>
      <div className="hub-actions">
        <Button variant="primary" busy={busy} disabled={!ready} data-testid="hub-pair" onClick={() => void pair()}>
          {paired ? 'Pair again' : 'Pair'}
        </Button>
      </div>
      {error && (
        <div className="ui-field-error" role="alert" data-testid="hub-pair-error">
          {error}
        </div>
      )}
    </div>
  );
}
