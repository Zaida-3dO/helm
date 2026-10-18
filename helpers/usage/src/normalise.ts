// Turns the binary's `get_usage` answer into Helm's reading. Only the fields
// named here are read; anything else in the answer is ignored.

export interface PlanWindow {
  /** Percent used, 0 to 100 (fractions allowed); null when the binary gave no number. */
  pct: number | null;
  /** Epoch milliseconds; null when unknown. */
  resetsAt: number | null;
}

export interface ModelWindow extends PlanWindow {
  model: string;
}

export interface ControlReading {
  plan: string | null;
  fiveHour: PlanWindow | null;
  weekly: PlanWindow | null;
  perModel: ModelWindow[];
}

type Json = Record<string, unknown>;

function obj(v: unknown): Json {
  return v && typeof v === "object" && !Array.isArray(v) ? (v as Json) : {};
}

function num(v: unknown): number | null {
  return typeof v === "number" && Number.isFinite(v) ? v : null;
}

/** An ISO string, or epoch seconds or milliseconds, as epoch milliseconds. */
export function toMillis(v: unknown): number | null {
  if (typeof v === "string") {
    const t = Date.parse(v);
    return Number.isNaN(t) ? null : t;
  }
  const n = num(v);
  if (n === null) return null;
  return n < 1e11 ? Math.round(n * 1000) : Math.round(n);
}

function window(v: unknown): PlanWindow | null {
  const w = obj(v);
  const pct = num(w.utilization);
  const resetsAt = toMillis(w.resets_at);
  if (pct === null && resetsAt === null) return null;
  return { pct, resetsAt };
}

/** The reading for a `get_usage` response body, or null when the binary reports no plan limits. */
export function normaliseControl(raw: unknown): ControlReading | null {
  const r = obj(raw);
  const rl = obj(r.rate_limits);
  if (r.rate_limits_available === false || Object.keys(rl).length === 0) return null;
  const perModel: ModelWindow[] = [];
  const scoped = Array.isArray(rl.model_scoped) ? rl.model_scoped : [];
  for (const m of scoped) {
    const mo = obj(m);
    const w = window(mo);
    if (w && typeof mo.display_name === "string") perModel.push({ model: mo.display_name, ...w });
  }
  for (const [key, name] of [
    ["seven_day_opus", "Opus"],
    ["seven_day_sonnet", "Sonnet"],
  ] as const) {
    const w = window(rl[key]);
    if (w && !perModel.some((x) => x.model === name)) perModel.push({ model: name, ...w });
  }
  const reading: ControlReading = {
    plan: typeof r.subscription_type === "string" ? r.subscription_type : null,
    fiveHour: window(rl.five_hour),
    weekly: window(rl.seven_day),
    perModel,
  };
  if (!reading.fiveHour && !reading.weekly && perModel.length === 0) return null;
  return reading;
}
