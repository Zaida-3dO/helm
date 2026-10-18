// Typed wrappers and pure helpers for the usage governor (host crate helm-governor).

import type { Api } from '../api/client.ts';

export type Band = 'unknown' | 'surplus' | 'selective' | 'windDown' | 'stop';

export interface GovernorSettings {
  enabled: boolean;
  selectivePct: number;
  windDownPct: number;
  stopPct: number;
  atStop: 'stop' | 'wrapUp';
}

export interface GovernorView {
  settings: GovernorSettings;
  defaults: GovernorSettings;
  band: Band;
  usedPct: number | null;
  since: number | null;
  told: number;
  stopped: number;
}

export function governorOps(api: Api) {
  return {
    state: () => api<GovernorView>('governor_state', {}),
    save: (settings: GovernorSettings) => api<GovernorView>('governor_settings', { settings }),
  };
}

export function bandLabel(b: Band): string {
  switch (b) {
    case 'unknown':
      return 'no fresh usage reading';
    case 'surplus':
      return 'plenty left';
    case 'selective':
      return 'selective';
    case 'windDown':
      return 'winding down';
    case 'stop':
      return 'at the limit';
  }
}

/** The host's ordering rule, checked before Save. */
export function thresholdsError(s: GovernorSettings): string | null {
  if (!(s.selectivePct < s.windDownPct && s.windDownPct < s.stopPct)) {
    return 'The thresholds must rise: selective, then wind down, then stop.';
  }
  return null;
}
