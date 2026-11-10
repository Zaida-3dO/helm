// Settings → Profiles: the people who use this Helm. Add, rename, recolour, link to a person on the
// hub, delete (its chats go back to the home profile). The home profile is the one whose Claude
// sign-in this computer runs under; it cannot be deleted. What this Helm shares with the household
// is under Settings → Hub.

import { useState } from 'react';
import type { ProfileSummary } from '../../api/profiles.ts';
import { PROFILE_COLOURS } from '../../profiles/model.ts';
import { ProfileDisc } from '../../profiles/Disc.tsx';
import { useHouseholdWatch, useProfiles } from '../../profiles/hooks.ts';
import { errorText, type ProfilesStore } from '../../profiles/store.ts';
import { useShell } from '../../shell/context.ts';
import { Button, ConfirmDialog, Picker, SettingRow, TextInput, type ConfirmSpec } from '../../ui/index.ts';
import '../../profiles/profiles.css';

const NOT_LINKED = '';

function ProfileRow({ profile, store, people, onError, confirm }: {
  profile: ProfileSummary;
  store: ProfilesStore;
  people: { id: string; name: string; you: boolean }[] | null;
  onError: (text: string | null) => void;
  confirm: (spec: ConfirmSpec) => void;
}) {
  const [name, setName] = useState(profile.name);
  const [busy, setBusy] = useState(false);
  const change = (patch: { name?: string; colour?: string; hubPersonId?: string | null }) => {
    setBusy(true);
    store.ops
      .update({ id: profile.id, ...patch })
      .then((v) => {
        store.apply(v);
        onError(null);
      })
      .catch((e: unknown) => {
        setName(profile.name);
        onError(errorText(e));
      })
      .finally(() => setBusy(false));
  };
  const commitName = () => {
    const n = name.trim();
    if (n && n !== profile.name) change({ name: n });
    else setName(profile.name);
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
                body: `Its ${profile.chats === 1 ? 'chat goes' : 'chats go'} back to the home profile. Nothing is deleted from any chat.`,
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
        description="The people who use this Helm. A profile is a label on chats and a way to look at them one person at a time. Every chat here runs on this computer's Claude sign-in, whichever profile it is labelled with."
      />
      {(view?.profiles ?? [])
        .slice()
        .sort((a, b) => Number(b.isHome) - Number(a.isHome))
        .map((p) => (
          <ProfileRow key={p.id} profile={p} store={store} people={snap.people} onError={setError} confirm={setConfirm} />
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
