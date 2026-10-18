// The Schedules pane's store: holds the host's list and re-reads when the `schedules` event says
// something changed (a tick claimed a run, a schedule was paused, ...). Changes go through the
// host; the store never edits its copy by hand.

import type { Api } from '../api/client.ts';
import type { EventBus } from '../api/events.ts';
import { scheduleOps, type CreateArgs, type Schedule, type ScheduleList, type UpdateArgs } from './api.ts';

type Listener = () => void;

export class ScheduleStore {
  private data: ScheduleList | null = null;
  private failure: string | null = null;
  private readonly listeners = new Set<Listener>();
  private readonly ops: ReturnType<typeof scheduleOps>;
  private off: (() => void) | undefined;
  private started = false;
  /** Bumped per read so a slow older answer cannot overwrite a newer one. */
  private reads = 0;

  private readonly deps: { api: Api; bus: EventBus };

  constructor(deps: { api: Api; bus: EventBus }) {
    this.deps = deps;
    this.ops = scheduleOps(deps.api);
  }

  subscribe = (l: Listener): (() => void) => {
    this.listeners.add(l);
    return () => {
      this.listeners.delete(l);
    };
  };

  get list(): ScheduleList | null {
    return this.data;
  }

  get error(): string | null {
    return this.failure;
  }

  private emit(): void {
    for (const l of [...this.listeners]) l();
  }

  /** Read now, then follow the `schedules` event. Safe to call twice. */
  start(): void {
    if (this.started) return;
    this.started = true;
    this.off = this.deps.bus.on('schedules', () => void this.refresh());
    void this.refresh();
  }

  stop(): void {
    this.off?.();
    this.started = false;
  }

  async refresh(): Promise<void> {
    const n = ++this.reads;
    try {
      const list = await this.ops.list(true);
      if (n !== this.reads) return;
      this.data = list;
      this.failure = null;
    } catch (e) {
      if (n !== this.reads) return;
      this.failure = String((e as Error)?.message ?? e);
    }
    this.emit();
  }

  runs(scheduleId: string) {
    return this.ops.runs(scheduleId);
  }

  async create(a: CreateArgs): Promise<Schedule> {
    const s = await this.ops.create(a);
    await this.refresh();
    return s;
  }

  async update(a: UpdateArgs): Promise<Schedule> {
    const s = await this.ops.update(a);
    await this.refresh();
    return s;
  }

  async cancel(s: Pick<Schedule, 'id' | 'revision'>): Promise<Schedule> {
    const out = await this.ops.cancel(s.id, s.revision);
    await this.refresh();
    return out;
  }
}

const stores = new WeakMap<object, ScheduleStore>();

/** One store per event bus (the app's). */
export function scheduleStoreFor(api: Api, bus: EventBus): ScheduleStore {
  let s = stores.get(bus);
  if (!s) {
    s = new ScheduleStore({ api, bus });
    stores.set(bus, s);
  }
  return s;
}
