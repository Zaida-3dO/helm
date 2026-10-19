// Paced reveal of streaming text.
//
// The host sends a snapshot about every 80 ms. Revealing each snapshot at once looks jerky, so each
// burst of new text is spread over the time we expect until the next burst:
// - the first burst shows at once: nothing is on screen yet, so there is no rhythm to keep, and
//   holding it back only delays the first words;
// - later bursts are spread over STEADY_GAP_MS;
// - a burst's reveal window is that gap, never more than MAX_WINDOW_MS;
// - once the stream is done, whatever is left is revealed over FINISH_WINDOW_MS;
// - the unrevealed backlog never exceeds BACKLOG_CAP chars (the excess is revealed at once);
// - a new visible text is committed at most every COMMIT_MS;
// - the reveal always ends on a word boundary, so each word appears whole and fades in once.
// Hidden documents skip pacing entirely. Pure: the caller supplies the clock.

export const STEADY_GAP_MS = 120;
export const MAX_WINDOW_MS = 1000;
export const FINISH_WINDOW_MS = 250;
export const BACKLOG_CAP = 4096;
export const COMMIT_MS = 32;

export class Pacer {
  private target = '';
  private shown = 0;
  /** Revealed length when the current window opened, and the window itself. */
  private from = 0;
  private windowStart = 0;
  private windowMs = STEADY_GAP_MS;
  private bursts = 0;
  private done = false;
  private lastCommit = -Infinity;

  /** Visible text right now. */
  get text(): string {
    return this.target.slice(0, this.shown);
  }

  get finished(): boolean {
    return this.done && this.shown >= this.target.length;
  }

  /** A new snapshot of the full text. `done` once the row is no longer streaming. */
  push(text: string, now: number, done: boolean, hidden = false): void {
    if (hidden) {
      this.target = text;
      this.shown = text.length;
      this.done = done;
      return;
    }
    if (!text.startsWith(this.target.slice(0, this.shown))) {
      // The text was rewritten, not extended: show the new text as it is.
      this.target = text;
      this.shown = text.length;
      this.from = this.shown;
      this.done = done;
      return;
    }
    const grew = text.length > this.target.length;
    this.target = text;
    if (this.target.length - this.shown > BACKLOG_CAP) this.shown = this.target.length - BACKLOG_CAP;
    if (done && !this.done) {
      this.done = true;
      this.openWindow(now, FINISH_WINDOW_MS);
    } else if (grew) {
      this.bursts += 1;
      if (this.bursts === 1) this.shown = this.from = this.target.length;
      else this.openWindow(now, Math.min(STEADY_GAP_MS, MAX_WINDOW_MS));
    }
    if (this.shown > this.target.length) this.shown = this.target.length;
  }

  private openWindow(now: number, ms: number): void {
    this.from = this.shown;
    this.windowStart = now;
    this.windowMs = ms;
  }

  /** Advance the reveal. Returns the new visible text, or null when nothing should be committed. */
  tick(now: number): string | null {
    if (this.shown >= this.target.length) return null;
    if (now - this.lastCommit < COMMIT_MS) return null;
    const progress = Math.min(1, (now - this.windowStart) / this.windowMs);
    let next = Math.floor(this.from + (this.target.length - this.from) * progress);
    if (progress < 1) next = wordEnd(this.target, next);
    if (next <= this.shown) return null;
    this.shown = Math.min(next, this.target.length);
    this.lastCommit = now;
    return this.text;
  }
}

/** Extend `i` to the end of the word it falls in (the next whitespace, or the end of the text). */
export function wordEnd(text: string, i: number): number {
  if (i <= 0) return 0;
  if (i >= text.length) return text.length;
  if (/\s/.test(text[i - 1]!)) return i;
  const rest = text.slice(i).search(/\s/);
  return rest < 0 ? text.length : i + rest;
}
