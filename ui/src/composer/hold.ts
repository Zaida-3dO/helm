// A send to a chat on a device that is only away waits here instead of being lost: the composer
// keeps the message with the submission id it already made, and hands it over once the device is
// back. The host claims each submission id once, so the same id can only ever run once; a held
// send is therefore handed over exactly once, and a refusal or an unconfirmed answer never makes
// it go a second time on its own.

import type { SendAction } from '../api/types.ts';

export interface HeldSend {
  submissionId: string;
  action: SendAction;
  /** The device it waits for. It only ever goes there: picking another device takes it back. */
  deviceUid: string;
  /** The message as it was when Send was pressed; edits made while it waits are the draft's. */
  text: string;
}

export class SendHold {
  private held: HeldSend | null = null;

  /** The message waits. Pressing Send again replaces what waited before it. */
  hold(send: HeldSend): void {
    this.held = send;
  }

  /** What waits, without taking it. */
  peek(): HeldSend | null {
    return this.held;
  }

  /** The person takes it back: nothing will be sent on its own, the draft stays. */
  cancel(): HeldSend | null {
    const h = this.held;
    this.held = null;
    return h;
  }

  /** The device is live: the held send comes to light exactly once, however many live updates follow. */
  release(): HeldSend | null {
    const h = this.held;
    this.held = null;
    return h;
  }
}

/** What to do with a held send when the device, its state or the send in flight changes. */
export type HoldStep = 'keep' | 'cancel' | 'release';

/**
 * The release decision. A held send only ever goes to the device it was held for: with no device
 * (the chat is on this PC now) or another one picked, it is taken back. It goes once that device
 * is live and no other send is still starting; until then it keeps waiting, so nothing is dropped.
 */
export function holdStep(
  held: Pick<HeldSend, 'deviceUid'> | null,
  remote: { deviceUid: string; blocked: string | null } | null,
  sending: boolean,
): HoldStep {
  if (!held) return 'keep';
  if (!remote || remote.deviceUid !== held.deviceUid) return 'cancel';
  if (remote.blocked !== null || sending) return 'keep';
  return 'release';
}
