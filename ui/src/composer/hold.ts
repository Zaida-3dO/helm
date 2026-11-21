// A send to a chat on a device that is only away waits here instead of being lost: the composer
// keeps the message with the submission id it already made, and hands it over once the device is
// back. The host claims each submission id once, so the same id can only ever run once; a held
// send is therefore handed over exactly once, and a refusal or an unconfirmed answer never makes
// it go a second time on its own.

import type { SendAction } from '../api/types.ts';

export interface HeldSend {
  submissionId: string;
  action: SendAction;
  /** The message as it was when Send was pressed; edits made while it waits are the draft's. */
  text: string;
}

export class SendHold {
  private held: HeldSend | null = null;

  /** The message waits. Pressing Send again replaces what waited before it. */
  hold(send: HeldSend): void {
    this.held = send;
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
