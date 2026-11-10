// The sidebar's profile item, directly above Settings: the active profile's disc and name. A click
// opens the profile screen. While the home profile still has its placeholder name the item asks
// "Who uses this Helm?" and opens the first-run form instead: the person's name, and what this PC
// is called.

import { useEffect, useState, type FormEvent } from 'react';
import type { Api } from '../api/client.ts';
import type { EventBus } from '../api/events.ts';
import { SETTINGS_DIALOG } from '../settings/SettingsDialog.tsx';
import { requestPane } from '../settings/registry.ts';
import { useShell } from '../shell/context.ts';
import { deviceOps } from '../devices/api.ts';
import { Button, Dialog, TextInput } from '../ui/index.ts';
import { ProfileDisc } from './Disc.tsx';
import { useProfiles } from './hooks.ts';
import { itemText, needsFirstRun } from './model.ts';
import { ProfileScreen } from './ProfileScreen.tsx';
import { errorText, type ProfilesStore } from './store.ts';
import './profiles.css';

/** The Settings pane "Manage profiles" opens. */
export const PROFILES_PANE = 'profiles';

const NAME_MAX = 40;

export function ProfileItem({ api, bus }: { api: Api; bus: EventBus }) {
  const shell = useShell();
  const { store, snap } = useProfiles(api, bus);
  const [screen, setScreen] = useState(false);
  const [firstRun, setFirstRun] = useState(false);
  const view = snap.view;
  if (!snap.loaded || !view) return null;

  const asking = needsFirstRun(view);
  const active = view.profiles.find((p) => p.id === view.activeId) ?? view.profiles.find((p) => p.id === view.homeId) ?? null;
  const text = itemText(view);
  const manage = () => {
    setScreen(false);
    requestPane(PROFILES_PANE);
    shell.openDialog(SETTINGS_DIALOG);
  };

  return (
    <>
      <button
        type="button"
        className="sb-foot sb-profile"
        data-testid="profile-item"
        data-needs-name={asking ? 'true' : undefined}
        aria-haspopup="dialog"
        title={asking ? 'Tell Helm who you are' : 'Choose a profile'}
        onClick={() => (asking ? setFirstRun(true) : setScreen(true))}
      >
        {active && !asking ? <ProfileDisc name={active.name} colour={active.colour} size={22} /> : <ProfileDisc name="?" colour="#90a4ae" size={22} />}
        <span data-testid="profile-item-name">{text}</span>
      </button>
      {screen && <ProfileScreen api={api} bus={bus} onClose={() => setScreen(false)} onManage={manage} />}
      {firstRun && <FirstRun api={api} store={store} onClose={() => setFirstRun(false)} />}
    </>
  );
}

/** The first-run question: the person's name, and this PC's name (prefilled from the host). */
function FirstRun({ api, store, onClose }: { api: Api; store: ProfilesStore; onClose: () => void }) {
  const view = store.snapshot.view;
  const home = view?.profiles.find((p) => p.isHome) ?? null;
  const ops = deviceOps(api);
  const [who, setWho] = useState(home && home.needsName !== true ? home.name : '');
  const [device, setDevice] = useState('');
  const [deviceWas, setDeviceWas] = useState('');
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let live = true;
    ops
      .self()
      .then((d) => {
        if (!live) return;
        setDevice((cur) => cur || d.name);
        setDeviceWas(d.name);
      })
      .catch(() => undefined);
    return () => {
      live = false;
    };
    // The device name is read once when the form opens.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  const save = async (e?: FormEvent) => {
    e?.preventDefault();
    const name = who.trim();
    const pc = device.trim();
    if (!name || busy) return;
    setBusy(true);
    setError(null);
    try {
      const v = home ? await store.ops.update({ id: home.id, name }) : await store.ops.create({ name });
      store.apply(v);
      if (pc && pc !== deviceWas) await ops.rename(pc);
      onClose();
    } catch (err) {
      setError(errorText(err));
    } finally {
      setBusy(false);
    }
  };

  return (
    <Dialog
      title="Who uses this Helm?"
      onClose={onClose}
      busy={busy}
      padded
      testId="profile-first-run"
      footer={
        <>
          <Button variant="secondary" onClick={onClose} disabled={busy}>
            Later
          </Button>
          <Button variant="primary" onClick={() => void save()} disabled={busy || !who.trim()} busy={busy} data-testid="profile-first-run-save">
            Save
          </Button>
        </>
      }
    >
      <form className="pf-first" onSubmit={(e) => void save(e)}>
        <label>
          <span>Your name</span>
          <TextInput autoFocus value={who} maxLength={NAME_MAX} placeholder="Your name" autoComplete="off" onChange={(e) => setWho(e.target.value)} data-testid="profile-first-run-name" />
        </label>
        <label>
          <span>This PC is called</span>
          <TextInput value={device} maxLength={NAME_MAX} autoComplete="off" onChange={(e) => setDevice(e.target.value)} data-testid="profile-first-run-device" />
        </label>
        {error && (
          <div className="pf-error" role="alert">
            {error}
          </div>
        )}
      </form>
    </Dialog>
  );
}
