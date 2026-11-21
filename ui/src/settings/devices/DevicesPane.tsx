// Settings → Devices: this PC's name, whether other devices may run chats on it, and the other
// devices on the household hub. A chat runs on its home PC — the PC it runs on — under that PC's
// Claude sign-in and plan; the switch here decides whether a chat started on another device, yours
// or someone else's in the household, may take this PC as its home PC. Reading chats needs only
// pairing, not this switch. Below them, the ports of other PCs forwarded to this one (people's
// seats), and the ports of this PC that seats on other PCs have open — each list with Stop.

import { useCallback, useEffect, useMemo, useState } from 'react';
import { useShell } from '../../shell/context.ts';
import { Button, Checkbox, SettingRow, TextInput } from '../../ui/index.ts';
import { openForward, useForwards, useHostedForwards } from '../../devices/RemoteChatBar.tsx';
import { devicesOps, RUN_FROM_OTHERS_KEY, type DeviceRow } from './api.ts';
import { forwardRows, hostedRows, nameOk, NAME_MAX, otherDevices, peopleLine, presence } from './model.ts';
import './devices.css';

function errorText(e: unknown): string {
  return String((e as { message?: unknown })?.message ?? e);
}

export function DevicesPane() {
  const shell = useShell();
  const ops = useMemo(() => devicesOps(shell.api), [shell.api]);
  const [savedName, setSavedName] = useState<string | null>(null);
  const [name, setName] = useState('');
  const [runFromOthers, setRunFromOthers] = useState(true);
  const [others, setOthers] = useState<DeviceRow[]>([]);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [now, setNow] = useState(() => Date.now());
  const forwards = useForwards();
  const hosted = useHostedForwards();

  const loadSelf = useCallback(
    () =>
      ops
        .self()
        .then((d) => {
          setSavedName(d.name);
          setName(d.name);
        })
        .catch((e: unknown) => setError(errorText(e))),
    [ops],
  );

  // Without a paired hub the list is simply empty.
  const loadOthers = useCallback(
    () =>
      ops
        .list()
        .then((l) => {
          setOthers(otherDevices(l.devices));
          setNow(Date.now());
        })
        .catch(() => setOthers([])),
    [ops],
  );

  useEffect(() => {
    void loadSelf();
    void loadOthers();
    const offs = [shell.bus.on('device_changed', () => void loadSelf()), shell.bus.on('devices_changed', () => void loadOthers())];
    return () => offs.forEach((off) => off());
  }, [shell, loadSelf, loadOthers]);

  useEffect(() => {
    const readRun = () =>
      ops
        .runFromOthers()
        .then(setRunFromOthers)
        .catch(() => undefined);
    void readRun();
    return shell.bus.on('preferences', (e) => {
      const changed = (e as { changed?: object }).changed ?? {};
      if (RUN_FROM_OTHERS_KEY in changed) void readRun();
    });
  }, [shell, ops]);

  const saveName = () => {
    const n = name.trim();
    if (!nameOk(n) || n === savedName) return;
    setBusy(true);
    setError(null);
    ops
      .rename(n)
      .then((d) => {
        setSavedName(d.name);
        setName(d.name);
      })
      .catch((e: unknown) => setError(errorText(e)))
      .finally(() => setBusy(false));
  };

  const setRun = (on: boolean) => {
    const before = runFromOthers;
    setRunFromOthers(on);
    setError(null);
    ops.setRunFromOthers(on).catch((e: unknown) => {
      setRunFromOthers(before);
      setError(errorText(e));
    });
  };

  return (
    <div className="hub-pane" data-testid="settings-devices">
      <h3 className="hub-heading">This PC</h3>
      <SettingRow title="Name" description="How this PC shows up on your other devices.">
        <div className="dv-name">
          <TextInput
            aria-label="Name of this PC"
            value={name}
            maxLength={NAME_MAX}
            disabled={busy || savedName === null}
            onChange={(e) => setName(e.target.value)}
            onKeyDown={(e) => e.key === 'Enter' && saveName()}
            data-testid="device-name"
          />
          <Button variant="primary" disabled={busy || !nameOk(name) || name.trim() === savedName} data-testid="device-name-save" onClick={saveName}>
            Save
          </Button>
        </div>
      </SettingRow>
      <SettingRow
        title="Let other devices use chats on this PC"
        description="A chat always runs on one PC, its home PC. This lets a chat started on your other devices take this PC as its home PC, on this PC's Claude sign-in and plan."
      >
        <Checkbox checked={runFromOthers} aria-label="Let other devices use chats on this PC" data-testid="device-run-switch" onChange={setRun} />
      </SettingRow>
      {error && (
        <div className="ui-field-error" role="alert" data-testid="devices-error">
          {error}
        </div>
      )}

      <h3 className="hub-heading">Other devices</h3>
      {others.length === 0 ? (
        <div className="setting-desc" data-testid="devices-empty">
          Pair this PC with your hub (Settings → Hub) to see your other devices.
        </div>
      ) : (
        others.map((d) => (
          <SettingRow
            key={d.uid}
            testId={`device-row-${d.uid}`}
            title={
              <>
                <span className={`dv-dot${d.online ? ' on' : ''}`} aria-hidden="true" /> {d.name}
              </>
            }
            description={`Acts for: ${peopleLine(d)} · ${presence(d, now)}`}
          >
            <span className="dv-run" data-testid={`device-run-${d.uid}`}>
              Can run chats there: {d.run ? 'yes' : 'no'}
            </span>
          </SettingRow>
        ))
      )}

      <h3 className="hub-heading">Ports forwarded to this PC</h3>
      {forwards.length === 0 ? (
        <div className="setting-desc" data-testid="forwards-empty">
          None. An agent can offer one, or use "Forward a port…" on a chat whose home PC is another PC. A forward ends when
          either PC closes Helm, or after 30 minutes unused.
        </div>
      ) : (
        forwardRows(forwards, Date.now()).map(({ forward: f, line, use }) => (
          <SettingRow key={f.forwardId} testId={`forward-row-${f.forwardId}`} title={line} description={use}>
            <div className="dv-forward-actions">
              <Button data-testid={`forward-open-${f.forwardId}`} onClick={() => openForward(f, null)}>
                Open
              </Button>
              <Button data-testid={`forward-stop-${f.forwardId}`} onClick={() => void ops.forwardClose(f.forwardId).catch((e: unknown) => setError(errorText(e)))}>
                Stop
              </Button>
            </div>
          </SettingRow>
        ))
      )}

      <h3 className="hub-heading">Ports this PC shares with other PCs</h3>
      {hosted.length === 0 ? (
        <div className="setting-desc" data-testid="forward-hosted-empty">
          None. A chat running on this PC can offer its person, typing on another PC (their seat), a port of this PC to
          open (a dev server, say); while they do, the port shows here. Stop ends it at their PC too. A share ends when
          either PC closes Helm, or after 30 minutes unused.
        </div>
      ) : (
        hostedRows(hosted, Date.now()).map(({ forward: f, line, use }) => (
          <SettingRow key={`hosted-${f.forwardId}`} testId={`forward-hosted-row-${f.forwardId}`} title={line} description={use}>
            <Button data-testid={`forward-hosted-stop-${f.forwardId}`} onClick={() => void ops.forwardHostedStop(f.forwardId).catch((e: unknown) => setError(errorText(e)))}>
              Stop
            </Button>
          </SettingRow>
        ))
      )}
    </div>
  );
}
