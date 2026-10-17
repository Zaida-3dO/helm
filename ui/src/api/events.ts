// The one tagged event channel. Every event has a `kind`; most are signals that
// tell the UI to re-read through `api`.

export const EVENT_CHANNEL = 'app-event';

export type AppEvent = { kind: string } & Record<string, unknown>;
export type Handler = (event: AppEvent) => void;

/** Routes host events to handlers by kind. */
export class EventBus {
  private readonly handlers = new Map<string, Set<Handler>>();

  on(kind: string, handler: Handler): () => void {
    let set = this.handlers.get(kind);
    if (!set) {
      set = new Set();
      this.handlers.set(kind, set);
    }
    set.add(handler);
    return () => {
      set.delete(handler);
    };
  }

  /** Deliver one raw payload. Anything without a string `kind` is dropped. */
  dispatch(payload: unknown): void {
    if (!payload || typeof payload !== 'object') return;
    const kind = (payload as { kind?: unknown }).kind;
    if (typeof kind !== 'string') return;
    for (const handler of this.handlers.get(kind) ?? []) {
      try {
        handler(payload as AppEvent);
      } catch (e) {
        console.error(`event handler for ${kind} failed`, e);
      }
    }
  }
}
