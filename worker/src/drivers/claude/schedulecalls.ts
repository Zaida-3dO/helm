// Claude Code's own schedule tools (cron, wake-up, remote trigger), when the user lets them be
// offered. A call never runs: a timer or task file made by the CLI would live outside Helm, where
// the user cannot see, pause or cancel it. Every call is refused with directions to Helm's
// schedule tools, with the timing translated where it can be done safely. The prompt text of the
// call is never repeated in the refusal.

/** The callback id the worker registers for the schedule-tool PreToolUse hook. */
export const SCHEDULE_REDIRECT_HOOK_ID = "helm-native-schedules";
export const NATIVE_SCHEDULE_TOOLS = ["CronCreate", "CronDelete", "CronList", "ScheduleWakeup", "RemoteTrigger"] as const;
/** A hook matcher that selects exactly those tools. */
export const NATIVE_SCHEDULE_MATCHER = NATIVE_SCHEDULE_TOOLS.join("|");

const ASK = "Only schedule something when the user asked for it.";

function obj(v: unknown): Record<string, unknown> {
  return v && typeof v === "object" && !Array.isArray(v) ? (v as Record<string, unknown>) : {};
}

/** Day-of-week field to Helm's days (0 = Monday to 6 = Sunday); null when it is not plain numbers. */
function weekDays(field: string): number[] | null {
  const days = new Set<number>();
  for (const part of field.split(",")) {
    const m = /^(\d)(?:-(\d))?$/.exec(part);
    if (!m) return null;
    const from = Number(m[1]);
    const to = m[2] === undefined ? from : Number(m[2]);
    if (from > 7 || to > 7 || to < from) return null;
    for (let d = from; d <= to; d++) days.add((d + 6) % 7);
  }
  return [...days].sort((a, b) => a - b);
}

/** Helm timing for a cron rule `M H DoM Mon DoW` when one fits exactly; otherwise null. */
export function cronTiming(rule: string): Record<string, unknown> | null {
  const f = rule.trim().split(/\s+/);
  if (f.length !== 5) return null;
  const [min, hour, dom, mon, dow] = f as [string, string, string, string, string];
  if (dom !== "*" || mon !== "*") return null;
  const every = (s: string) => /^\*\/(\d{1,2})$/.exec(s)?.[1];
  const nMin = every(min);
  if (nMin && hour === "*" && dow === "*" && 60 % Number(nMin) === 0) return { kind: "interval", seconds: Number(nMin) * 60 };
  const nHour = every(hour);
  if (/^\d{1,2}$/.test(min) && Number(min) < 60 && nHour && dow === "*" && 24 % Number(nHour) === 0) return { kind: "interval", seconds: Number(nHour) * 3600 };
  if (/^\d{1,2}$/.test(min) && /^\d{1,2}$/.test(hour) && Number(min) < 60 && Number(hour) < 24) {
    const days = dow === "*" ? [0, 1, 2, 3, 4, 5, 6] : weekDays(dow);
    if (days) return { kind: "weekly", hour: Number(hour), minute: Number(min), days };
  }
  return null;
}

/** The refusal text for one native schedule call. */
export function scheduleRedirect(tool: string, input: unknown): string {
  const a = obj(input);
  const head = `Helm keeps schedules itself, so ${tool} does nothing here.`;
  switch (tool) {
    case "ScheduleWakeup": {
      const raw = Number(a.delaySeconds);
      const secs = Math.min(3600, Math.max(60, Number.isFinite(raw) ? Math.round(raw) : 60));
      return `${head} Call app_create_schedule with timing {"kind":"once","inSeconds":${secs}} and a short title and message for the wake-up. ${ASK}`;
    }
    case "CronCreate": {
      const timing = typeof a.cron === "string" ? cronTiming(a.cron) : null;
      const how = timing
        ? `Call app_create_schedule with timing ${JSON.stringify(timing)} (times are the user's local time).`
        : "Call app_create_schedule with a timing of kind once, interval (seconds, 60 or more) or weekly (hour, minute, days 0 = Monday to 6 = Sunday); this rule does not map onto one of them exactly, so pick the nearest and tell the user.";
      return `${head} ${how} ${ASK}`;
    }
    case "CronList":
      return `${head} Call app_list_schedules instead.`;
    case "CronDelete":
      return `${head} Call app_list_schedules to find the schedule, then app_cancel_schedule.`;
    default:
      return `${head} Remote triggers are not available in Helm; use app_create_schedule for something that should run later. ${ASK}`;
  }
}
