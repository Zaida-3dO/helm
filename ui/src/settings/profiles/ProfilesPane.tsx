// Settings → Profiles: the people who use this Helm. Add, rename, recolour, say how to address each
// one ("About you"), link to a person on the hub, delete (its chats go back to the home profile).
// The home profile is the one whose Claude sign-in this computer runs under; it cannot be deleted.
// Which devices may run chats here is under Settings → Devices; pairing is under Settings → Hub.

import { useEffect, useState } from 'react';
import type { ProfileSummary } from '../../api/profiles.ts';
import { PROFILE_COLOURS } from '../../profiles/model.ts';
import { ProfileDisc } from '../../profiles/Disc.tsx';
import { useHouseholdWatch, useProfiles } from '../../profiles/hooks.ts';
import { errorText, type ProfilesStore } from '../../profiles/store.ts';
import { useShell } from '../../shell/context.ts';
import { Button, ConfirmDialog, Picker, SettingRow, TextArea, TextInput, type ConfirmSpec } from '../../ui/index.ts';
import '../../profiles/profiles.css';
import './profilesPane.css';

const NOT_LINKED = '';
/** The longest About text the host keeps. */
const ABOUT_MAX = 2000;

function ProfileRow({ profile, homeName, store, people, onError, confirm }: {
  profile: ProfileSummary;
  /** The home profile's name, for what happens to a deleted profile's chats. */
  homeName: string;
  store: ProfilesStore;
  people: { id: string; name: string; you: boolean }[] | null;
  onError: (text: string | null) => void;
  confirm: (spec: ConfirmSpec) => void;
}) {
  const [name, setName] = useState(profile.name);
  const [about, setAbout] = useState(profile.about ?? '');
  const [busy, setBusy] = useState(false);
  // An edit made elsewhere (another window, the hub) replaces what is shown.
  useEffect(() => setAbout(profile.about ?? ''), [profile.about]);
  const change = (patch: { name?: string; colour?: string; hubPersonId?: string | null; about?: string }) => {
    setBusy(true);
    store.ops
      .update({ id: profile.id, ...patch })
      .then((v) => {
        store.apply(v);
        onError(null);
      })
      .catch((e: unknown) => {
        setName(profile.name);
        setAbout(profile.about ?? '');
        onError(errorText(e));
      })
      .finally(() => setBusy(false));
  };
  const commitName = () => {
    const n = name.trim();
    if (n && n !== profile.name) change({ name: n });
    else setName(profile.name);
  };
  const commitAbout = () => {
    if (about !== (profile.about ?? '')) change({ about });
  };
  const linkOptions = [
    { value: NOT_LINKED, title: 'Not linked', subtitle: 'Only labels chats on this computer' },
    ...(people ?? []).map((p) => ({ value: p.id, title: p.you ? `${p.name} (you)` : p.name })),
  ];
  if (profile.hubPersonId && !linkOptions.some((o) => o.value === profile.hubPersonId)) {
    linkOptions.push({ value: profile.hubPersonId, title: profile.hubPersonId, subtitle: 'The hub is not answering' });
  }
  return (
    <SettingRow
      testId={`profile-row-${profile.id}`}
      title={
        <>
          <ProfileDisc name={profile.name} colour={profile.colour} size={16} /> {profile.name}
          {profile.isHome && <span className="pf-badge">This computer</span>}
        </>
      }
      description={`${profile.chats} ${profile.chats === 1 ? 'chat' : 'chats'}`}
    >
      <div className="pf-pane-rows">
        <TextInput
          aria-label={`Name of ${profile.name}`}
          value={name}
          maxLength={40}
          disabled={busy}
          onChange={(e) => setName(e.target.value)}
          onBlur={commitName}
          onKeyDown={(e) => e.key === 'Enter' && e.currentTarget.blur()}
          data-testid="profile-name"
        />
        <div className="pf-swatches" role="group" aria-label={`Colour of ${profile.name}`}>
          {PROFILE_COLOURS.map((c) => (
            <button
              key={c}
              type="button"
              className="pf-swatch"
              style={{ background: c }}
              aria-label={c}
              aria-pressed={profile.colour === c}
              disabled={busy}
              onClick={() => profile.colour !== c && change({ colour: c })}
            />
          ))}
        </div>
        <label className="pf-about">
          <span className="pf-about-label">About you</span>
          <TextArea
            aria-label={`About ${profile.name}`}
            rows={3}
            maxLength={ABOUT_MAX}
            value={about}
            disabled={busy}
            placeholder="For example: call me Sam, keep answers short, I am new to git."
            onChange={(e) => setAbout(e.target.value)}
            onBlur={commitAbout}
            data-testid="profile-about"
          />
          <span className="setting-desc" data-testid="profile-about-hint">
            How to address you and write for you. Every chat on your profile gets this, on any device. {about.length}/{ABOUT_MAX}
          </span>
        </label>
        <Picker
          settings
          label={`Hub person for ${profile.name}`}
          testId="profile-link"
          value={profile.hubPersonId ?? NOT_LINKED}
          options={linkOptions}
          disabled={busy}
          onChange={(v) => change({ hubPersonId: v === NOT_LINKED ? null : v })}
        />
        {!profile.isHome && (
          <Button
            variant="text"
            danger
            disabled={busy}
            data-testid="profile-delete"
            onClick={() =>
              confirm({
                title: `Delete ${profile.name}?`,
                body: `Its ${profile.chats === 1 ? 'chat moves' : 'chats move'} to ${homeName}. Nothing is deleted from any chat.`,
                confirm: 'Delete profile',
                danger: true,
                onConfirm: () =>
                  store.ops
                    .remove(profile.id)
                    .then((v) => store.apply(v))
                    .catch((e: unknown) => onError(errorText(e))),
              })
            }
          >
            Delete
          </Button>
        )}
      </div>
    </SettingRow>
  );
}

export function ProfilesPane() {
  const shell = useShell();
  const { store, snap } = useProfiles(shell.api, shell.bus);
  useHouseholdWatch(store);
  const [newName, setNewName] = useState('');
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [confirm, setConfirm] = useState<ConfirmSpec | null>(null);
  const view = snap.view;
  const homeName = view?.profiles.find((p) => p.isHome)?.name ?? 'the home profile';

  const add = () => {
    const name = newName.trim();
    if (!name) return;
    setBusy(true);
    store.ops
      .create({ name })
      .then((v) => {
        store.apply(v);
        setNewName('');
        setError(null);
      })
      .catch((e: unknown) => setError(errorText(e)))
      .finally(() => setBusy(false));
  };

  return (
    <div className="hub-pane" data-testid="settings-profiles">
      <SettingRow
        title="Profiles"
        description="The people who use this Helm. A profile owns chats, and the sidebar shows one profile's chats and the ones shared with it. A chat runs on the computer it was started on, under that computer's Claude sign-in, whichever profile it belongs to."
      />
      {(view?.profiles ?? [])
        .slice()
        .sort((a, b) => Number(b.isHome) - Number(a.isHome))
        .map((p) => (
          <ProfileRow key={p.id} profile={p} homeName={homeName} store={store} people={snap.people} onError={setError} confirm={setConfirm} />
        ))}
      <div className="pf-pane-add">
        <TextInput
          aria-label="New profile name"
          placeholder={view?.homeId ? 'Add a person' : 'Your name'}
          value={newName}
          maxLength={40}
          onChange={(e) => setNewName(e.target.value)}
          onKeyDown={(e) => e.key === 'Enter' && add()}
          data-testid="profile-new-name"
        />
        <Button variant="primary" disabled={busy || !newName.trim()} onClick={add} data-testid="profile-add">
          {view?.homeId ? 'Add profile' : 'Create profile'}
        </Button>
      </div>
      {error && (
        <div className="ui-field-error" role="alert" data-testid="profiles-error">
          {error}
        </div>
      )}
      {confirm && <ConfirmDialog spec={confirm} onClose={() => setConfirm(null)} />}
    </div>
  );
}
