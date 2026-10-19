// Frame-interval sampling for the frame-stats overlay. It measures
// animation-frame callback cadence only: not GPU presentation, CPU or memory. Pure: the caller
// feeds the timestamps, so the maths is tested without a browser.

/** A sample is published every PUBLISH_MS, or sooner once MAX_INTERVALS intervals are held. */
export const PUBLISH_MS = 500;
export const MAX_INTERVALS = 1024;
/** A frame longer than this counts as slow. */
export const SLOW_FRAME_MS = 50;

export interface FrameSample {
  /** Frames per second over the sample: frames × 1000 / elapsed. */
  fps: number;
  avgMs: number;
  /** Nearest-rank 95th percentile of the intervals. */
  p95Ms: number;
  worstMs: number;
  /** Intervals over SLOW_FRAME_MS. */
  slow: number;
  frames: number;
  elapsedMs: number;
}

/** Summarise one window of intervals; null when it holds none. Non-positive intervals are ignored. */
export function summarise(intervals: readonly number[], elapsedMs: number): FrameSample | null {
  const xs = intervals.filter((d) => d > 0).sort((a, b) => a - b);
  if (xs.length === 0 || elapsedMs <= 0) return null;
  let sum = 0;
  let slow = 0;
  for (const d of xs) {
    sum += d;
    if (d > SLOW_FRAME_MS) slow++;
  }
  return {
    fps: (xs.length * 1000) / elapsedMs,
    avgMs: sum / xs.length,
    p95Ms: xs[Math.ceil(0.95 * xs.length) - 1]!,
    worstMs: xs[xs.length - 1]!,
    slow,
    frames: xs.length,
    elapsedMs,
  };
}

/** Feeds on animation-frame timestamps and hands back a sample whenever one is due. */
export class FrameSampler {
  private last: number | null = null;
  private start = 0;
  private intervals: number[] = [];

  frame(t: number): FrameSample | null {
    if (this.last === null) {
      this.last = t;
      this.start = t;
      return null;
    }
    const d = t - this.last;
    this.last = t;
    if (d > 0) this.intervals.push(d);
    const elapsed = t - this.start;
    if (elapsed < PUBLISH_MS && this.intervals.length < MAX_INTERVALS) return null;
    const sample = summarise(this.intervals, elapsed);
    this.intervals = [];
    this.start = t;
    return sample;
  }

  /** Forget everything (the window was hidden): the next frame starts a fresh window. */
  reset(): void {
    this.last = null;
    this.intervals = [];
  }
}
