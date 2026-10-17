// The real transport: Tauri's invoke and event listener. Imported only by the app bootstrap, so
// everything else stays testable under node:test.

import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { createApi } from './client.ts';
import { EVENT_CHANNEL, EventBus } from './events.ts';

export const api = createApi((command, payload) => invoke(command, payload));
export const bus = new EventBus();

/** Start delivering host events to the bus. */
export async function connectEvents(): Promise<() => void> {
  return listen(EVENT_CHANNEL, (e) => bus.dispatch(e.payload));
}
