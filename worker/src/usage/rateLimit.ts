// The passive plan-usage reader: a `rate_limit_event` line from the claude binary becomes one
// `plan_usage` line to the host. Helm reads nothing but what the binary already printed in this
// chat: no token store, no endpoint.
//
// The binary names the window in `rateLimitType` (`five_hour`, `seven_day`, `seven_day_opus`,
// `seven_day_sonnet`, `overage`), gives the reset time in epoch seconds and includes
// `utilization` only near a threshold (host/crates/usage/NOTES.md). Utilization is read as a
// fraction when it is 1 or less and as a percent otherwise.

export interface PlanUsageWindow {
  kind: string;
  /** Percent used, 0 to 100, or null when the event carried no number. */
  pct: number | null;
  /** Epoch milliseconds, or null. */
  resetsAt: number | null;
}

export interface PlanUsageLine {
  type: "plan_usage";
  source: "rate_limit_event";
  windows: PlanUsageWindow[];
  at: number;
}

type Json = Record<string, unknown>;

const WINDOWS = new Set(["five_hour", "seven_day", "seven_day_opus", "seven_day_sonnet"]);

function obj(v: unknown): Json {
  return v && typeof v === "object" && !Array.isArray(v) ? (v as Json) : {};
}

/** The `plan_usage` line for one `rate_limit_event` record, or null when it says nothing usable. */
export function planUsageFromRateLimit(rec: Json, now: number): PlanUsageLine | null {
  const info = obj(rec.rate_limit_info ?? rec);
  const kind = typeof info.rateLimitType === "string" ? info.rateLimitType : typeof info.type === "string" ? info.type : "";
  if (!WINDOWS.has(kind)) return null;
  const u = info.utilization;
  let pct: number | null = null;
  if (typeof u === "number" && Number.isFinite(u) && u >= 0) pct = Math.min(100, u <= 1 ? u * 100 : u);
  const r = info.resetsAt;
  const resetsAt = typeof r === "number" && Number.isFinite(r) && r > 0 ? Math.round(r < 1e11 ? r * 1000 : r) : null;
  if (pct === null && resetsAt === null) return null;
  return { type: "plan_usage", source: "rate_limit_event", windows: [{ kind, pct, resetsAt }], at: now };
}
