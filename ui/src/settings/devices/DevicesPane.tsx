// Settings → Devices: this PC's name, whether other devices may run chats on it, and the other
// devices on the household hub. A chat always runs on the device it was started on, under that
// device's Claude sign-in and plan; the switches here decide whether another device, and whether
// someone else in the household, may start and steer chats on this PC. Reading chats needs only
// pairing, not these switches.

import { useCallback, useEffect, useMemo, useState } from 'react';
import { useShell } from '../../shell/context.ts';
import { Button, Checkbox, SettingRow, TextInput } from '../../ui/index.ts';
import { devicesOps, HOUSEHOLD_RUN_KEY, RUN_FROM_OTHERS_KEY, type DeviceRow } from './api.ts';
import { nameOk, NAME_MAX, otherDevices, presence } from './model.ts';
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
  const [householdRun, setHouseholdRun] = useState(true);
  const [others, setOthers] = useState<DeviceRow[]>([]);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [now, setNow] = useState(() => Date.now());

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
    const readHousehold = () =>
      ops
        .householdRun()
        .then(setHouseholdRun)
        .catch(() => undefined);
    void readRun();
    void readHousehold();
    return shell.bus.on('preferences', (e) => {
      const changed = (e as { changed?: object }).changed ?? {};
      if (RUN_FROM_OTHERS_KEY in changed) void readRun();
      if (HOUSEHOLD_RUN_KEY in changed) void readHousehold();
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

  const setHousehold = (on: boolean) => {
    const before = householdRun;
    setHouseholdRun(on);
    setError(null);
    ops.setHouseholdRun(on).catch((e: unknown) => {
      setHouseholdRun(before);
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
        description="They run here, on this PC's Claude sign-in and plan."
      >
        <Checkbox checked={runFromOthers} aria-label="Let other devices use chats on this PC" data-testid="device-run-switch" onChange={setRun} />
      </SettingRow>
      <SettingRow
        title="Let people in my household run their chats on this PC"
        description="Their chats run here, on this PC's Claude sign-in and plan."
      >
        <Checkbox
          checked={householdRun}
          aria-label="Let people in my household run their chats on this PC"
          data-testid="device-household-run-switch"
          onChange={setHousehold}
        />
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
            description={`${d.person ?? 'Unknown person'} · ${presence(d, now)}`}
          >
            <span className="dv-run" data-testid={`device-run-${d.uid}`}>
              Can run chats there: {d.run ? 'yes' : 'no'}
            </span>
          </SettingRow>
        ))
      )}
    </div>
  );
}
