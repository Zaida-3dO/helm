// Typed wrappers over the plan-usage ops. Every call still goes through
// api(op, args).

import type { Api } from './client.ts';
import type { CostReport, RequestModes, UsageSnapshot } from './generated/usage/index.ts';

export type {
  ChatCost,
  CostReport,
  CostScope,
  CostTotals,
  LocalTrack,
  WeeklyShare,
  ModelUsage,
  PlanUsageReading,
  ReadinessState,
  RequestModes,
  UsageReadiness,
  UsageSnapshot,
  UsageSource,
  UsageWindow,
} from './generated/usage/index.ts';

/** One feature in the agent-feedback report (`feedback_report`). Notes are other agents' words. */
export interface FeedbackFeature {
  feature: string;
  count: number;
  mean: number;
  /** Counts for scores -2 .. 2, keyed by the score as text. */
  histogram: Record<'-2' | '-1' | '0' | '1' | '2', number>;
  kinds: string[];
  newestNotes: string[];
}

export interface FeedbackEntry {
  chatId: string;
  feature: string;
  score: number;
  kind: string;
  note: string;
  context: string | null;
  origin: string;
  at: number;
}

export function usageOps(api: Api) {
  return {
    get: () => api<UsageSnapshot>('usage_get', {}),
    refresh: () => api<UsageSnapshot>('usage_refresh', {}),
    modes: (since: number, until: number) => api<RequestModes>('request_modes', { since, until }),
    /** What one chat, or every chat in a project and its sub-projects, used. */
    cost: (of: { chatId: string } | { projectId: string }) => api<CostReport>('usage_cost', of),
    feedbackReport: (feature?: string, since?: string) =>
      api<{ features: FeedbackFeature[] }>('feedback_report', { ...(feature ? { feature } : {}), ...(since ? { since } : {}) }),
    feedbackExport: () => api<{ entries: FeedbackEntry[] }>('feedback_export', {}),
  };
}

export type UsageOps = ReturnType<typeof usageOps>;
