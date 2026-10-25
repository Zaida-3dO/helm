// Typed wrappers over the schedules ops (host/crates/schedules/src/ops.rs). Every call still goes
// through api(op, args).

import type { Api } from '../api/client.ts';
import type { Destination, Reminder, ReminderList, RunList, Schedule, ScheduleKind, ScheduleList, Timing } from '../api/generated/schedules/index.ts';

export type { Definition, Destination, Reminder, ReminderList, Run, RunList, RunStatus, Schedule, ScheduleKind, ScheduleList, ScheduleState, Template, Timing } from '../api/generated/schedules/index.ts';

export interface CreateArgs {
  requestId: string;
  title: string;
  message: string;
  timing: Timing;
  kind?: ScheduleKind;
  destination?: Destination;
  /** The chat a prompt goes to, or a reminder opens; empty for a new-chat destination. */
  chatId?: string;
  /** The folder project a new-chat destination starts its chats in. */
  projectId?: string;
  template?: string;
}

export interface UpdateArgs {
  scheduleId: string;
  revision: number;
  title?: string;
  message?: string;
  timing?: Timing;
  kind?: ScheduleKind;
  destination?: Destination;
  chatId?: string;
  projectId?: string;
  paused?: boolean;
}

export function scheduleOps(api: Api) {
  return {
    list: (includeInactive = false) => api<ScheduleList>('schedule_list', { includeInactive }),
    runs: (scheduleId: string) => api<RunList>('schedule_runs', { scheduleId }),
    create: (a: CreateArgs) => api<Schedule>('schedule_create', a),
    update: (a: UpdateArgs) => api<Schedule>('schedule_update', a),
    cancel: (scheduleId: string, revision: number) => api<Schedule>('schedule_cancel', { scheduleId, revision }),
    reminders: () => api<ReminderList>('reminder_list', {}),
    /** One reminder, or every pending one when no id is given. */
    dismiss: (reminderId?: string) => api<{ dismissed: number }>('reminder_dismiss', reminderId ? { reminderId } : {}),
  };
}

export type ScheduleOps = ReturnType<typeof scheduleOps>;
