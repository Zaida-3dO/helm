// React access to the profiles store.

import { useEffect, useSyncExternalStore } from 'react';
import type { Api } from '../api/client.ts';
import type { EventBus } from '../api/events.ts';
import { profilesStoreFor, type ProfilesSnapshot, type ProfilesStore } from './store.ts';

/** The one store for this app, started, and its latest snapshot. */
export function useProfiles(api: Api, bus: EventBus): { store: ProfilesStore; snap: ProfilesSnapshot } {
  const store = profilesStoreFor(api, bus);
  useEffect(() => {
    store.start();
  }, [store]);
  const snap = useSyncExternalStore(store.subscribe, () => store.snapshot);
  return { store, snap };
}
