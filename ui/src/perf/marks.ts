// Timing marks for "where did the time go". Each send gets one timeline:
// the send click or key, the user row arriving from the host and painted, and the reply's first
// words arriving, shown and painted. The app exposes the recorder as `window.__helmPerf` so a
// check or a person at the DevTools console can read it; nothing leaves the window.
//
// Pure apart from the clock and the frame scheduler, which are injected.

/** A send key or click counts for the user row that arrives within this long. */
export const SEND_WINDOW_MS = 3000;
const MAX_MARKS = 200;
const MAX_SENDS = 20;

export interface PerfMark {
  name: string;
  /** performance.now() time. */
  t: number;
  detail?: string;
}

/** One send's timeline, in performance.now() milliseconds. Unknown steps are null. */
export interface SendTimeline {
  chatId: string;
  /** The send click or key; null when the row arrived without one (a queued or scheduled send). */
  send: number | null;
  rowArrived: number;
  rowPainted: number | null;
  /** The first assistant text of the turn reached the store. */
  tokenArrived: number | null;
  /** The pacer put the first words on screen. */
  tokenShown: number | null;
  tokenPainted: number | null;
}

export interface SendSummary {
  /** Send to the user row painted. */
  rowMs: number | null;
  /** Send to the first words painted. */
  firstWordMs: number | null;
}

export class PerfRecorder {
  readonly marks: PerfMark[] = [];
  readonly sends: SendTimeline[] = [];
  private intent: number | null = null;
  /** The newest send, until its first words are painted. */
  private open: SendTimeline | null = null;

  private readonly now: () => number;
  /** Runs `fn` once the next frame has been painted. */
  private readonly afterPaint: (fn: () => void) => void;

  constructor(now: () => number, afterPaint: (fn: () => void) => void) {
    this.now = now;
    this.afterPaint = afterPaint;
  }

  mark(name: string, detail?: string): void {
    this.marks.push(detail === undefined ? { name, t: this.now() } : { name, t: this.now(), detail });
    if (this.marks.length > MAX_MARKS) this.marks.splice(0, this.marks.length - MAX_MARKS);
  }

  /** The user pressed Send (or its key). */
  sendIntent(): void {
    this.intent = this.now();
    this.mark('send');
  }

  /** A new user row reached the store. */
  userRow(chatId: string): void {
    const t = this.now();
    const send = this.intent !== null && t - this.intent <= SEND_WINDOW_MS ? this.intent : null;
    this.intent = null;
    const tl: SendTimeline = { chatId, send, rowArrived: t, rowPainted: null, tokenArrived: null, tokenShown: null, tokenPainted: null };
    this.sends.push(tl);
    if (this.sends.length > MAX_SENDS) this.sends.shift();
    this.open = tl;
    this.mark('user-row', chatId);
    this.afterPaint(() => {
      tl.rowPainted = this.now();
    });
  }

  /** Assistant text reached the store for this chat. Only the first after a send counts. */
  tokenArrived(chatId: string): void {
    const tl = this.open;
    if (!tl || tl.chatId !== chatId || tl.tokenArrived !== null) return;
    tl.tokenArrived = this.now();
    this.mark('first-token', chatId);
  }

  /** A streaming reply showed its first words. */
  tokenShown(): void {
    const tl = this.open;
    if (!tl || tl.tokenArrived === null || tl.tokenShown !== null) return;
    tl.tokenShown = this.now();
    this.open = null;
    this.afterPaint(() => {
      tl.tokenPainted = this.now();
    });
  }

  /** The newest send's headline numbers. */
  last(): SendSummary | null {
    const tl = this.sends.at(-1);
    return tl ? summariseSend(tl) : null;
  }
}

export function summariseSend(tl: SendTimeline): SendSummary {
  const from = tl.send ?? tl.rowArrived;
  return {
    rowMs: tl.rowPainted === null ? null : tl.rowPainted - from,
    firstWordMs: tl.tokenPainted === null ? null : tl.tokenPainted - from,
  };
}
