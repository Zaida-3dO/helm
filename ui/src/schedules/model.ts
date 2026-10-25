// Pure helpers for the Schedules pane: describing a timing in words, turning the editor's fields
// into a Timing (and back), and the relative "next run" text. Times are Unix milliseconds; weekdays
// count from Monday = 0, as the host does.

import type { Definition, Reminder, Schedule, Timing } from './api.ts';

export const DAY_LABELS = ['Mon', 'Tue', 'Wed', 'Thu', 'Fri', 'Sat', 'Sun'] as const;
export const MIN_INTERVAL_SECONDS = 60;
export const MAX_INTERVAL_SECONDS = 31_536_000;
export const MAX_TITLE = 160;
export const MAX_MESSAGE = 8000;

const pad = (n: number) => String(n).padStart(2, '0');

export function clock(hour: number, minute: number): string {
  return `${pad(hour)}:${pad(minute)}`;
}

function plural(n: number, one: string, many: string): string {
  return n === 1 ? one : `${n} ${many}`;
}

/** "Every 30 minutes", "Every 2 hours", "Every day". */
export function intervalText(seconds: number): string {
  if (seconds % 86_400 === 0) return seconds === 86_400 ? 'Every day' : `Every ${seconds / 86_400} days`;
  if (seconds % 3_600 === 0) return seconds === 3_600 ? 'Every hour' : `Every ${seconds / 3_600} hours`;
  if (seconds % 60 === 0) return seconds === 60 ? 'Every minute' : `Every ${seconds / 60} minutes`;
  return `Every ${plural(seconds, 'second', 'seconds')}`;
}

export function daysText(days: number[]): string {
  const sorted = [...days].sort((a, b) => a - b);
  if (sorted.length === 7) return 'every day';
  if (sorted.join() === '0,1,2,3,4') return 'weekdays';
  if (sorted.join() === '5,6') return 'weekends';
  return sorted.map((d) => DAY_LABELS[d] ?? '?').join(', ');
}

/** Local date and time, e.g. "Mon 19 Oct, 12:00". */
export function dateTimeText(ms: number): string {
  return new Intl.DateTimeFormat(undefined, { weekday: 'short', day: 'numeric', month: 'short', hour: '2-digit', minute: '2-digit' }).format(new Date(ms));
}

export function timingText(t: Timing): string {
  switch (t.kind) {
    case 'once':
      return `Once, ${dateTimeText(t.at)}`;
    case 'interval':
      return intervalText(t.seconds);
    case 'weekly':
      return `At ${clock(t.hour, t.minute)}, ${daysText(t.days)}`;
  }
}

/** "in 25 min", "in 2h 5m", "in 3d 4h", "now". */
export function relativeFuture(at: number, now: number): string {
  const s = Math.round((at - now) / 1000);
  if (s <= 0) return 'now';
  if (s < 90) return 'in under a minute';
  const m = Math.round(s / 60);
  if (m < 60) return `in ${m} min`;
  const h = Math.floor(m / 60);
  if (h < 24) return `in ${h}h ${m % 60}m`;
  return `in ${Math.floor(h / 24)}d ${h % 24}h`;
}

/** The status line of a schedule: when it next runs, or why it will not. */
export function statusText(s: Pick<Schedule, 'state' | 'nextRunAt'>, now: number): string {
  switch (s.state) {
    case 'paused':
      return 'Paused';
    case 'completed':
      return 'Finished';
    case 'cancelled':
      return 'Cancelled';
    case 'active':
      if (s.nextRunAt === null) return 'Waiting';
      return s.nextRunAt <= now ? 'Due, waiting for the chat to be free' : `Next run ${relativeFuture(s.nextRunAt, now)}`;
  }
}

/** Where a schedule's message goes, in words: "Reminder", "New chat in Helm" or the chat's title. */
export function destinationText(s: Pick<Schedule, 'chatTitle'> & { definition: Pick<Definition, 'kind' | 'destination' | 'chatId'> }): string {
  const { kind, destination, chatId } = s.definition;
  if (kind === 'reminder') return chatId ? `Reminder, opens ${s.chatTitle || 'a chat'}` : 'Reminder';
  if (destination === 'newChat') return `New chat in ${s.chatTitle || 'a project'}`;
  return s.chatTitle || 'Chat unavailable';
}

/** The reminders in `next` that were not in `seen`. The first read (`seen` is null) announces none. */
export function freshReminders(seen: ReadonlySet<string> | null, next: readonly Reminder[]): Reminder[] {
  if (!seen) return [];
  return next.filter((r) => !seen.has(r.id));
}

/** The toast line for a reminder: its name, then the first line of its message. */
export function reminderText(r: Pick<Reminder, 'title' | 'message'>): string {
  const first = r.message.trim().split('\n', 1)[0]?.trim() ?? '';
  const line = first.length > 120 ? `${first.slice(0, 119)}…` : first;
  return line && line !== r.title ? `Reminder: ${r.title} - ${line}` : `Reminder: ${r.title}`;
}

// The editor's fields.

export type Kind = 'once' | 'interval' | 'weekly';

export interface Draft {
  kind: Kind;
  /** `datetime-local` text: YYYY-MM-DDTHH:mm in local time. */
  onceAt: string;
  /** Minutes between runs; fractions are rounded to whole seconds. */
  everyMinutes: string;
  /** `time` text: HH:mm. */
  at: string;
  days: boolean[];
}

/** A `datetime-local` value for an instant, in the machine's local time. */
export function toLocalInput(ms: number): string {
  const d = new Date(ms);
  return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())}T${pad(d.getHours())}:${pad(d.getMinutes())}`;
}

/** Defaults when a kind is picked: one-time = an hour from now, every 30 minutes, 09:00 Monday to Friday. */
export function defaultDraft(now: number): Draft {
  return { kind: 'interval', onceAt: toLocalInput(now + 3_600_000), everyMinutes: '30', at: '09:00', days: [true, true, true, true, true, false, false] };
}

export function timingToDraft(t: Timing, now: number): Draft {
  const d = defaultDraft(now);
  switch (t.kind) {
    case 'once':
      return { ...d, kind: 'once', onceAt: toLocalInput(t.at) };
    case 'interval':
      return { ...d, kind: 'interval', everyMinutes: String(Math.round((t.seconds / 60) * 1000) / 1000) };
    case 'weekly':
      return { ...d, kind: 'weekly', at: clock(t.hour, t.minute), days: DAY_LABELS.map((_, i) => t.days.includes(i)) };
  }
}

export type Parsed<T> = { ok: true; value: T } | { ok: false; error: string };

export function draftToTiming(d: Draft, now: number): Parsed<Timing> {
  switch (d.kind) {
    case 'once': {
      const at = new Date(d.onceAt).getTime();
      if (!d.onceAt || Number.isNaN(at)) return { ok: false, error: 'Pick a date and time.' };
      if (at <= now) return { ok: false, error: 'Pick a time in the future.' };
      return { ok: true, value: { kind: 'once', at } };
    }
    case 'interval': {
      const minutes = Number(d.everyMinutes);
      if (!d.everyMinutes.trim() || !Number.isFinite(minutes)) return { ok: false, error: 'Enter how many minutes between runs.' };
      const seconds = Math.round(minutes * 60);
      if (seconds < MIN_INTERVAL_SECONDS || seconds > MAX_INTERVAL_SECONDS) return { ok: false, error: 'Repeat between every 1 minute and every 525,600 minutes.' };
      return { ok: true, value: { kind: 'interval', seconds } };
    }
    case 'weekly': {
      const m = /^(\d{1,2}):(\d{2})$/.exec(d.at);
      if (!m) return { ok: false, error: 'Pick a time of day.' };
      const hour = Number(m[1]);
      const minute = Number(m[2]);
      if (hour > 23 || minute > 59) return { ok: false, error: 'Pick a time of day.' };
      const days = d.days.flatMap((on, i) => (on ? [i] : []));
      if (days.length === 0) return { ok: false, error: 'Pick at least one weekday.' };
      return { ok: true, value: { kind: 'weekly', hour, minute, days } };
    }
  }
}

/** Whether two timings are the same (an update sends timing only when it changed). */
export function sameTiming(a: Timing, b: Timing): boolean {
  return JSON.stringify(a) === JSON.stringify(b);
}

/** The shortest repeat for a schedule that starts a new chat on every run, in seconds. */
export const MIN_NEW_CHAT_SECONDS = 3600;

/** Why a timing is too fast for a new chat on every run, or null. */
export function newChatPaceProblem(t: Timing): string | null {
  return t.kind === 'interval' && t.seconds < MIN_NEW_CHAT_SECONDS ? 'A new chat each time can repeat at most once an hour.' : null;
}

/** The first problem with a title or message, or null. */
export function textProblem(title: string, message: string): string | null {
  const t = title.trim();
  if (!t) return 'Give the schedule a name.';
  if (new TextEncoder().encode(t).length > MAX_TITLE) return 'The name is too long (160 characters at most).';
  if (!message.trim()) return 'Write the message to send.';
  if (new TextEncoder().encode(message).length > MAX_MESSAGE) return 'The message is too long (8000 bytes at most).';
  return null;
}
