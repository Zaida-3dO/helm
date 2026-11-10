// The app's end of the page bridge. A page frame can only post messages to its parent; this is
// the one place that decides which of them are heard.
//
// A message is heard only when all of these hold: it came from the very window of the frame this
// bridge was made for (not from a frame's navigated-to document, a sibling or the app itself), it
// carries this bridge's channel and the protocol mark, and it is well formed. Requests are then
// answered only while the tab is visible, at most `MAX_PENDING` at a time, no larger than
// `MAX_PAYLOAD`, and only for the three methods a page may use.

import { MARK, MAX_PAYLOAD, MAX_PENDING, METHODS, parseMessage } from './protocol.ts';

export interface BridgeOptions {
  channel: string;
  /** The frame's window right now; null when the frame is not mounted. */
  frame: () => unknown;
  /** Posts to the frame. */
  post: (message: Record<string, unknown>) => void;
  /** Whether the tab is shown. Requests while hidden are refused. */
  visible: () => boolean;
  /** Runs a method already known to be allowed. A rejection's message goes back to the page. */
  request: (method: string, payload: unknown) => Promise<unknown>;
  onReady: () => void;
  /** At most one per second. */
  onError: (message: string) => void;
  onNetworkBlocked: () => void;
  now?: () => number;
}

function words(e: unknown): string {
  return (e instanceof Error ? e.message : String(e)).slice(0, 300);
}

export class PageBridge {
  private outstanding = 0;
  private lastError = -Infinity;
  private closed = false;

  private readonly o: BridgeOptions;

  constructor(o: BridgeOptions) {
    this.o = o;
  }

  /** Stops hearing and answering. */
  close(): void {
    this.closed = true;
  }

  /** Sends the app's side of the protocol to the page. */
  send(type: string, fields: Record<string, unknown> = {}): void {
    if (!this.closed) this.o.post({ mark: MARK, channel: this.o.channel, type, ...fields });
  }

  private respond(id: number, result: unknown, error?: string): void {
    this.send('response', error === undefined ? { id, result: result ?? null } : { id, error });
  }

  /** One `message` event from the window. */
  receive(event: { source: unknown; data: unknown }): void {
    const frame = this.o.frame();
    if (this.closed || !frame || event.source !== frame) return;
    const m = parseMessage(event.data, this.o.channel);
    if (!m) return;
    switch (m.type) {
      case 'ready':
        this.o.onReady();
        return;
      case 'networkBlocked':
        this.o.onNetworkBlocked();
        return;
      case 'error': {
        const now = (this.o.now ?? Date.now)();
        if (now - this.lastError < 1000) return;
        this.lastError = now;
        this.o.onError(m.message);
        return;
      }
      case 'request': {
        const { id, method, payload } = m;
        if (!this.o.visible()) return this.respond(id, null, 'This app cannot request that action right now.');
        if (this.outstanding >= MAX_PENDING) return this.respond(id, null, 'Too many pending app requests.');
        let size = 0;
        try {
          size = JSON.stringify(payload ?? null).length;
        } catch {
          return this.respond(id, null, 'Unsupported app request.');
        }
        if (size > MAX_PAYLOAD) return this.respond(id, null, 'App request is too large.');
        if (!(METHODS as readonly string[]).includes(method)) return this.respond(id, null, 'Unsupported app request.');
        this.outstanding++;
        void this.o
          .request(method, payload)
          .then(
            (r) => this.respond(id, r),
            (e: unknown) => this.respond(id, null, words(e)),
          )
          .finally(() => {
            this.outstanding--;
          });
      }
    }
  }
}
