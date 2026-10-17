// Draft saving. The draft is saved DRAFT_DEBOUNCE_MS after the last edit,
// with revision max(now, prev + 1). The host refuses anything not newer than what it holds, so a
// stale writer cannot overwrite newer text. A send clears the draft unless a newer revision was
// written after the send began.

export const DRAFT_DEBOUNCE_MS = 250;

export function nextRevision(prev: number, now: number): number {
  return Math.max(now, prev + 1);
}

export interface Timers {
  now(): number;
  set(f: () => void, ms: number): unknown;
  clear(h: unknown): void;
}

export const realTimers: Timers = {
  now: () => Date.now(),
  set: (f, ms) => setTimeout(f, ms),
  clear: (h) => clearTimeout(h as ReturnType<typeof setTimeout>),
};

export type SaveFn = (text: string, revision: number) => Promise<unknown>;

export class DraftSaver {
  /** The newest revision this composer has assigned. */
  revision: number;
  private text: string;
  private timer: unknown = null;
  private dirty = false;
  private readonly save: SaveFn;
  private readonly timers: Timers;

  constructor(initialText: string, initialRevision: number, save: SaveFn, timers: Timers = realTimers) {
    this.save = save;
    this.timers = timers;
    this.text = initialText;
    this.revision = initialRevision;
  }

  /** Record an edit; the save fires DRAFT_DEBOUNCE_MS after the last one. */
  edit(text: string): void {
    this.text = text;
    this.revision = nextRevision(this.revision, this.timers.now());
    this.dirty = true;
    if (this.timer !== null) this.timers.clear(this.timer);
    this.timer = this.timers.set(() => void this.flush(), DRAFT_DEBOUNCE_MS);
  }

  /** Save now if anything is unsaved (used on quit and on chat switch). */
  async flush(): Promise<void> {
    if (this.timer !== null) {
      this.timers.clear(this.timer);
      this.timer = null;
    }
    if (!this.dirty) return;
    this.dirty = false;
    try {
      await this.save(this.text, this.revision);
    } catch {
      // A stale refusal means a newer draft is stored; nothing to do. Other failures retry on the
      // next edit.
    }
  }

  /** Start a send: returns a token to pass to `sent`. */
  beginSend(): number {
    return this.revision;
  }

  /**
   * The send succeeded. Clears the draft (returns true) unless a newer revision exists than the one
   * the send began at, in which case the newer text stays. `sameText` says the editor still holds
   * exactly the sent text: an edit event that landed after the send began without changing the
   * text (the editor reports changes asynchronously) does not count as newer text.
   */
  sent(token: number, sameText = false): boolean {
    if (this.revision !== token && !sameText) return false;
    this.text = '';
    this.revision = nextRevision(this.revision, this.timers.now());
    this.dirty = true;
    void this.flush();
    return true;
  }

  dispose(): void {
    if (this.timer !== null) this.timers.clear(this.timer);
    this.timer = null;
  }
}
