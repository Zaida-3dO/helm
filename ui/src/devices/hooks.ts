// React access to the devices store and to another computer's chat list.

import { useEffect, useMemo, useSyncExternalStore } from 'react';
import type { Api } from '../api/client.ts';
import { DEVICE_EVENTS } from '../api/devices.ts';
import type { EventBus } from '../api/events.ts';
import type { ChatRecord } from '../api/types.ts';
import { deviceOps, type DeviceInfo } from './api.ts';
import { composerFor, remoteChatState, remoteHostOf, type RemoteChatState, type RemoteComposer } from './state.ts';
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

/** A chat that runs on another device, as the chat view needs it. */
export interface RemoteView {
  uid: string;
  device: DeviceInfo | null;
  state: RemoteChatState;
  composer: RemoteComposer;
}

/** The device a chat runs on and how it reads right now; null for a chat that runs here. */
export function useRemoteChat(api: Api, bus: EventBus, chat: Pick<ChatRecord, 'hostDeviceId'> | null): RemoteView | null {
  const { snap } = useDevices(api, bus);
  const uid = remoteHostOf(chat, snap.list, snap.selfUid);
  return useMemo(() => {
    if (!uid) return null;
    const device = snap.list?.devices.find((d) => d.uid === uid) ?? null;
    const state = remoteChatState(device, Date.now(), !snap.list);
    return { uid, device, state, composer: composerFor(uid, state) };
  }, [uid, snap.list]);
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
      // Chats of that computer nobody has open here change without a `chat` event.
      bus.on(DEVICE_EVENTS.chats, (e) => {
        if ((e as { deviceUid?: string }).deviceUid === deviceUid) later();
      }),
      bus.on(DEVICE_EVENTS.devices, later),
      bus.on(DEVICE_EVENTS.profiles, later),
    ];
    return () => {
      clearTimeout(timer);
      offs.forEach((o) => o());
    };
  }, [list, bus, deviceUid, profileId, archived]);
  return { snap, loadMore: () => void list.loadMore() };
}
