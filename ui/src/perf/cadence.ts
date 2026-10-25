// Low-refresh motion. On a display that refreshes slowly (Remote Desktop
// runs at 32 Hz, so every frame is 31 ms) word-by-word reveals, spinners and ticking counters look
// choppy and every frame of them is more to send over the wire. When the median animation-frame
// interval is 25 ms or more, the app treats motion as reduced, exactly as if the system asked for
// Reduce Motion: no paced reveal, no animations or transitions (perf.css), still spinners.
//
// The cadence is sampled for about a second shortly after start, and again whenever the window
// comes back into view (a Remote Desktop session may have connected or left in between). A slow
// sample must be confirmed by the next one. Frames slower than 100 ms are a throttled or busy
// window, not a display, and are ignored. While the verdict is low it is sampled again every 30 s,
// so a false one (two busy spells in a row at start-up) clears itself.

export const LOW_CADENCE_MS = 25;
export const SAMPLE_MS = 1000;
const MAX_FRAME_MS = 100;
const MIN_FRAMES = 12;

/** While motion is held low, look again this often, so a verdict reached during a busy spell heals. */
export const RECHECK_MS = 30_000;

export type Cadence = 'low' | 'normal';

/** The display's cadence from a run of frame intervals, or null when the run says too little. */
export function classifyCadence(intervals: readonly number[]): Cadence | null {
  const xs = intervals.filter((d) => d > 0 && d < MAX_FRAME_MS).sort((a, b) => a - b);
  if (xs.length < MIN_FRAMES) return null;
  const mid = xs.length >> 1;
  const median = xs.length % 2 ? xs[mid]! : (xs[mid - 1]! + xs[mid]!) / 2;
  return median >= LOW_CADENCE_MS ? 'low' : 'normal';
}

/** A second sample confirms a slow one this long after it. */
export const CONFIRM_MS = 3000;

/**
 * Turns samples into decisions. A fast sample decides at once; a slow one only counts once the
 * next sample agrees, so one busy second (a heavy start-up) cannot switch a fast display to
 * reduced motion.
 */
export class CadenceGate {
  private slow = false;

  /** 'confirm' asks for another sample soon; null means the sample said nothing. */
  push(c: Cadence | null): Cadence | 'confirm' | null {
    if (c === null) return null;
    if (c === 'normal') {
      this.slow = false;
      return 'normal';
    }
    if (this.slow) return 'low';
    this.slow = true;
    return 'confirm';
  }

  reset(): void {
    this.slow = false;
  }
}

/** What the cadence says about motion, with a manual override for checks and debugging. */
export class MotionPolicy {
  private cadence: Cadence | null = null;
  private forced: Cadence | null = null;
  private readonly listeners = new Set<() => void>();

  /** True when motion should be treated as reduced because of the display. */
  get low(): boolean {
    return (this.forced ?? this.cadence) === 'low';
  }

  get measured(): Cadence | null {
    return this.cadence;
  }

  setCadence(c: Cadence): void {
    const before = this.low;
    this.cadence = c;
    if (this.low !== before) this.emit();
  }

  /** Force a cadence ('auto' goes back to the measured one). */
  force(c: Cadence | 'auto'): void {
    const before = this.low;
    this.forced = c === 'auto' ? null : c;
    if (this.low !== before) this.emit();
  }

  subscribe = (l: () => void): (() => void) => {
    this.listeners.add(l);
    return () => {
      this.listeners.delete(l);
    };
  };

  private emit(): void {
    for (const l of [...this.listeners]) l();
  }
}

export const motion = new MotionPolicy();

/**
 * Sample the frame cadence now and whenever the document becomes visible again, and keep the root
 * element's `data-motion` ("low" or absent) in step with the policy. Returns the uninstaller.
 */
export function watchCadence(doc: Document, policy: MotionPolicy = motion, startDelayMs = 1500): () => void {
  const root = doc.documentElement;
  const apply = () => {
    if (policy.low) root.dataset.motion = 'low';
    else delete root.dataset.motion;
  };
  const offPolicy = policy.subscribe(apply);
  apply();

  let raf = 0;
  let timer: ReturnType<typeof setTimeout> | undefined;
  let tries = 0;
  const gate = new CadenceGate();
  const sample = () => {
    if (doc.hidden) return;
    const intervals: number[] = [];
    let last = -1;
    let first = -1;
    const step = (t: number) => {
      if (first < 0) first = t;
      if (last >= 0) intervals.push(t - last);
      last = t;
      if (t - first < SAMPLE_MS) {
        raf = requestAnimationFrame(step);
        return;
      }
      const next = gate.push(classifyCadence(intervals));
      if (next === 'low' || next === 'normal') {
        policy.setCadence(next);
        if (next === 'low') timer = setTimeout(sample, RECHECK_MS);
      } else if (next === 'confirm') timer = setTimeout(sample, CONFIRM_MS);
      else if (++tries < 3) timer = setTimeout(sample, 5000);
    };
    raf = requestAnimationFrame(step);
  };
  const onVisible = () => {
    if (doc.hidden) return;
    cancelAnimationFrame(raf);
    clearTimeout(timer);
    tries = 0;
    gate.reset();
    timer = setTimeout(sample, 500);
  };
  timer = setTimeout(sample, startDelayMs);
  doc.addEventListener('visibilitychange', onVisible);
  return () => {
    cancelAnimationFrame(raf);
    clearTimeout(timer);
    doc.removeEventListener('visibilitychange', onVisible);
    offPolicy();
    delete root.dataset.motion;
  };
}
