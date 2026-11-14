// The gatekeeper's shapes as the UI sees them. They mirror the host's audit row and settings
// view; when the host crate's generated bindings land in `api/generated/gatekeeper/`, this file
// re-exports them instead of defining its own.

/** What the gate's write-up of an escalated question looks like (the same brief the card header
 *  and any remote copy carry). */
export interface GateBriefView {
  problem: string;
  options: Array<{ label: string; means: string; pros: string; cons: string; cost: string }>;
  recommendation: string;
  why: string;
}

/** One audit row: a question the gate judged, in one of its verdicts. */
export interface GateAsk {
  id: string;
  profileId: string;
  chatId: string;
  chatTitle: string;
  /** `question`, `plan`, `flag`, `telegram_ask` or `board_pick`. */
  channel: string;
  requestRef: string;
  askedAt: number;
  /** The question as asked, in one line. */
  question: string;
  /** `checking`, `decided`, `escalated`, `failed` or `passed`. */
  verdict: string;
  passReason?: string;
  /** `local`, `claude` or `rule`. */
  decidedBy?: string;
  model?: string;
  confidence?: number;
  /** `low`, `medium` or `high`. */
  risk?: string;
  reason?: string;
  choice?: string;
  brief?: GateBriefView;
  tookMs?: number | null;
  /** `open`, `answered`, `parked`, `withdrawn`, `kept`, `vetoed` or `expired`. */
  state: string;
  answer?: string;
  /** `helm`, `telegram`, `board` or `chat`. */
  answeredVia?: string;
  answeredAt?: number | null;
  /** `pending`, `fine`, `should_have_asked` or `didnt_need_me`. */
  review: string;
  reviewNote?: string;
}

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
