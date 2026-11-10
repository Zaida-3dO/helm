// The profile view: who a profile is, where its chats run, how busy it is, the pages pinned for it,
// and (for the home profile) what this Helm shares with the household.

import { useEffect, useMemo, useState } from 'react';
import type { Api } from '../api/client.ts';
import type { EventBus } from '../api/events.ts';
import { Button, Dialog } from '../ui/index.ts';
import { pageOps, type PinnedPage } from '../pages/api.ts';
import { ProfileDisc } from './Disc.tsx';
import { useHouseholdWatch, useProfiles } from './hooks.ts';
import { offlineText, personFor, SIGN_IN_NOTE } from './model.ts';
import { SharingSwitches } from './SharingSwitches.tsx';

export interface ProfileViewProps {
  api: Api;
  bus: EventBus;
  profileId: string;
  onClose: () => void;
  /** Open Settings → Profiles. */
  manage: () => void;
}

export function ProfileView({ api, bus, profileId, onClose, manage }: ProfileViewProps) {
  const { store, snap } = useProfiles(api, bus);
  useHouseholdWatch(store);
  const profile = snap.view?.profiles.find((p) => p.id === profileId);
  const [pages, setPages] = useState<PinnedPage[] | null>(null);
  useEffect(() => {
    let live = true;
    const read = () =>
      void pageOps(api)
        .list(profileId)
        .then((r) => live && setPages(r.pages))
        .catch(() => live && setPages(null));
    read();
    const off = bus.on('pages_changed', read);
    return () => {
      live = false;
      off();
    };
  }, [api, bus, profileId]);
  const person = useMemo(() => (profile ? personFor(profile, snap.people) : null), [profile, snap.people]);
  if (!profile) return null;
  const now = Date.now();
  const active = snap.view?.activeId === profile.id;
  return (
    <Dialog
      title="Profile"
      onClose={onClose}
      testId="profile-view"
      footer={
        <>
          <Button variant="text" onClick={manage} data-testid="profile-view-manage">
            Manage profiles…
          </Button>
          <Button variant="secondary" disabled={active} onClick={() => void store.ops.setActive(profile.id).then((v) => store.apply(v))} data-testid="profile-view-show">
            {active ? 'Showing now' : 'Show this profile'}
          </Button>
          <Button variant="primary" onClick={onClose}>
            Close
          </Button>
        </>
      }
    >
      <div className="pf-view">
        <div className="pf-view-head">
          <ProfileDisc name={profile.name} colour={profile.colour} size={34} />
          <div>
            <div className="pf-view-name" data-testid="profile-view-name">
              {profile.name}
            </div>
            <div className="pf-view-sub">{profile.isHome ? "This computer's profile" : 'Another person'}</div>
          </div>
        </div>

        <dl className="pf-view-list">
          <dt>Sign-in</dt>
          <dd>{SIGN_IN_NOTE}</dd>
          <dt>Chats</dt>
          <dd data-testid="profile-view-counts">
            {profile.chats} {profile.chats === 1 ? 'chat' : 'chats'} · {profile.running} running · {profile.attention} need attention
          </dd>
          <dt>Hub person</dt>
          <dd>
            {!profile.hubPersonId ? (
              'Not linked to anyone on the hub.'
            ) : !person ? (
              'Linked; the hub is not answering right now.'
            ) : (
              <>
                {person.name}
                {person.you ? ' (you)' : ''}
                <ul className="pf-machines">
                  {person.machines.length === 0 && <li>No computers yet.</li>}
                  {person.machines.map((m) => (
                    <li key={m.id}>
                      <span className="pf-dot" data-online={m.online ? 'true' : 'false'} aria-hidden="true" />
                      {m.name} · {offlineText(m, now)}
                    </li>
                  ))}
                </ul>
              </>
            )}
          </dd>
          <dt>Pinned pages</dt>
          <dd>{pages === null ? '—' : pages.length === 0 ? 'None.' : pages.map((p) => p.title || p.path).join(', ')}</dd>
        </dl>

        {profile.isHome && (
          <>
            <h3 className="pf-view-heading">Household</h3>
            <SharingSwitches api={api} bus={bus} />
          </>
        )}
      </div>
    </Dialog>
  );
}
