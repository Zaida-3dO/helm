// The sidebar footer's profile chip: a disc and "<Name> · <Plan>" for the profile the sidebar shows,
// or the stacked discs and "All profiles". A click opens the switcher. On an install with no
// profile yet the chip is a small inline question instead ("Who uses this Helm?"), never a dialog,
// so nothing is blocked until it is answered.

import { useRef, useState, type FormEvent, type ReactNode } from 'react';
import type { Api } from '../api/client.ts';
import type { EventBus } from '../api/events.ts';
import type { ProfileSummary } from '../api/profiles.ts';
import { SETTINGS_DIALOG } from '../settings/SettingsDialog.tsx';
import { requestPane } from '../settings/registry.ts';
import { useShell } from '../shell/context.ts';
import { Button, ContextMenu, TextInput, type MenuItem } from '../ui/index.ts';
import { useUsageStore } from '../usage/UsageMeter.tsx';
import { AllDiscs, ProfileDisc } from './Disc.tsx';
import { useProfiles } from './hooks.ts';
import { chipText, offlineText, personFor, planText } from './model.ts';
import { errorText } from './store.ts';
import { ProfileView } from './ProfileView.tsx';
import './profiles.css';

export const PROFILES_PANE = 'profiles';

function ProfileEntry({ profile, sub }: { profile: ProfileSummary; sub: ReactNode }) {
  return (
    <span className="pf-entry">
      <ProfileDisc name={profile.name} colour={profile.colour} />
      <span className="pf-entry-text">
        <span className="pf-entry-name">{profile.name}</span>
        <span className="pf-entry-sub">{sub}</span>
      </span>
    </span>
  );
}

export function ProfileChip({ api, bus }: { api: Api; bus: EventBus }) {
  const shell = useShell();
  const { store, snap } = useProfiles(api, bus);
  const { snap: usage } = useUsageStore();
  const [menu, setMenu] = useState<{ x: number; y: number } | null>(null);
  const [viewing, setViewing] = useState<string | null>(null);
  const [name, setName] = useState('');
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const button = useRef<HTMLButtonElement>(null);

  const view = snap.view;
  if (!snap.loaded || !view) return null;

  if (view.homeId === null) {
    const submit = (e: FormEvent) => {
      e.preventDefault();
      if (!name.trim() || busy) return;
      setBusy(true);
      store.ops
        .create({ name: name.trim() })
        .then((v) => {
          store.apply(v);
          setError(null);
        })
        .catch((err: unknown) => setError(errorText(err)))
        .finally(() => setBusy(false));
    };
    return (
      <form className="pf-first" onSubmit={submit} data-testid="profile-first-run">
        <label className="pf-first-q" htmlFor="pf-first-name">
          Who uses this Helm?
        </label>
        <div className="pf-first-row">
          <TextInput id="pf-first-name" value={name} maxLength={40} placeholder="Your name" autoComplete="off" onChange={(e) => setName(e.target.value)} data-testid="profile-first-name" />
          <Button type="submit" variant="secondary" disabled={busy || !name.trim()} data-testid="profile-first-save">
            Save
          </Button>
        </div>
        {error && <div className="pf-first-error">{error}</div>}
      </form>
    );
  }

  const plan = planText(usage?.reading?.plan);
  const active = view.activeId === null ? null : (view.profiles.find((p) => p.id === view.activeId) ?? null);
  const text = chipText(view, plan);
  const manage = () => {
    requestPane(PROFILES_PANE);
    shell.openDialog(SETTINGS_DIALOG);
  };
  const choose = (id: string | null) =>
    void store.ops
      .setActive(id)
      .then((v) => store.apply(v))
      .catch((e: unknown) => setError(errorText(e)));

  const open = () => {
    const r = button.current?.getBoundingClientRect();
    setMenu({ x: r?.left ?? 0, y: r?.top ?? 0 });
    void store.refreshHousehold();
  };
  const now = Date.now();
  const ordered = [...view.profiles].sort((a, b) => Number(b.isHome) - Number(a.isHome));
  const items: MenuItem[] = [
    ...ordered.map((p): MenuItem => {
      const person = personFor(p, snap.people);
      const machines = person && !person.you ? person.machines : [];
      return {
        id: `profile:${p.id}`,
        checked: view.activeId === p.id,
        label: (
          <ProfileEntry
            profile={p}
            sub={
              <>
                {p.chats} {p.chats === 1 ? 'chat' : 'chats'}
                {p.running > 0 ? ` · ${p.running} running` : ''}
                {p.attention > 0 ? ` · ${p.attention} need you` : ''}
                {machines.map((m) => (
                  <span key={m.id} className="pf-machine" title={offlineText(m, now)}>
                    <span className="pf-dot" data-online={m.online ? 'true' : 'false'} aria-hidden="true" />
                    {m.name}
                  </span>
                ))}
              </>
            }
          />
        ),
        onSelect: () => choose(p.id),
      };
    }),
    {
      id: 'profile:all',
      separator: true,
      checked: view.activeId === null,
      label: (
        <span className="pf-entry">
          <AllDiscs />
          <span className="pf-entry-text">
            <span className="pf-entry-name">All profiles</span>
          </span>
        </span>
      ),
      onSelect: () => choose(null),
    },
    { id: 'profile:view', separator: true, label: 'Profile view…', onSelect: () => setViewing(view.activeId ?? view.homeId) },
    { id: 'profile:manage', label: 'Manage profiles…', onSelect: manage },
  ];

  return (
    <>
      <button ref={button} type="button" className="sb-foot pf-chip" onClick={open} aria-haspopup="menu" aria-label={`Profile: ${text}`} title="Switch profile" data-testid="profile-chip">
        {active ? <ProfileDisc name={active.name} colour={active.colour} /> : <AllDiscs />}
        <span data-testid="profile-chip-text">{text}</span>
      </button>
      {error && <div className="pf-first-error">{error}</div>}
      {menu && <ContextMenu x={menu.x} y={menu.y} items={items} label="Profiles" onClose={() => setMenu(null)} testId="profile-menu" />}
      {viewing && <ProfileView api={api} bus={bus} profileId={viewing} onClose={() => setViewing(null)} manage={() => (setViewing(null), manage())} />}
    </>
  );
}
