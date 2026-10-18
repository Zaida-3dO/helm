// Typed wrappers over the schedules ops (host/crates/schedules/src/ops.rs). Every call still goes
// through api(op, args).

import type { Api } from '../api/client.ts';
import type { RunList, Schedule, ScheduleList, Timing } from '../api/generated/schedules/index.ts';

export type { Definition, Run, RunList, RunStatus, Schedule, ScheduleList, ScheduleState, Template, Timing } from '../api/generated/schedules/index.ts';

export interface CreateArgs {
  requestId: string;
  title: string;
  message: string;
  timing: Timing;
  chatId: string;
  template?: string;
}

export interface UpdateArgs {
  scheduleId: string;
  revision: number;
  title?: string;
  message?: string;
  timing?: Timing;
  chatId?: string;
  paused?: boolean;
}

export function scheduleOps(api: Api) {
  return {
    list: (includeInactive = false) => api<ScheduleList>('schedule_list', { includeInactive }),
    runs: (scheduleId: string) => api<RunList>('schedule_runs', { scheduleId }),
    create: (a: CreateArgs) => api<Schedule>('schedule_create', a),
    update: (a: UpdateArgs) => api<Schedule>('schedule_update', a),
    cancel: (scheduleId: string, revision: number) => api<Schedule>('schedule_cancel', { scheduleId, revision }),
  };
}

export type ScheduleOps = ReturnType<typeof scheduleOps>;
