// The pet page's live state: feeds the host's events to the pure model and keeps the pose current
// as timers run out. One instance per page; `connect` starts it.

import type { Api } from '../api/client.ts';
import type { EventBus } from '../api/events.ts';
import { FailureWatch, bubbleOf, clickTarget, initialState, nextPoseChange, poseOf, withCompletion, withFailure, withSummary, type PetState, type Pose } from './model.ts';

export interface PetView {
  pose: Pose;
  bubble: string | null;
  /** The chat a click opens, or null. */
  target: string | null;
}

/** A finished reply older than this is not news (it may be a replay after a reconnect). */
const FRESH_MS = 30_000;

export class PetBrain {
  private readonly now: () => number;
  private state: PetState;
  private summary: unknown = null;
  private readonly watch = new FailureWatch();
  private timer: ReturnType<typeof setTimeout> | null = null;
  private readonly listeners = new Set<() => void>();
  private view: PetView = { pose: 'idle', bubble: null, target: null };

  constructor(now: () => number = Date.now) {
    this.now = now;
    this.state = initialState(now());
  }

  subscribe = (l: () => void): (() => void) => {
    this.listeners.add(l);
    return () => {
      this.listeners.delete(l);
    };
  };

  get = (): PetView => this.view;

  private onSummary(summary: unknown): void {
    this.summary = summary;
    this.watch.setRunning(summary);
    this.state = withSummary(this.state, summary, this.now());
    this.publish();
  }

  private publish(): void {
    const now = this.now();
    const next: PetView = { pose: poseOf(this.state, now), bubble: bubbleOf(this.state), target: clickTarget(this.summary) };
    const v = this.view;
    if (next.pose !== v.pose || next.bubble !== v.bubble || next.target !== v.target) {
      this.view = next;
      for (const l of [...this.listeners]) l();
    }
    if (this.timer !== null) clearTimeout(this.timer);
    this.timer = null;
    const at = nextPoseChange(this.state, now);
    if (at !== null) this.timer = setTimeout(() => this.publish(), Math.max(50, at - now) + 5);
  }

  /** Listen to the host and read the summary once. Returns the stop function. */
  connect(api: Api, bus: EventBus): () => void {
    const offs = [
      bus.on('attention', (e) => this.onSummary(e.summary)),
      bus.on('completion', (e) => {
        const c = (e.completion ?? {}) as { completedAt?: unknown; read?: unknown; dismissed?: unknown };
        if (c.read === true || c.dismissed === true) return;
        if (typeof c.completedAt === 'number' && this.now() - c.completedAt > FRESH_MS) return;
        this.state = withCompletion(this.state, this.now());
        this.publish();
      }),
      bus.on('chat', (e) => {
        if (!this.watch.chat(e.chat)) return;
        this.state = withFailure(this.state, this.now());
        this.publish();
      }),
    ];
    api<unknown>('attention_get', {})
      .then((s) => this.onSummary(s))
      .catch(() => undefined);
    return () => {
      offs.forEach((o) => o());
      if (this.timer !== null) clearTimeout(this.timer);
      this.timer = null;
    };
  }
}
