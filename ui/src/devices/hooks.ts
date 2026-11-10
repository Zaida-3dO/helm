// React access to the devices store and to another computer's chat list.

import { useEffect, useMemo, useSyncExternalStore } from 'react';
import type { Api } from '../api/client.ts';
import type { EventBus } from '../api/events.ts';
import { deviceOps } from './api.ts';
import { devicesStoreFor, RemoteList, type DevicesSnapshot, type DevicesStore, type RemoteListSnapshot } from './store.ts';

/** The one store for this app, started, and its latest snapshot. */
export function useDevices(api: Api, bus: EventBus): { store: DevicesStore; snap: DevicesSnapshot } {
  const store = devicesStoreFor(api, bus);
  useEffect(() => {
    store.start();
  }, [store]);
  const snap = useSyncExternalStore(store.subscribe, () => store.snapshot);
  return { store, snap };
}

/** How long a burst of events waits before the other computer's list is read again. */
export const REMOTE_REFRESH_MS = 400;

/**
 * The chats of another computer for a profile, kept fresh while `deviceUid` is set. With no
 * computer (or no profile) nothing is read and the list is empty.
 */
export function useRemoteList(
  api: Api,
  bus: EventBus,
  deviceUid: string | null,
  profileId: string | null,
  archived: boolean,
): { snap: RemoteListSnapshot; loadMore: () => void } {
  const list = useMemo(() => new RemoteList(deviceOps(api)), [api]);
  const snap = useSyncExternalStore(list.subscribe, () => list.snapshot);
  useEffect(() => {
    if (!deviceUid || !profileId) {
      list.clear();
      return;
    }
    const q = { deviceUid, profileId, archived };
    void list.load(q);
    let timer: ReturnType<typeof setTimeout> | undefined;
    const later = () => {
      clearTimeout(timer);
      timer = setTimeout(() => void list.load(q), REMOTE_REFRESH_MS);
    };
    const offs = [
      bus.on('chat', (e) => {
        // Only that computer's chats matter here; a record that does not say where it runs is read anyway.
        const host = (e.chat as { hostDeviceId?: string } | undefined)?.hostDeviceId;
        if (host === undefined || host === deviceUid) later();
      }),
      bus.on('chat_removed', later),
      bus.on('devices_changed', later),
      bus.on('profiles_changed', later),
    ];
    return () => {
      clearTimeout(timer);
      offs.forEach((o) => o());
    };
  }, [list, bus, deviceUid, profileId, archived]);
  return { snap, loadMore: () => void list.loadMore() };
}
