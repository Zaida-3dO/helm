// The gatekeeper's shapes as the UI sees them. The audit row and the brief come straight from
// the host's generated bindings; only the settings view stays here, since it is this UI's own
// pairing of the saved settings with what they imply.

import type { GateBrief } from '../api/generated/gatekeeper';

/** What the gate's write-up of an escalated question looks like (the same brief the card header
 *  and any remote copy carry). */
export type GateBriefView = GateBrief;

/** One audit row: a question the gate judged, in one of its verdicts. */
export type { GateAsk } from '../api/generated/gatekeeper';

/** The settings record for the current person, plus what it implies. */
export interface GateSettingsView {
  enabled: boolean;
  /** `less`, `balanced` or `more`. */
  askFrequency: string;
  /** The person's own policy words; null means the built-in ones. */
  policy: string | null;
  /** The built-in policy words, for the placeholder and the "restore" hint. */
  defaultPolicy: string;
  model: string;
  /** The confidence the gate needs before it decides, from the frequency pick. */
  bar: number;
  /** True while the gate is paused after repeated failures. */
  paused: boolean;
  pausedUntil: number | null;
}

/** The four settings the save accepts; a blank policy means the built-in words. */
export interface GateSettingsEdit {
  enabled: boolean;
  askFrequency: string;
  policy: string | null;
  model: string;
}

/** History filters, in tab order. */
export type GateHistoryFilter = 'all' | 'decided' | 'asked' | 'failed';
