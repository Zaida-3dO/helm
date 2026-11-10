// How a chat that runs on another device reads and what may be done to it. Plain data, no React, so
// the rules are tested under node:test.
//
// The device is live, read-only (its switch for other devices is off), offline (the hub says it is
// not up) or the copy is all there is because this PC's own line to the hub is down. The words are
// the ones the host uses when it refuses a change, so the bar and a refusal never disagree.

import type { DeviceState } from '../api/devices.ts';
import type { ChatRecord } from '../api/types.ts';
import { lastSeenMs } from '../profiles/model.ts';
import type { DeviceInfo, DevicesList } from './api.ts';

export type RemoteTone = 'neutral' | 'amber' | 'grey';

export interface RemoteChatState {
  state: DeviceState;
  deviceName: string;
  /** The sentence for the person; null while the chat is live. */
  text: string | null;
  tone: RemoteTone;
  /** Sending, stopping, answering and changing the model all work. */
  live: boolean;
}

/** What the composer and the new-chat page are told about a chat (or a new chat) on another device. */
export interface RemoteComposer {
  deviceUid: string;
  deviceName: string;
  /** Why sending is off (the placeholder); null when it is on. */
  blocked: string | null;
}

/** The "didn't confirm" sentence a send that got no answer is shown with. */
export function unconfirmedText(deviceName: string): string {
  return `${deviceName} didn't confirm this message. It may or may not have arrived.`;
}

/** How a device reads: the host's own word when it gives one, else worked out from the flags. */
export function deviceState(d: Pick<DeviceInfo, 'online' | 'run' | 'state'> | null | undefined): DeviceState {
  if (!d) return 'offline';
  if (d.state) return d.state;
  if (!d.online) return 'offline';
  return d.run ? 'live' : 'readonly';
}

/** "14:02" for today, "10 Nov 14:02" for another day, "never" when it never connected. */
export function seenText(iso: string | null | undefined, now: number): string {
  const ms = lastSeenMs(iso);
  if (ms === null) return 'never';
  const at = new Date(ms);
  const time = at.toLocaleTimeString(undefined, { hour: '2-digit', minute: '2-digit' });
  if (at.toDateString() === new Date(now).toDateString()) return time;
  return `${at.toLocaleDateString(undefined, { day: 'numeric', month: 'short' })} ${time}`;
}

/**
 * The state of a chat's host device. A device that is not on the hub (any more) reads as offline;
 * with no device list at all (`listMissing`) nothing is known, so it reads as reconnecting.
 */
export function remoteChatState(d: DeviceInfo | null | undefined, now: number, listMissing = false): RemoteChatState {
  const state = !d && listMissing ? 'reconnecting' : deviceState(d);
  const name = d?.name ?? 'Another device';
  switch (state) {
    case 'live':
      return { state, deviceName: name, text: null, tone: 'neutral', live: true };
    case 'readonly':
      return { state, deviceName: name, text: `${name} doesn't let other devices run its chats. You can read this one.`, tone: 'neutral', live: false };
    case 'reconnecting':
      return { state, deviceName: name, text: 'Reconnecting to the hub… showing the copy on this PC.', tone: 'amber', live: false };
    default:
      return {
        state: 'offline',
        deviceName: name,
        text: `${name} is offline (last seen ${seenText(d?.lastSeenAt, now)}). You can read this chat; sending is off until it's back.`,
        tone: 'grey',
        live: false,
      };
  }
}

/**
 * The device a chat runs on, when that is not this PC; null for a chat here. Without a list, this
 * PC's own uid (`selfUid`) decides; with neither, the chat is taken to run here.
 */
export function remoteHostOf(chat: Pick<ChatRecord, 'hostDeviceId'> | null | undefined, list: DevicesList | null, selfUid: string | null = null): string | null {
  const host = chat?.hostDeviceId ?? '';
  const self = list?.self ?? selfUid;
  if (!self || host === '' || host === self) return null;
  return host;
}

/** What the composer needs for a chat on another device. */
export function composerFor(uid: string, st: RemoteChatState): RemoteComposer {
  return { deviceUid: uid, deviceName: st.deviceName, blocked: st.live ? null : st.text };
}

/** The line a tool card shows while an Allow request waits on the device the chat runs on. */
export function waitingForApproval(deviceName: string): string {
  return `Waiting for approval on ${deviceName}`;
}
