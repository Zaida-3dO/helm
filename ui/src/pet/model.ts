// What the desktop pet shows: a pose and a speech bubble worked out from what is happening in the
// chats. Pure: the page feeds it the attention summary, finished replies and chat records, and
// asks it for the pose at a given time. Muted chats never reach it (the host leaves them out of
// the summary and the completion events, and a muted chat's records are ignored here).

export type Pose = 'idle' | 'working' | 'waiting' | 'celebrate' | 'failed' | 'sleeping';
export type PetKind = 'puffin' | 'crab' | 'lantern';
export type PetSize = 's' | 'm' | 'l';

export interface PetPref {
  enabled: boolean;
  kind: PetKind;
  size: PetSize;
  bubble: boolean;
  /** Where the user left the window (screen pixels); the host owns it, the settings pane only keeps it. */
  position: { x: number; y: number } | null;
}

export const PET_DEFAULT: PetPref = { enabled: false, kind: 'puffin', size: 'm', bubble: true, position: null };
export const PET_KINDS: readonly PetKind[] = ['puffin', 'crab', 'lantern'];
export const PET_SIZES: readonly PetSize[] = ['s', 'm', 'l'];
/** The creature's height and width in px for each size. */
export const PET_PX: Record<PetSize, number> = { s: 96, m: 128, l: 160 };

/** A stored `desktopPet`; each field is kept only when it is the right shape. */
export function parsePet(v: unknown): PetPref {
  const o = (v && typeof v === 'object' ? v : {}) as Record<string, unknown>;
  const p = o.position && typeof o.position === 'object' ? (o.position as Record<string, unknown>) : null;
  return {
    enabled: o.enabled === true,
    kind: PET_KINDS.find((k) => k === o.kind) ?? PET_DEFAULT.kind,
    size: PET_SIZES.find((s) => s === o.size) ?? PET_DEFAULT.size,
    bubble: typeof o.bubble === 'boolean' ? o.bubble : PET_DEFAULT.bubble,
    position: p && Number.isInteger(p.x) && Number.isInteger(p.y) ? { x: p.x as number, y: p.y as number } : null,
  };
}

export const CELEBRATE_MS = 6_000;
export const FAILED_MS = 6_000;
export const SLEEP_AFTER_MS = 10 * 60_000;

/** Everything the pose depends on. */
export interface PetState {
  needsYou: number;
  running: number;
  unread: number;
  /** When the last reply finished, or null. */
  completedAt: number | null;
  /** When the last chat was interrupted, or null. */
  failedAt: number | null;
  /** When anything last happened. */
  lastEventAt: number;
}

export function initialState(now: number): PetState {
  return { needsYou: 0, running: 0, unread: 0, completedAt: null, failedAt: null, lastEventAt: now };
}

/** The pose, by priority: a question waiting, a recent finish, a recent failure, work, sleep, idle. */
export function poseOf(s: PetState, now: number): Pose {
  if (s.needsYou > 0) return 'waiting';
  if (s.completedAt !== null && now - s.completedAt < CELEBRATE_MS) return 'celebrate';
  if (s.failedAt !== null && now - s.failedAt < FAILED_MS) return 'failed';
  if (s.running > 0) return 'working';
  if (now - s.lastEventAt >= SLEEP_AFTER_MS) return 'sleeping';
  return 'idle';
}

const count = (n: number) => (n > 99 ? '99+' : String(n));

/** The bubble's words (18 characters at most), or null when there is nothing to say. */
export function bubbleOf(s: PetState): string | null {
  if (s.needsYou > 0) return s.needsYou === 1 ? '1 needs you' : `${count(s.needsYou)} need you`;
  if (s.unread > 0) return `${count(s.unread)} done`;
  return null;
}

/** When the pose will change by itself (a timer running out), or null if only an event can change it. */
export function nextPoseChange(s: PetState, now: number): number | null {
  const at: number[] = [];
  if (s.completedAt !== null) at.push(s.completedAt + CELEBRATE_MS);
  if (s.failedAt !== null) at.push(s.failedAt + FAILED_MS);
  at.push(s.lastEventAt + SLEEP_AFTER_MS);
  const later = at.filter((t) => t > now);
  return later.length ? Math.min(...later) : null;
}

// --- events -----------------------------------------------------------------------------------

interface Rows {
  chatId?: unknown;
}

const ids = (rows: unknown): string[] =>
  Array.isArray(rows) ? rows.map((r) => (r as Rows)?.chatId).filter((id): id is string => typeof id === 'string') : [];

/** An attention summary: the three counts. A change in them counts as something happening. */
export function withSummary(s: PetState, summary: unknown, now: number): PetState {
  const o = (summary && typeof summary === 'object' ? summary : {}) as Record<string, unknown>;
  const next = { ...s, needsYou: ids(o.needsYou).length, running: ids(o.running).length, unread: ids(o.unread).length };
  const same = next.needsYou === s.needsYou && next.running === s.running && next.unread === s.unread;
  return same ? next : { ...next, lastEventAt: now };
}

/** A reply finished. */
export function withCompletion(s: PetState, now: number): PetState {
  return { ...s, completedAt: now, lastEventAt: now };
}

/** A chat was interrupted. */
export function withFailure(s: PetState, now: number): PetState {
  return { ...s, failedAt: now, lastEventAt: now };
}

const BUSY = ['starting', 'running', 'stopping'];

/**
 * Follows chat records and says when one has just failed: a chat that was working (or listed as
 * running) and now reads `interrupted`. A chat first heard of already interrupted is old news, and
 * a muted or archived chat never counts.
 */
export class FailureWatch {
  private readonly status = new Map<string, string>();
  private running = new Set<string>();

  /** The chats the latest summary lists as running. */
  setRunning(summary: unknown): void {
    this.running = new Set(ids((summary as { running?: unknown } | null)?.running));
  }

  /** Feed a chat record; true when it has just failed. */
  chat(record: unknown): boolean {
    const c = (record && typeof record === 'object' ? record : {}) as Record<string, unknown>;
    if (typeof c.id !== 'string' || typeof c.status !== 'string') return false;
    const before = this.status.get(c.id);
    this.status.set(c.id, c.status);
    if (this.status.size > 2000) this.status.delete(this.status.keys().next().value as string);
    if (c.status !== 'interrupted' || c.muted === true || c.archived === true) return false;
    return (before !== undefined && BUSY.includes(before)) || (before === undefined && this.running.has(c.id));
  }
}

/** Which chat a click opens: the first that needs you, else the newest unread, else none. */
export function clickTarget(summary: unknown): string | null {
  const o = (summary && typeof summary === 'object' ? summary : {}) as Record<string, unknown>;
  return ids(o.needsYou)[0] ?? ids(o.unread)[0] ?? null;
}

// --- moved reports ---------------------------------------------------------------------------

/**
 * Pass on at most one call per `ms`, always ending with the latest arguments (a drag fires many
 * moves; the host needs the spot the window stopped at). Timers are parameters for tests.
 */
export function throttled<A extends unknown[]>(
  fn: (...a: A) => void,
  ms: number,
  timers: { set: (h: () => void, ms: number) => unknown; clear: (id: unknown) => void; now: () => number } = {
    set: (h, d) => setTimeout(h, d),
    clear: (id) => clearTimeout(id as ReturnType<typeof setTimeout>),
    now: () => Date.now(),
  },
): { call: (...a: A) => void; cancel: () => void } {
  let last = -Infinity;
  let pending: unknown = null;
  let latest: A | null = null;
  const fire = () => {
    pending = null;
    if (!latest) return;
    const a = latest;
    latest = null;
    last = timers.now();
    fn(...a);
  };
  return {
    call: (...a: A) => {
      latest = a;
      if (pending !== null) return;
      const wait = last + ms - timers.now();
      if (wait <= 0) fire();
      else pending = timers.set(fire, wait);
    },
    cancel: () => {
      if (pending !== null) timers.clear(pending);
      pending = null;
      latest = null;
    },
  };
}
